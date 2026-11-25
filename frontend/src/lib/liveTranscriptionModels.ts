import { invoke } from '@tauri-apps/api/core';

// The live settings moved next to the speech models they configure; these
// re-exports keep older imports working.
export {
  LIVE_ENGINE_OPTIONS,
  liveTranscriptionApi,
  type LiveEngineChoice,
  type LiveTranscriptionSettings,
} from './speechModels';

/** When the final pass ("Improve transcript after meetings") runs. */
export type FinalPassTiming = 'automatic' | 'when-plugged-in' | 'off';

export const finalPassApi = {
  getTiming: () => invoke<FinalPassTiming>('get_final_pass_timing'),
  setTiming: (timing: FinalPassTiming) => invoke<FinalPassTiming>('set_final_pass_timing', { timing }),
  /** Whether the meeting's pass waits for the Mac to be plugged in. */
  isWaiting: (meetingId: string) => invoke<boolean>('final_pass_waiting', { meetingId }),
  /** Run a waiting pass now, on battery or not. */
  runNow: (meetingId: string) => invoke<FinalPassStarted>('run_waiting_final_pass', { meetingId }),
};

/** `final-transcript-pass-waiting` payload. */
export interface FinalPassWaitingEvent {
  meeting_id: string;
  waiting: boolean;
}

export const FINAL_PASS_WAITING_EVENT = 'final-transcript-pass-waiting';

/** `start_final_transcript_pass` result. `waiting`: it runs once the Mac is
 * plugged in; until then the live transcript is the meeting's transcript. */
export interface FinalPassStarted {
  status: 'queued' | 'waiting' | 'skipped';
  taskId: string | null;
  skipReason: 'disabled' | 'edited' | 'no_engine' | null;
  message: string | null;
  provider: string | null;
  model: string | null;
  /** Older builds named a fallback engine here; with one engine it stays empty. */
  fallbackNotice?: string | null;
  /** What a `waiting` pass waits for; older builds only waited for power. */
  waitingFor?: 'power' | 'models' | null;
}

/** `final-transcript-pass-complete` payload: the hook for work that should
 * run on the kept transcript (e.g. the auto-summary). */
export interface FinalPassComplete {
  meeting_id: string;
  task_id: string;
  outcome: 'replaced' | 'kept_live';
  provider: string | null;
  model: string | null;
  warning: string | null;
}

export const FINAL_PASS_COMPLETE_EVENT = 'final-transcript-pass-complete';

/**
 * Whether the final pass took over post-processing. Only a queued pass does:
 * then it replaces the transcript, rediarizes, and its completion drives the
 * summary. Anything else (skipped, an older backend, a failed call) keeps the
 * live transcript's own diarization and summary.
 */
export function finalPassTookOver(result: unknown): result is FinalPassStarted {
  return (
    typeof result === 'object'
    && result !== null
    && (result as Partial<FinalPassStarted>).status === 'queued'
  );
}

/**
 * Whether the pass waits for the speech models to finish downloading. It then
 * runs by itself once they land, so nothing should retranscribe in its place.
 */
export function finalPassWaitsForModels(result: unknown): boolean {
  return (
    typeof result === 'object'
    && result !== null
    && (result as Partial<FinalPassStarted>).status === 'waiting'
    && (result as Partial<FinalPassStarted>).waitingFor === 'models'
  );
}

/** A user-facing note for the result, if it deserves one. */
export function finalPassNotice(result: unknown): string | null {
  if (typeof result !== 'object' || result === null) return null;
  const started = result as Partial<FinalPassStarted>;
  if (started.status === 'queued') return started.fallbackNotice ?? null;
  if (started.status === 'skipped' && started.skipReason !== 'disabled') {
    return started.message ?? null;
  }
  return null;
}

/** `transcript-partial` payload: the utterance in progress on one source. */
export interface TranscriptPartialEvent {
  source: string;
  text: string;
  audio_start_time: number;
}

export const TRANSCRIPT_PARTIAL_EVENT = 'transcript-partial';

export type LivePartials = Record<string, { text: string; start: number }>;

/** Fold a partial event into the per-source in-progress lines. Empty text
 * clears the source (its utterance was committed). */
export function applyPartial(partials: LivePartials, event: TranscriptPartialEvent): LivePartials {
  const text = event.text.trim();
  const current = partials[event.source];
  if (!text) {
    if (!current) return partials;
    const next = { ...partials };
    delete next[event.source];
    return next;
  }
  if (current && current.text === text && current.start === event.audio_start_time) return partials;
  return { ...partials, [event.source]: { text, start: event.audio_start_time } };
}

/** In-progress lines in timeline order, for display. */
export function orderedPartials(partials: LivePartials) {
  return Object.entries(partials)
    .map(([source, value]) => ({ source, ...value }))
    .sort((a, b) => a.start - b.start);
}
