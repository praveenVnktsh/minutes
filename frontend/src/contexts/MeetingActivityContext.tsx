'use client';

import React, { createContext, useCallback, useContext, useMemo, useSyncExternalStore } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { meetingActivityService, type MeetingActivityService } from '@/services/meetingActivityService';
import type {
  MeetingActivity,
  MeetingActivitySnapshot,
  RecordingActivity,
} from '@/types/meetingActivity';
import type { SummaryProcessResponse } from '@/types';

export type ActivityHydrationStatus = 'idle' | 'loading' | 'ready' | 'error';
export type SummaryActivityStatus =
  | 'queued'
  | 'processing'
  | 'completed'
  | 'failed'
  | 'cancelled';

export interface SummaryActivity {
  meetingId: string;
  processId: string;
  status: SummaryActivityStatus;
  response: SummaryProcessResponse | null;
  error: string | null;
}

interface MeetingActivityState {
  hydrationStatus: ActivityHydrationStatus;
  hydrationError: string | null;
  snapshot: MeetingActivitySnapshot;
  summaries: SummaryActivity[];
}

type SummaryListener = (response: SummaryProcessResponse) => void | Promise<void>;

interface SummaryPoll {
  key: string;
  meetingId: string;
  processId: string;
  timer: unknown;
  listeners: Set<SummaryListener>;
  inFlight: boolean;
  reads: number;
}

interface MeetingActivityStoreDependencies {
  service?: Pick<MeetingActivityService, 'subscribe'>;
  readSummary?: (meetingId: string) => Promise<SummaryProcessResponse>;
  pollIntervalMs?: number;
  setIntervalFn?: (callback: () => void, timeout: number) => unknown;
  clearIntervalFn?: (timer: unknown) => void;
}

const EMPTY_SNAPSHOT: MeetingActivitySnapshot = {
  revision: -1,
  recording: null,
  activities: [],
};

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function summaryStatus(response: SummaryProcessResponse): SummaryActivityStatus | null {
  switch (response.status) {
    case 'pending': return 'queued';
    case 'processing': return 'processing';
    case 'completed': return 'completed';
    case 'failed':
    case 'error': return 'failed';
    case 'cancelled': return 'cancelled';
    default: return null;
  }
}

function isSummaryTerminal(status: SummaryActivityStatus): boolean {
  return status === 'completed' || status === 'failed' || status === 'cancelled';
}

export class MeetingActivityStore {
  private state: MeetingActivityState = {
    hydrationStatus: 'idle',
    hydrationError: null,
    snapshot: EMPTY_SNAPSHOT,
    summaries: [],
  };
  private readonly listeners = new Set<() => void>();
  private readonly service: Pick<MeetingActivityService, 'subscribe'>;
  private readonly readSummary: (meetingId: string) => Promise<SummaryProcessResponse>;
  private readonly pollIntervalMs: number;
  private readonly setIntervalFn: (callback: () => void, timeout: number) => unknown;
  private readonly clearIntervalFn: (timer: unknown) => void;
  private summaryPolls = new Map<string, SummaryPoll>();
  private nativeUnlisten: (() => void) | null = null;
  private subscriptionGeneration = 0;

  constructor(dependencies: MeetingActivityStoreDependencies = {}) {
    this.service = dependencies.service ?? meetingActivityService;
    this.readSummary = dependencies.readSummary
      ?? ((meetingId) => invoke<SummaryProcessResponse>('api_get_summary', { meetingId }));
    this.pollIntervalMs = dependencies.pollIntervalMs ?? 5000;
    this.setIntervalFn = dependencies.setIntervalFn
      ?? ((handler, timeout) => globalThis.setInterval(handler, timeout));
    this.clearIntervalFn = dependencies.clearIntervalFn
      ?? ((timer) => globalThis.clearInterval(timer as never));
  }

  getState = (): MeetingActivityState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    if (this.listeners.size === 1) void this.connect();
    return () => {
      this.listeners.delete(listener);
      if (this.listeners.size === 0) this.disconnect();
    };
  };

  rehydrate = async (): Promise<void> => {
    this.disconnect();
    await this.connect();
  };

  startSummaryPolling = (
    meetingId: string,
    processId: string,
    listener?: SummaryListener,
  ): (() => void) => {
    const key = this.summaryKey(meetingId, processId);
    let poll = this.summaryPolls.get(key);
    if (!poll) {
      poll = {
        key,
        meetingId,
        processId,
        listeners: new Set(),
        inFlight: false,
        reads: 0,
        timer: undefined,
      };
      poll.timer = this.setIntervalFn(() => void this.pollSummary(poll!), this.pollIntervalMs);
      this.summaryPolls.set(key, poll);
      this.setSummary({ meetingId, processId, status: 'queued', response: null, error: null });
      void this.pollSummary(poll);
    }
    if (listener) poll.listeners.add(listener);
    return () => listener && poll?.listeners.delete(listener);
  };

  stopSummaryPolling = (meetingId: string, processId?: string): void => {
    for (const [key, poll] of this.summaryPolls) {
      if (poll.meetingId !== meetingId || (processId && poll.processId !== processId)) continue;
      this.clearIntervalFn(poll.timer);
      this.summaryPolls.delete(key);
    }
  };

  private async connect(): Promise<void> {
    if (this.nativeUnlisten || this.state.hydrationStatus === 'loading') return;
    const generation = ++this.subscriptionGeneration;
    this.update({ hydrationStatus: 'loading', hydrationError: null });
    try {
      const unlisten = await this.service.subscribe((snapshot) => {
        if (generation !== this.subscriptionGeneration) return;
        if (snapshot.revision <= this.state.snapshot.revision) return;
        this.update({ snapshot, hydrationStatus: 'ready', hydrationError: null });
      });
      if (generation !== this.subscriptionGeneration || this.listeners.size === 0) {
        unlisten();
        return;
      }
      this.nativeUnlisten = unlisten;
      this.update({ hydrationStatus: 'ready', hydrationError: null });
    } catch (error) {
      if (generation === this.subscriptionGeneration) {
        this.update({ hydrationStatus: 'error', hydrationError: errorText(error) });
      }
    }
  }

  private disconnect(): void {
    this.subscriptionGeneration += 1;
    this.nativeUnlisten?.();
    this.nativeUnlisten = null;
    if (this.state.hydrationStatus === 'loading') this.update({ hydrationStatus: 'idle' });
  }

  private async pollSummary(poll: SummaryPoll): Promise<void> {
    if (this.summaryPolls.get(poll.key) !== poll || poll.inFlight) return;
    poll.inFlight = true;
    try {
      poll.reads += 1;
      if (poll.reads > 180) {
        await this.publishSummary(poll, {
          status: 'error', meetingName: null, meeting_id: poll.meetingId,
          start: poll.processId, end: null, data: null,
          error: 'Summary generation timed out after 15 minutes. Please try again or check your model configuration.',
        });
        return;
      }
      const response = await this.readSummary(poll.meetingId);
      if (this.summaryPolls.get(poll.key) !== poll) return;
      if (response.meeting_id !== poll.meetingId) return;
      if (response.start && response.start !== poll.processId) {
        // A newly-started process can briefly observe the previous stored
        // terminal response. Only a different live process proves this one was
        // superseded; otherwise wait for storage to catch up.
        if (response.status === 'pending' || response.status === 'processing') {
          await this.publishSummary(poll, {
            ...response,
            status: 'cancelled',
            meeting_id: poll.meetingId,
            start: poll.processId,
            error: 'This summary attempt was superseded by a newer process.',
          });
        }
        return;
      }
      if (response.status === 'idle' && poll.reads === 1) return;
      if (response.status === 'idle') {
        await this.publishSummary(poll, {
          ...response,
          status: 'cancelled',
          meeting_id: poll.meetingId,
          start: poll.processId,
        });
        return;
      }
      await this.publishSummary(poll, response);
    } catch (error) {
      await this.publishSummary(poll, {
        status: 'error', meetingName: null, meeting_id: poll.meetingId,
        start: poll.processId, end: null, data: null, error: errorText(error),
      });
    } finally {
      poll.inFlight = false;
    }
  }

  private async publishSummary(poll: SummaryPoll, response: SummaryProcessResponse): Promise<void> {
    const status = summaryStatus(response);
    if (!status || this.summaryPolls.get(poll.key) !== poll) return;
    this.setSummary({
      meetingId: poll.meetingId,
      processId: poll.processId,
      status,
      response,
      error: status === 'failed' ? response.error ?? 'Summary generation failed.' : null,
    });
    for (const listener of [...poll.listeners]) {
      try {
        await listener(response);
      } catch (error) {
        console.error('Failed to handle summary update:', error);
      }
    }
    if (isSummaryTerminal(status)) this.stopSummaryPolling(poll.meetingId, poll.processId);
  }

  private setSummary(summary: SummaryActivity): void {
    const summaries = this.state.summaries.filter((candidate) => (
      candidate.meetingId !== summary.meetingId || candidate.processId !== summary.processId
    ));
    summaries.push(summary);
    this.update({ summaries });
  }

  private summaryKey(meetingId: string, processId: string): string {
    return `${meetingId}\u0000${processId}`;
  }

  private update(patch: Partial<MeetingActivityState>): void {
    this.state = { ...this.state, ...patch };
    this.listeners.forEach((listener) => listener());
  }
}

export const meetingActivityStore = new MeetingActivityStore();

export interface MeetingActivityContextValue extends MeetingActivityState {
  recording: RecordingActivity | null;
  activeMeetingId: string | null;
  getMeetingActivities: (meetingId: string) => MeetingActivity[];
  getTaskActivity: (taskId: string) => MeetingActivity | null;
  getSummaryActivity: (meetingId: string, processId?: string) => SummaryActivity | null;
  startSummaryPolling: MeetingActivityStore['startSummaryPolling'];
  stopSummaryPolling: MeetingActivityStore['stopSummaryPolling'];
  cancelTranscription: (taskId: string) => Promise<boolean>;
  pauseTranscription: (taskId: string) => Promise<boolean>;
  resumeTranscription: (taskId: string) => Promise<boolean>;
  rehydrate: () => Promise<void>;
}

const MeetingActivityContext = createContext<MeetingActivityStore>(meetingActivityStore);

export function MeetingActivityProvider({ children }: { children: React.ReactNode }) {
  useSyncExternalStore(meetingActivityStore.subscribe, meetingActivityStore.getState, meetingActivityStore.getState);
  return (
    <MeetingActivityContext.Provider value={meetingActivityStore}>
      {children}
    </MeetingActivityContext.Provider>
  );
}

export function useMeetingActivity(): MeetingActivityContextValue {
  const store = useContext(MeetingActivityContext);
  const state = useSyncExternalStore(store.subscribe, store.getState, store.getState);
  const activities = state.snapshot.activities;

  const getMeetingActivities = useCallback((meetingId: string) => (
    activities.filter((activity) => activity.meeting_id === meetingId)
  ), [activities]);
  const getTaskActivity = useCallback((taskId: string) => (
    activities.find((activity) => activity.task_id === taskId) ?? null
  ), [activities]);
  const getSummaryActivity = useCallback((meetingId: string, processId?: string) => {
    const matching = state.summaries.filter((summary) => (
      summary.meetingId === meetingId && (!processId || summary.processId === processId)
    ));
    return matching.at(-1) ?? null;
  }, [state.summaries]);

  return useMemo(() => ({
    ...state,
    recording: state.snapshot.recording,
    activeMeetingId: state.snapshot.recording?.meeting_id ?? null,
    getMeetingActivities,
    getTaskActivity,
    getSummaryActivity,
    startSummaryPolling: store.startSummaryPolling,
    stopSummaryPolling: store.stopSummaryPolling,
    cancelTranscription: (taskId: string) => invoke<boolean>('cancel_transcription_task', { taskId }),
    pauseTranscription: (taskId: string) => invoke<boolean>('pause_transcription_task', { taskId }),
    resumeTranscription: (taskId: string) => invoke<boolean>('resume_transcription_task', { taskId }),
    rehydrate: store.rehydrate,
  }), [getMeetingActivities, getSummaryActivity, getTaskActivity, state, store]);
}
