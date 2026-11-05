import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';

// Restore the real module afterwards so a mocked `invoke` cannot leak into a
// later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
});

const invokeCalls: Array<[string, unknown]> = [];
mock.module('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args?: unknown) => {
    invokeCalls.push([command, args]);
    return undefined;
  },
}));

const {
  TRANSCRIPTION_OS_UNSUPPORTED_MESSAGE,
  liveTranscriptionApi,
  missingSpeechBytes,
  recordingWithoutLiveTextMessage,
  speechReadiness,
  transcriptAvailability,
  transcriptionOsSupported,
  speechModel,
  speechModelCardStatus,
  speechModelsApi,
} = await import('./speechModels');
type SpeechModelsStatus = import('./speechModels').SpeechModelsStatus;
type SpeechModelDownloadProgressEvent = import('./speechModels').SpeechModelDownloadProgressEvent;

const MIB = 1024 * 1024;

function status(downloaded: Partial<Record<string, boolean>> = {}): SpeechModelsStatus {
  return {
    models: [
      { id: 'nemotron-speech-streaming-en-0.6b-coreml', kind: 'live-asr', displayName: 'Nemotron Streaming 0.6B', sizeBytes: 600 * MIB, downloaded: downloaded['live-asr'] ?? false, downloading: false },
      { id: 'parakeet-tdt-0.6b-v2-coreml', kind: 'final-asr', displayName: 'Parakeet TDT 0.6B v2', sizeBytes: 450 * MIB, downloaded: downloaded['final-asr'] ?? false, downloading: false },
      { id: 'silero-vad-coreml', kind: 'vad', displayName: 'Silero VAD', sizeBytes: 2 * MIB, downloaded: downloaded.vad ?? false, downloading: false },
      { id: 'speaker-diarization-coreml', kind: 'diarization', displayName: 'Pyannote speakers', sizeBytes: 30 * MIB, downloaded: downloaded.diarization ?? false, downloading: false },
    ],
    liveReady: downloaded['live-asr'] ?? false,
    batchReady: (downloaded['final-asr'] ?? false) && (downloaded.vad ?? false),
    diarizationReady: downloaded.diarization ?? false,
  };
}

function progress(
  state: SpeechModelDownloadProgressEvent['status'],
  percent = 0,
  error: string | null = null,
): SpeechModelDownloadProgressEvent {
  return {
    modelId: 'parakeet-tdt-0.6b-v2-coreml',
    kind: 'final-asr',
    progress: percent,
    downloadedMb: 0,
    totalMb: 450,
    speedMbps: 0,
    etaSeconds: null,
    status: state,
    error,
  };
}

beforeEach(() => {
  invokeCalls.length = 0;
});

describe('speechModelsApi', () => {
  test('sends the commands and arguments the Rust side registers', async () => {
    await speechModelsApi.status();
    await speechModelsApi.download();
    await speechModelsApi.download(['final-asr', 'vad']);
    await speechModelsApi.cancelDownload();
    await speechModelsApi.deleteModel('diarization');
    await speechModelsApi.openFolder();
    await liveTranscriptionApi.setSettings({ liveEngine: 'gpu' });
    expect(invokeCalls).toEqual([
      ['speech_models_status', undefined],
      ['speech_models_download', { kinds: null }],
      ['speech_models_download', { kinds: ['final-asr', 'vad'] }],
      ['speech_models_cancel_download', undefined],
      ['speech_models_delete', { kind: 'diarization' }],
      ['speech_models_open_folder', undefined],
      ['set_live_transcription_settings', { liveEngine: 'gpu' }],
    ]);
  });
});

describe('speechModel and missingSpeechBytes', () => {
  test('find a model by kind and sum what is left to download', () => {
    expect(speechModel(null, 'vad')).toBeNull();
    expect(speechModel(status(), 'vad')?.id).toBe('silero-vad-coreml');
    expect(missingSpeechBytes(null)).toBe(0);
    expect(missingSpeechBytes(status())).toBe(1082 * MIB);
    expect(missingSpeechBytes(status({ 'live-asr': true, vad: true }))).toBe(480 * MIB);
    expect(missingSpeechBytes(status({ 'live-asr': true, 'final-asr': true, vad: true, diarization: true }))).toBe(0);
  });
});

describe('speechModelCardStatus', () => {
  test('reflects the stored download state before any event', () => {
    expect(speechModelCardStatus(null, null)).toBe('Missing');
    expect(speechModelCardStatus({ downloaded: false, downloading: false }, null)).toBe('Missing');
    expect(speechModelCardStatus({ downloaded: true, downloading: false }, null)).toBe('Available');
    expect(speechModelCardStatus({ downloaded: false, downloading: true }, null)).toEqual({
      Downloading: { progress: 0 },
    });
  });

  test('progress events take precedence while downloading and on failure', () => {
    const missing = { downloaded: false, downloading: false };
    expect(speechModelCardStatus(missing, progress('downloading', 42))).toEqual({ Downloading: { progress: 42 } });
    expect(speechModelCardStatus(missing, progress('error', 0, 'network'))).toEqual({ Error: 'network' });
    expect(speechModelCardStatus(missing, progress('error'))).toEqual({ Error: 'Download failed' });
    expect(speechModelCardStatus(missing, progress('completed', 100))).toBe('Available');
    expect(speechModelCardStatus(missing, progress('cancelled'))).toBe('Missing');
  });
});

describe('macOS support', () => {
  test('a missing osSupported field (older backend) or an unknown status counts as supported', () => {
    expect(transcriptionOsSupported(null)).toBe(true);
    expect(transcriptionOsSupported(undefined)).toBe(true);
    expect(transcriptionOsSupported(status())).toBe(true);
    expect(transcriptionOsSupported({ ...status(), osSupported: true })).toBe(true);
    expect(transcriptionOsSupported({ ...status(), osSupported: false })).toBe(false);
  });

  test('the transcript after meetings is unsupported before ready or missing', () => {
    const ready = status({ 'final-asr': true, vad: true });
    expect(transcriptAvailability(ready)).toBe('ready');
    expect(transcriptAvailability(status())).toBe('missing');
    expect(transcriptAvailability({ ...ready, osSupported: false })).toBe('unsupported');
    expect(transcriptAvailability({ ...status(), osSupported: false })).toBe('unsupported');
  });
});

describe('speechReadiness', () => {
  test('ready when live text or the transcript after meetings can run', () => {
    expect(speechReadiness(status({ 'live-asr': true }))).toBe('ready');
    expect(speechReadiness(status({ 'final-asr': true, vad: true }))).toBe('ready');
  });

  test('downloading while any model downloads, missing otherwise or when the read failed', () => {
    const downloading = status();
    downloading.models[1] = { ...downloading.models[1], downloading: true };
    expect(speechReadiness(downloading)).toBe('downloading');
    expect(speechReadiness(status())).toBe('missing');
    expect(speechReadiness(null)).toBe('missing');
  });

  test('unsupported on a macOS that cannot run the models, whatever is on disk', () => {
    expect(speechReadiness({ ...status({ 'live-asr': true }), osSupported: false })).toBe('unsupported');
  });
});

describe('recordingWithoutLiveTextMessage', () => {
  test('says the meeting is transcribed automatically once the download finishes', () => {
    expect(recordingWithoutLiveTextMessage('downloading')).toBe(
      'Speech models are still downloading. This meeting will be transcribed automatically when they finish.',
    );
  });

  test('points at Settings → Transcription when nothing is downloading', () => {
    expect(recordingWithoutLiveTextMessage('missing')).toContain('Settings → Transcription');
  });

  test('gives the macOS 15 message on an unsupported macOS', () => {
    expect(recordingWithoutLiveTextMessage('unsupported')).toBe(TRANSCRIPTION_OS_UNSUPPORTED_MESSAGE);
    expect(TRANSCRIPTION_OS_UNSUPPORTED_MESSAGE).toBe(
      'Transcription needs macOS 15 or later. Recording still works; update macOS to transcribe meetings.',
    );
  });
});
