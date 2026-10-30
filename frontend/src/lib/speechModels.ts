import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

/**
 * The four FluidAudio models the speech helper runs: live transcription
 * (Nemotron streaming), the transcript after meetings (Parakeet TDT v2), voice
 * detection (Silero) and speakers (pyannote). They are fixed; the app only
 * downloads, deletes and reports them.
 */
export type SpeechModelKind = 'live-asr' | 'final-asr' | 'vad' | 'diarization';

/** One model as `speech_models_status` reports it. */
export interface SpeechModelStatus {
  id: string;
  kind: SpeechModelKind;
  displayName: string;
  sizeBytes: number;
  downloaded: boolean;
  downloading: boolean;
}

/** `speech_models_status` result. `batchReady`: the transcript after meetings
 * can run (final-asr and vad downloaded). `osSupported`: this macOS can run
 * the speech models at all (false before macOS 15; the app still records).
 * Missing from older backends, which read as supported. */
export interface SpeechModelsStatus {
  models: SpeechModelStatus[];
  liveReady: boolean;
  batchReady: boolean;
  diarizationReady: boolean;
  osSupported?: boolean;
}

/** Shown wherever transcription is offered on a macOS that cannot run it. */
export const TRANSCRIPTION_OS_UNSUPPORTED_MESSAGE =
  'Transcription needs macOS 15 or later. Recording still works; update macOS to transcribe meetings.';

/** Whether this macOS can transcribe. Unknown (no status, or an older backend
 * without the field) counts as supported, so nothing is hidden by mistake. */
export function transcriptionOsSupported(status: Pick<SpeechModelsStatus, 'osSupported'> | null | undefined): boolean {
  return status?.osSupported !== false;
}

/**
 * Whether a meeting can be transcribed, for the recording start check:
 * 'ready' when live text or the transcript after meetings can run,
 * 'unsupported' on a macOS that cannot run the models, 'downloading' while
 * any model is downloading, otherwise 'missing'. A failed read is 'missing'.
 */
export type SpeechReadiness = 'ready' | 'downloading' | 'missing' | 'unsupported';

export function speechReadiness(status: SpeechModelsStatus | null): SpeechReadiness {
  if (!status) return 'missing';
  if (!transcriptionOsSupported(status)) return 'unsupported';
  if (status.liveReady || status.batchReady) return 'ready';
  return status.models.some((model) => model.downloading) ? 'downloading' : 'missing';
}

/** What to tell someone who starts recording before live text can run. */
export function recordingWithoutLiveTextMessage(readiness: Exclude<SpeechReadiness, 'ready'>): string {
  switch (readiness) {
    case 'unsupported':
      return TRANSCRIPTION_OS_UNSUPPORTED_MESSAGE;
    case 'downloading':
      return 'Speech models are still downloading. This meeting will be transcribed automatically when they finish.';
    case 'missing':
      return 'The speech models are not downloaded yet. Download them in Settings → Transcription to transcribe this meeting; the recording is kept.';
  }
}

/** `speech-model-download-progress` payload, one stream per model. */
export interface SpeechModelDownloadProgressEvent {
  modelId: string;
  kind: SpeechModelKind;
  progress: number;
  downloadedMb: number;
  totalMb: number;
  speedMbps: number;
  etaSeconds: number | null;
  status: 'downloading' | 'completed' | 'error' | 'cancelled';
  error: string | null;
}

export const SPEECH_MODEL_DOWNLOAD_PROGRESS_EVENT = 'speech-model-download-progress';

export const speechModelsApi = {
  status: () => invoke<SpeechModelsStatus>('speech_models_status'),
  /** Downloads the given models, or every missing one. Resolves when done. */
  download: (kinds?: SpeechModelKind[]) => invoke<void>('speech_models_download', { kinds: kinds ?? null }),
  cancelDownload: () => invoke<boolean>('speech_models_cancel_download'),
  deleteModel: (kind: SpeechModelKind) => invoke<void>('speech_models_delete', { kind }),
  openFolder: () => invoke<void>('speech_models_open_folder'),
};

/** Where the live model runs (Settings → Transcription → Advanced). */
export type LiveEngineChoice = 'auto' | 'gpu' | 'neural-engine';

export const LIVE_ENGINE_OPTIONS: { value: LiveEngineChoice; label: string }[] = [
  { value: 'auto', label: 'Auto (Neural Engine)' },
  { value: 'gpu', label: 'GPU' },
  { value: 'neural-engine', label: 'Neural Engine' },
];

export interface LiveTranscriptionSettings {
  autoFinalPass: boolean;
  liveEngine: LiveEngineChoice;
}

export const liveTranscriptionApi = {
  getSettings: () => invoke<LiveTranscriptionSettings>('get_live_transcription_settings'),
  setSettings: (changes: Partial<LiveTranscriptionSettings>) =>
    invoke<LiveTranscriptionSettings>('set_live_transcription_settings', changes),
};

/** What each model does, for the line under its name. */
export const SPEECH_MODEL_PURPOSE: Record<SpeechModelKind, string> = {
  'live-asr': 'Streams English text while you record',
  'final-asr': 'Transcribes the whole recording after each meeting',
  vad: 'Finds the speech in a recording before it is transcribed',
  diarization: 'Tells speakers apart after each meeting',
};

/** The model of `kind` in a status, if the backend reported it. */
export function speechModel(status: SpeechModelsStatus | null, kind: SpeechModelKind): SpeechModelStatus | null {
  return status?.models.find((model) => model.kind === kind) ?? null;
}

/** Bytes still to download for the models that are not on disk. */
export function missingSpeechBytes(status: SpeechModelsStatus | null): number {
  if (!status) return 0;
  return status.models.filter((model) => !model.downloaded).reduce((sum, model) => sum + model.sizeBytes, 0);
}

/** Card status for one model from its stored state plus its latest progress event. */
export type SpeechModelCardStatus =
  | 'Available'
  | 'Missing'
  | { Downloading: { progress: number } }
  | { Error: string };

export function speechModelCardStatus(
  model: Pick<SpeechModelStatus, 'downloaded' | 'downloading'> | null,
  progress: SpeechModelDownloadProgressEvent | null,
): SpeechModelCardStatus {
  if (progress?.status === 'downloading') return { Downloading: { progress: progress.progress } };
  if (progress?.status === 'error') return { Error: progress.error ?? 'Download failed' };
  if (progress?.status === 'completed' || model?.downloaded) return 'Available';
  if (model?.downloading) return { Downloading: { progress: progress?.progress ?? 0 } };
  return 'Missing';
}

/** Whether the transcript after meetings (import, re-transcribe) can run. */
export type TranscriptAvailability = 'ready' | 'missing' | 'unsupported';

export function transcriptAvailability(status: SpeechModelsStatus): TranscriptAvailability {
  if (!transcriptionOsSupported(status)) return 'unsupported';
  return status.batchReady ? 'ready' : 'missing';
}

/**
 * Whether the transcript after meetings can run, read each time `active`
 * turns true (a dialog opening). null until known or if the check fails, so
 * callers only warn on a definite answer.
 */
export function useTranscriptAvailability(active: boolean): TranscriptAvailability | null {
  const [availability, setAvailability] = useState<TranscriptAvailability | null>(null);
  useEffect(() => {
    if (!active) return;
    let disposed = false;
    speechModelsApi
      .status()
      .then((status) => {
        if (!disposed) setAvailability(transcriptAvailability(status));
      })
      .catch((error) => {
        console.warn('Failed to read the speech models:', error);
        if (!disposed) setAvailability(null);
      });
    return () => {
      disposed = true;
    };
  }, [active]);
  return availability;
}
