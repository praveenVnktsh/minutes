import { invoke } from '@tauri-apps/api/core';

/** Which model streams text during a recording. */
export type LiveModelChoice = 'nemotron-streaming' | 'chunked';

export interface LiveModelStatus {
  modelId: string;
  downloaded: boolean;
  downloading: boolean;
  loaded: boolean;
  sizeBytes: number;
}

export interface LiveTranscriptionSettings {
  liveModel: LiveModelChoice;
  autoFinalPass: boolean;
  liveModelStatus: LiveModelStatus;
}

/** `live-model-download-progress` payload. */
export interface LiveModelDownloadProgressEvent {
  modelName: string;
  progress: number;
  downloaded_mb: number;
  total_mb: number;
  speed_mbps: number;
  eta_seconds: number | null;
  status: 'downloading' | 'completed' | 'error' | 'cancelled';
  error: string | null;
}

export const LIVE_MODEL_DOWNLOAD_PROGRESS_EVENT = 'live-model-download-progress';
export const LIVE_MODEL_DISPLAY_NAME = 'Nemotron Streaming 0.6B';

export const liveTranscriptionApi = {
  getSettings: () => invoke<LiveTranscriptionSettings>('get_live_transcription_settings'),
  setSettings: (changes: { liveModel?: LiveModelChoice; autoFinalPass?: boolean }) =>
    invoke<LiveTranscriptionSettings>('set_live_transcription_settings', changes),
  download: () => invoke<void>('live_model_download'),
  cancelDownload: () => invoke<boolean>('live_model_cancel_download'),
  deleteModel: () => invoke<void>('live_model_delete'),
};

/** `start_final_transcript_pass` result. */
export interface FinalPassStarted {
  status: 'queued' | 'skipped';
  taskId: string | null;
  skipReason: 'disabled' | 'edited' | 'no_engine' | null;
  message: string | null;
  provider: string | null;
  model: string | null;
  fallbackNotice: string | null;
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
