'use client';

/*
 * The app's real recording workspace (as meeting-details renders it while
 * recording), driven by a replay: the live components read the ReplaySource
 * through the live-source context, and the recording, transcript, controller
 * and config contexts are replaced for this subtree only, so nothing here can
 * reach the recorder, the transcription engines or the meetings database.
 */

import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { toast } from 'sonner';
import { MeetingWorkspace } from '@/components/MeetingDetails/MeetingWorkspace';
import { LiveTranscriptPanel } from '@/components/MeetingDetails/LiveTranscriptPanel';
import { CopilotPanel } from '@/components/MeetingDetails/CopilotPanel';
import { FloatingRecordingControls } from '@/components/MeetingDetails/FloatingRecordingControls';
import { LiveSourceProvider } from '@/lib/liveSource';
import { ConfigValueProvider, useConfig } from '@/contexts/ConfigContext';
import { TranscriptValueProvider, type TranscriptContextType } from '@/contexts/TranscriptContext';
import { RecordingStateValueProvider, RecordingStatus, type RecordingStateContextType } from '@/contexts/RecordingStateContext';
import { RecordingControllerValueProvider, type RecordingControllerValue } from '@/contexts/RecordingControllerContext';
import { SIDEBAR_COLLAPSED_WIDTH, SIDEBAR_WIDTH, useShell } from '@/contexts/ShellContext';
import { badge } from '@/lib/theme-classes';
import { useLiveTimeline } from '@/hooks/useLiveTimeline';
import { useShowAiNotes } from '@/lib/aiNotes';
import { aiNoteText, pendingGroups } from '@/lib/aiRawNotes';
import type { ReplaySource, ReplayView } from '@/lib/replay/replaySource';
import type { Transcript } from '@/types';

/** Phases after the user stopped the meeting, until the run ends. */
const PROCESSING_PHASES = new Set(['stopped', 'final-pass-start', 'final-pass-done', 'summary-start', 'summary-done']);
const PHASE_LABEL: Record<string, string> = {
  idle: 'Starting',
  'live-start': 'Recording',
  'audio-end': 'Recording (audio ended)',
  stop: 'Stopping',
  stopped: 'Stopped',
  'final-pass-start': 'Final pass',
  'final-pass-done': 'Final pass done',
  'summary-start': 'Summary',
  'summary-done': 'Summary done',
  'run-end': 'Run ended',
};

export function phaseFlags(phase: string) {
  const ended = phase === 'run-end';
  const stopping = phase === 'stop';
  const processing = PROCESSING_PHASES.has(phase);
  return { recording: !ended && !stopping && !processing, stopping, processing };
}

const noop = () => {};
const noopAsync = async () => {};
const neverRecords = async () => {
  toast.info('Replays never record', { description: 'This is a recorded run playing back.' });
};

/** The notes column, read-only: the notes typed at this point of the run,
 * then the live notes as AI lines under their topics (hidden with "Show AI
 * notes" off), as the notes document shows them. */
export function ReplayNotes({ notes }: { notes: string }) {
  const timeline = useLiveTimeline();
  const [showAi] = useShowAiNotes();
  const groups = showAi ? pendingGroups(timeline?.entries ?? [], new Set()) : [];
  return (
    <div data-replay-notes className="h-full overflow-y-auto px-8 pb-24 pt-4">
      <div className="mx-auto w-full max-w-[860px] text-sm leading-6">
        {notes.trim() ? (
          <pre className="whitespace-pre-wrap font-sans text-ink">{notes}</pre>
        ) : groups.length === 0 ? (
          <p className="text-ink-subtle">No notes at this point of the run.</p>
        ) : null}
        {groups.map((group) => (
          <section key={group.topic} data-ai-topic={group.topic} className="mt-4 text-ink-muted">
            <h3 className="font-semibold">
              {group.topic}
              <span className="ml-1.5 text-[10px] font-normal text-ink-subtle">AI</span>
            </h3>
            <ul className="list-disc pl-5">
              {group.entries.map((entry) => (
                <li key={entry.id}>
                  {aiNoteText(entry)}
                  <span className="ml-1.5 text-[10px] text-ink-subtle">AI</span>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </div>
  );
}

/** The workspace element, memoized so a cursor move re-renders only the
 * components whose context or events changed. */
const Workspace = memo(function Workspace({ title, createdAt, notes, phase }: { title: string; createdAt: string; notes: string; phase: string }) {
  return (
    <MeetingWorkspace
      title={title}
      createdAt={createdAt}
      notes={<ReplayNotes notes={notes} />}
      transcript={<LiveTranscriptPanel />}
      assistant={<CopilotPanel />}
      showAssistant
      assistantLabel="Copilot"
      peopleCount={0}
      initialAssistantOpen
      statusBanner={
        <span className={`shrink-0 rounded-full px-3 py-1.5 text-[11px] font-medium ${badge.info}`} role="status">
          Replay · {PHASE_LABEL[phase] ?? phase}
        </span>
      }
    />
  );
});

export function ReplayWorkspace({
  source,
  view,
  title,
  createdAt,
  playing,
  onTogglePlaying,
}: {
  source: ReplaySource;
  view: ReplayView;
  title: string;
  createdAt: string;
  playing: boolean;
  onTogglePlaying: (playing: boolean) => void;
}) {
  const config = useConfig();
  const { collapsed } = useShell();
  const sidebarWidth = collapsed ? SIDEBAR_COLLAPSED_WIDTH : SIDEBAR_WIDTH;
  const transcriptsRef = useRef<Transcript[]>(view.transcripts);
  transcriptsRef.current = view.transcripts;
  const containerRef = useRef<HTMLDivElement>(null);

  // Live transcription counts as always on: the panel's toggle would write the
  // user's own preference.
  const configValue = useMemo(
    () => ({ ...config, betaFeatures: { ...config.betaFeatures, liveTranscription: false } }),
    [config],
  );

  const transcriptValue = useMemo<TranscriptContextType>(() => ({
    transcripts: view.transcripts,
    transcriptsRef,
    addTranscript: noop,
    copyTranscript: noop,
    flushBuffer: noop,
    transcriptContainerRef: containerRef,
    meetingTitle: title,
    setMeetingTitle: noop,
    clearTranscripts: noop,
    currentMeetingId: null,
    markMeetingAsSaved: noopAsync,
  }), [view.transcripts, title]);

  const flags = phaseFlags(view.phase);
  const seconds = Math.floor(view.audioMs / 1000);
  const recordingValue = useMemo<RecordingStateContextType>(() => ({
    isRecording: flags.recording,
    // The pill's pause is the playback's.
    isPaused: flags.recording && !playing,
    isActive: flags.recording && playing,
    recordingDuration: seconds,
    activeDuration: seconds,
    status: flags.stopping
      ? RecordingStatus.STOPPING
      : flags.processing
        ? RecordingStatus.PROCESSING_TRANSCRIPTS
        : flags.recording
          ? RecordingStatus.RECORDING
          : RecordingStatus.COMPLETED,
    statusMessage: undefined,
    sessionId: null,
    activeMeetingId: null,
    activityStatus: null,
    activityError: null,
    setStatus: noop,
    isStopping: flags.stopping,
    isProcessing: flags.processing,
    isSaving: false,
    isStartingRecording: false,
  }), [flags.recording, flags.stopping, flags.processing, playing, seconds]);

  const controllerValue = useMemo<RecordingControllerValue>(() => ({
    activeMeetingId: null,
    sessionId: null,
    command: null,
    isCommandPending: false,
    feedback: null,
    feedbackManagedGlobally: true,
    startRecording: neverRecords,
    resumeMeeting: neverRecords,
    stopRecording: neverRecords,
    pauseRecording: async () => onTogglePlaying(false),
    resumeRecording: async () => onTogglePlaying(true),
    returnToRecording: noopAsync,
    dismissFeedback: noop,
    canRetryFeedback: false,
    retryFeedback: noopAsync,
    openFeedbackSettings: noopAsync,
    recovery: {
      recoverableMeetings: [],
      isLoading: false,
      isRecovering: false,
      checkForRecoverableTranscripts: noopAsync,
      recoverMeeting: async () => { throw new Error('Not available in a replay'); },
      loadMeetingTranscripts: async () => [],
      deleteRecoverableMeeting: noopAsync,
    },
    isRecoveryOpen: false,
    openRecovery: noop,
    closeRecovery: noop,
  }), [onTogglePlaying]);

  return (
    <LiveSourceProvider source={source}>
      <ConfigValueProvider value={configValue}>
        <RecordingStateValueProvider value={recordingValue}>
          <TranscriptValueProvider value={transcriptValue}>
            <RecordingControllerValueProvider value={controllerValue}>
              <div className="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
                <Workspace key={view.epoch} title={title} createdAt={createdAt} notes={view.notes} phase={view.phase} />
                <PillLayer sidebarWidth={sidebarWidth}>
                  <FloatingRecordingControls />
                </PillLayer>
              </div>
            </RecordingControllerValueProvider>
          </TranscriptValueProvider>
        </RecordingStateValueProvider>
      </ConfigValueProvider>
    </LiveSourceProvider>
  );
}

/**
 * The record pill is `position: fixed` against the window. A transformed
 * layer becomes its containing block, so it floats over this workspace
 * (above the transport bar) instead; the layer starts a sidebar's width to
 * the left because the pill offsets itself by that much.
 */
function PillLayer({ sidebarWidth, children }: { sidebarWidth: number; children: ReactNode }) {
  const layerRef = useRef<HTMLDivElement>(null);
  const [right, setRight] = useState(0);
  // Centre the pill over the notes column so it never covers the Copilot
  // input (the debug drawer makes the workspace narrower than in a meeting).
  useEffect(() => {
    const layer = layerRef.current;
    const workspace = layer?.parentElement;
    if (!layer || !workspace || typeof ResizeObserver === 'undefined') return undefined;
    const measure = () => {
      const notes = workspace.querySelector('[data-replay-notes]');
      if (!notes) return;
      const inset = Math.max(0, workspace.getBoundingClientRect().right - notes.getBoundingClientRect().right);
      setRight((current) => (Math.abs(current - inset) < 1 ? current : inset));
    };
    const observer = new ResizeObserver(measure);
    observer.observe(workspace);
    const notes = workspace.querySelector('[data-replay-notes]');
    if (notes) observer.observe(notes);
    measure();
    return () => observer.disconnect();
  });
  return (
    <div
      ref={layerRef}
      className="pointer-events-none absolute inset-y-0 z-30"
      style={{ left: -sidebarWidth, right, transform: 'translateZ(0)' }}
    >
      {children}
    </div>
  );
}
