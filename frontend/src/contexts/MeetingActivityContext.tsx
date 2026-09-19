'use client';

import React, { createContext, useCallback, useContext, useMemo, useSyncExternalStore } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { meetingActivityService, type MeetingActivityService } from '@/services/meetingActivityService';
import type {
  MeetingActivity,
  MeetingActivitySnapshot,
  RecordingActivity,
} from '@/types/meetingActivity';
import type { CancelSummaryResponse, ProcessTranscriptResponse, SummaryProcessResponse } from '@/types';

export type ActivityHydrationStatus = 'idle' | 'loading' | 'ready' | 'error';
export type SummaryActivityStatus =
  | 'queued'
  | 'processing'
  | 'completed'
  | 'failed'
  | 'cancelled';

export interface SummaryActivity {
  activityId: string;
  revision: number;
  meetingId: string;
  processId: string | null;
  status: SummaryActivityStatus;
  response: SummaryProcessResponse | null;
  error: string | null;
  reconciliationError: string | null;
}

export interface SummaryStartRequest {
  meetingId: string;
  text: string;
  model: string;
  modelName: string;
  chunkSize: number;
  overlap: number;
  customPrompt: string;
  templateId: string;
  summaryLanguage: string | null;
  replaceExisting?: boolean;
}

export interface SummaryStartResult {
  started: boolean;
  processId: string | null;
  response: SummaryProcessResponse | null;
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
}

interface MeetingActivityStoreDependencies {
  service?: Pick<MeetingActivityService, 'subscribe'>;
  readSummary?: (meetingId: string) => Promise<SummaryProcessResponse>;
  startSummary?: (request: SummaryStartRequest) => Promise<ProcessTranscriptResponse>;
  cancelSummary?: (meetingId: string, processId: string) => Promise<CancelSummaryResponse>;
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
  private readonly invokeSummaryStart: (request: SummaryStartRequest) => Promise<ProcessTranscriptResponse>;
  private readonly invokeSummaryCancel: (meetingId: string, processId: string) => Promise<CancelSummaryResponse>;
  private readonly pollIntervalMs: number;
  private readonly setIntervalFn: (callback: () => void, timeout: number) => unknown;
  private readonly clearIntervalFn: (timer: unknown) => void;
  private summaryPolls = new Map<string, SummaryPoll>();
  private summaryStarts = new Map<string, Promise<SummaryStartResult>>();
  private summaryRequests = new Map<string, SummaryStartRequest>();
  private summaryRetries = new Map<string, () => Promise<SummaryStartResult>>();
  private nextSummaryAttempt = 0;
  private summaryRevision = 0;
  private nativeUnlisten: (() => void) | null = null;
  private subscriptionGeneration = 0;

  constructor(dependencies: MeetingActivityStoreDependencies = {}) {
    this.service = dependencies.service ?? meetingActivityService;
    this.readSummary = dependencies.readSummary
      ?? ((meetingId) => invoke<SummaryProcessResponse>('api_get_summary', { meetingId }));
    this.invokeSummaryStart = dependencies.startSummary ?? (async (request) => {
      const { replaceExisting: _replaceExisting, ...args } = request;
      return invoke<ProcessTranscriptResponse>('api_process_transcript', args);
    });
    this.invokeSummaryCancel = dependencies.cancelSummary
      ?? ((meetingId, processId) => invoke<CancelSummaryResponse>('api_cancel_summary', { meetingId, processId }));
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
    const retained = this.state.summaries.find((summary) => (
      summary.meetingId === meetingId && summary.processId === processId && isSummaryTerminal(summary.status)
    ));
    if (retained?.response) {
      if (listener) void Promise.resolve(listener(retained.response)).catch((error: unknown) => {
        console.error('Failed to handle retained summary update:', error);
      });
      return () => {};
    }
    let poll = this.summaryPolls.get(key);
    if (!poll) {
      poll = {
        key,
        meetingId,
        processId,
        listeners: new Set(),
        inFlight: false,
        timer: undefined,
      };
      poll.timer = this.setIntervalFn(() => void this.pollSummary(poll!), this.pollIntervalMs);
      this.summaryPolls.set(key, poll);
      this.setSummary({
        activityId: key, revision: 0, meetingId, processId, status: 'queued', response: null,
        error: null, reconciliationError: null,
      });
      void this.pollSummary(poll);
    }
    if (listener) poll.listeners.add(listener);
    return () => listener && poll?.listeners.delete(listener);
  };

  hydrateSummary = (response: SummaryProcessResponse): void => {
    if (response.status === 'idle' || !response.start) return;
    const status = summaryStatus(response);
    if (!status) return;
    this.setSummary({
      activityId: this.summaryKey(response.meeting_id, response.start), revision: 0,
      meetingId: response.meeting_id,
      processId: response.start,
      status,
      response,
      error: status === 'failed' ? response.error ?? 'Summary generation failed.' : null,
      reconciliationError: null,
    });
    if (!isSummaryTerminal(status)) this.startSummaryPolling(response.meeting_id, response.start);
  };

  startSummary = (request: SummaryStartRequest): Promise<SummaryStartResult> => {
    this.summaryRequests.set(request.meetingId, request);
    const retry = () => this.withSummaryStartLock(
      request.meetingId,
      () => this.startSummaryLocked(request),
    );
    this.summaryRetries.set(request.meetingId, retry);
    return retry();
  };

  prepareAndStartSummary = (
    meetingId: string,
    prepare: () => Promise<SummaryStartRequest>,
  ): Promise<SummaryStartResult> => {
    const retry = () => this.withSummaryStartLock(meetingId, async () => {
      let request: SummaryStartRequest;
      try {
        request = await prepare();
      } catch (error) {
        this.retainSummaryStartFailure(meetingId, error);
        throw error;
      }
      this.summaryRequests.set(meetingId, request);
      return this.startSummaryLocked(request);
    });
    this.summaryRetries.set(meetingId, retry);
    return retry();
  };

  private withSummaryStartLock(
    meetingId: string,
    startOperation: () => Promise<SummaryStartResult>,
  ): Promise<SummaryStartResult> {
    const existingStart = this.summaryStarts.get(meetingId);
    if (existingStart) return existingStart;
    const start = startOperation().finally(() => {
      if (this.summaryStarts.get(meetingId) === start) {
        this.summaryStarts.delete(meetingId);
      }
    });
    this.summaryStarts.set(meetingId, start);
    return start;
  }

  retrySummary = async (meetingId: string): Promise<SummaryStartResult> => {
    const retry = this.summaryRetries.get(meetingId);
    if (retry) return retry();
    const request = this.summaryRequests.get(meetingId);
    if (request) return this.startSummary(request);
    throw new Error('No summary attempt is available to retry.');
  };

  cancelSummary = async (meetingId: string, processId: string): Promise<boolean> => {
    const result = await this.invokeSummaryCancel(meetingId, processId);
    if (!result.cancelled) {
      const poll = this.summaryPolls.get(this.summaryKey(meetingId, processId));
      if (poll) void this.pollSummary(poll);
      return false;
    }

    const poll = this.summaryPolls.get(this.summaryKey(meetingId, processId));
    let response: SummaryProcessResponse;
    try {
      response = await this.readSummary(meetingId);
      if (response.meeting_id !== meetingId || (response.start && response.start !== processId)) {
        throw new Error('Stored summary belongs to another process.');
      }
    } catch {
      const retained = this.summaryForProcess(meetingId, processId)?.response;
      response = {
        status: 'cancelled', meetingName: retained?.meetingName ?? null,
        meeting_id: meetingId, start: processId, end: null,
        data: retained?.data ?? null, error: null,
      };
    }
    if (poll) await this.publishSummary(poll, { ...response, status: 'cancelled', start: processId });
    else {
      this.setSummary({
        activityId: this.summaryKey(meetingId, processId), revision: 0, meetingId, processId,
        status: 'cancelled', response: { ...response, status: 'cancelled', start: processId },
        error: null, reconciliationError: null,
      });
    }
    return true;
  };

  dismissSummary = (meetingId: string, processId?: string): void => {
    this.update({
      summaries: this.state.summaries.filter((summary) => (
        summary.meetingId !== meetingId
        || (processId !== undefined && summary.processId !== processId)
        || !isSummaryTerminal(summary.status)
      )),
    });
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
      if (response.status === 'idle') return;
      await this.publishSummary(poll, response);
    } catch (error) {
      const current = this.summaryForProcess(poll.meetingId, poll.processId);
      if (current) this.setSummary({ ...current, reconciliationError: errorText(error) });
    } finally {
      poll.inFlight = false;
    }
  }

  private async publishSummary(poll: SummaryPoll, response: SummaryProcessResponse): Promise<void> {
    const status = summaryStatus(response);
    if (!status || this.summaryPolls.get(poll.key) !== poll) return;
    this.setSummary({
      activityId: poll.key, revision: 0,
      meetingId: poll.meetingId,
      processId: poll.processId,
      status,
      response,
      error: status === 'failed' ? response.error ?? 'Summary generation failed.' : null,
      reconciliationError: null,
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
      candidate.activityId !== summary.activityId
    ));
    summaries.push({ ...summary, revision: ++this.summaryRevision });
    this.update({ summaries });
  }

  private summaryKey(meetingId: string, processId: string): string {
    return `${meetingId}\u0000${processId}`;
  }

  private summaryForProcess(meetingId: string, processId: string): SummaryActivity | undefined {
    return this.state.summaries.find((summary) => (
      summary.meetingId === meetingId && summary.processId === processId
    ));
  }

  private async startSummaryLocked(request: SummaryStartRequest): Promise<SummaryStartResult> {
    this.summaryRequests.set(request.meetingId, request);
    let stored: SummaryProcessResponse;
    try {
      stored = await this.readSummary(request.meetingId);
    } catch (error) {
      this.retainSummaryStartFailure(request.meetingId, error);
      throw error;
    }
    this.hydrateSummary(stored);
    if (stored.start && (stored.status === 'pending' || stored.status === 'processing')) {
      return { started: false, processId: stored.start, response: stored };
    }
    if (stored.status === 'completed' && !request.replaceExisting) {
      return { started: false, processId: stored.start, response: stored };
    }

    const attemptId = `summary-start:${request.meetingId}:${++this.nextSummaryAttempt}`;
    this.setSummary({
      activityId: attemptId, revision: 0, meetingId: request.meetingId, processId: null,
      status: 'queued', response: null, error: null, reconciliationError: null,
    });
    try {
      const result = await this.invokeSummaryStart(request);
      this.removeSummary(attemptId);
      this.startSummaryPolling(request.meetingId, result.process_id);
      return { started: true, processId: result.process_id, response: null };
    } catch (error) {
      this.setSummary({
        activityId: attemptId, revision: 0, meetingId: request.meetingId, processId: null,
        status: 'failed', response: null, error: errorText(error), reconciliationError: null,
      });
      throw error;
    }
  }

  private retainSummaryStartFailure(meetingId: string, error: unknown): void {
    this.setSummary({
      activityId: `summary-start:${meetingId}:${++this.nextSummaryAttempt}`, revision: 0,
      meetingId,
      processId: null,
      status: 'failed',
      response: null,
      error: errorText(error),
      reconciliationError: null,
    });
  }

  private removeSummary(activityId: string): void {
    this.update({ summaries: this.state.summaries.filter((summary) => summary.activityId !== activityId) });
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
  hydrateSummary: (response: SummaryProcessResponse) => void;
  startSummary: (request: SummaryStartRequest) => Promise<SummaryStartResult>;
  prepareAndStartSummary: (
    meetingId: string,
    prepare: () => Promise<SummaryStartRequest>,
  ) => Promise<SummaryStartResult>;
  retrySummary: (meetingId: string) => Promise<SummaryStartResult>;
  cancelSummary: (meetingId: string, processId: string) => Promise<boolean>;
  dismissSummary: (meetingId: string, processId?: string) => void;
  startSummaryPolling: MeetingActivityStore['startSummaryPolling'];
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
    return matching.sort((left, right) => right.revision - left.revision)[0] ?? null;
  }, [state.summaries]);

  return useMemo(() => ({
    ...state,
    recording: state.snapshot.recording,
    activeMeetingId: state.snapshot.recording?.meeting_id ?? null,
    getMeetingActivities,
    getTaskActivity,
    getSummaryActivity,
    hydrateSummary: store.hydrateSummary,
    startSummary: store.startSummary,
    prepareAndStartSummary: store.prepareAndStartSummary,
    retrySummary: store.retrySummary,
    cancelSummary: store.cancelSummary,
    dismissSummary: store.dismissSummary,
    startSummaryPolling: store.startSummaryPolling,
    cancelTranscription: (taskId: string) => invoke<boolean>('cancel_transcription_task', { taskId }),
    pauseTranscription: (taskId: string) => invoke<boolean>('pause_transcription_task', { taskId }),
    resumeTranscription: (taskId: string) => invoke<boolean>('resume_transcription_task', { taskId }),
    rehydrate: store.rehydrate,
  }), [getMeetingActivities, getSummaryActivity, getTaskActivity, state, store]);
}
