import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestInstance, type ReactTestRenderer } from 'react-test-renderer';
import type { SpeechModelsStatus, SpeechModelDownloadProgressEvent } from '../../src/lib/speechModels';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvent = { ...await import('@tauri-apps/api/event') };
const originalLiveSettings = { ...await import('../../src/components/LiveTranscriptionModelSettings') };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
  mock.module('../../src/components/LiveTranscriptionModelSettings', () => originalLiveSettings);
});

const MIB = 1024 * 1024;
let downloaded: Record<string, boolean>;
let downloadCall: { resolve: () => void } | null;
const calls: Array<[string, unknown]> = [];

function status(): SpeechModelsStatus {
  const model = (id: string, kind: SpeechModelsStatus['models'][number]['kind'], displayName: string, size: number) => ({
    id, kind, displayName, sizeBytes: size * MIB, downloaded: downloaded[kind] ?? false, downloading: false,
  });
  return {
    models: [
      model('nemotron-speech-streaming-en-0.6b-coreml', 'live-asr', 'Nemotron Streaming 0.6B', 600),
      model('parakeet-tdt-0.6b-v2-coreml', 'final-asr', 'Parakeet TDT 0.6B v2', 450),
      model('silero-vad-coreml', 'vad', 'Silero VAD', 2),
      model('speaker-diarization-coreml', 'diarization', 'Pyannote community-1', 30),
    ],
    liveReady: downloaded['live-asr'] ?? false,
    batchReady: (downloaded['final-asr'] ?? false) && (downloaded.vad ?? false),
    diarizationReady: downloaded.diarization ?? false,
  };
}

const invoke = mock(async (command: string, args?: unknown) => {
  calls.push([command, args]);
  if (command === 'speech_models_status') return status();
  if (command === 'speech_models_download') {
    return new Promise<void>((resolve) => { downloadCall = { resolve }; });
  }
  if (command === 'speech_models_delete') return null;
  throw new Error(`Unexpected command: ${command}`);
});

type Handler = (event: { payload: SpeechModelDownloadProgressEvent }) => void;
let progressHandler: Handler | null = null;
const listen = mock(async (_event: string, handler: Handler) => {
  progressHandler = handler;
  return () => {
    if (progressHandler === handler) progressHandler = null;
  };
});

mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen }));
// The engine and timing selects have their own tests; this file covers the model cards.
mock.module('../../src/components/LiveTranscriptionModelSettings', () => ({
  ...originalLiveSettings,
  LiveEngineSettings: () => null,
  FinalPassTimingSettings: () => null,
}));

const { TranscriptSettings } = await import('../../src/components/TranscriptSettings');

function text(node: ReactTestRenderer): string {
  return JSON.stringify(node.toJSON());
}

function textOf(node: ReactTestInstance | string): string {
  return typeof node === 'string' ? node : node.children.map(textOf).join('');
}

function buttons(renderer: ReactTestRenderer, label: string): ReactTestInstance[] {
  return renderer.root.findAll((node) => node.type === 'button' && textOf(node).trim().startsWith(label));
}

function button(renderer: ReactTestRenderer, label: string): ReactTestInstance {
  const found = buttons(renderer, label);
  if (found.length === 0) throw new Error(`No button "${label}"`);
  return found[0];
}

async function mount(): Promise<ReactTestRenderer> {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<TranscriptSettings />);
    await Promise.resolve();
  });
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  return renderer;
}

beforeEach(() => {
  downloaded = {};
  downloadCall = null;
  calls.length = 0;
  progressHandler = null;
});

describe('TranscriptSettings', () => {
  test('lists the four speech models under their sections', async () => {
    downloaded = { 'final-asr': true, vad: true };
    const renderer = await mount();
    const rendered = text(renderer);
    for (const label of ['Live transcription', 'Transcript after meetings', 'Speakers',
      'Nemotron Streaming 0.6B', 'Parakeet TDT 0.6B v2', 'Silero VAD', 'Pyannote community-1']) {
      expect(rendered).toContain(label);
    }
    expect(rendered).toContain('without live text');
    renderer.unmount();
  });

  test('downloads one model and shows its progress until it completes', async () => {
    const renderer = await mount();
    const downloads = buttons(renderer, 'Download').filter((node) => textOf(node).trim() === 'Download');
    // Live first, in section order.
    await act(async () => { downloads[0].props.onClick({ stopPropagation() {} }); });
    expect(calls).toContainEqual(['speech_models_download', { kinds: ['live-asr'] }]);
    expect(text(renderer)).toContain('Downloading…');

    await act(async () => {
      progressHandler?.({ payload: {
        modelId: 'nemotron-speech-streaming-en-0.6b-coreml', kind: 'live-asr', progress: 40, downloadedMb: 240,
        totalMb: 600, speedMbps: 20, etaSeconds: 18, status: 'downloading', error: null,
      } });
    });
    expect(text(renderer)).toContain('"40","%"');

    downloaded = { 'live-asr': true };
    await act(async () => {
      progressHandler?.({ payload: {
        modelId: 'nemotron-speech-streaming-en-0.6b-coreml', kind: 'live-asr', progress: 100, downloadedMb: 600,
        totalMb: 600, speedMbps: 20, etaSeconds: 0, status: 'completed', error: null,
      } });
      downloadCall?.resolve();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const rendered = text(renderer);
    expect(rendered).not.toContain('Downloading…');
    expect(rendered).toContain('Ready');
    renderer.unmount();
  });

  test('download all asks for every missing model at once', async () => {
    downloaded = { 'live-asr': true };
    const renderer = await mount();
    await act(async () => { button(renderer, 'Download all').props.onClick(); });
    expect(calls).toContainEqual(['speech_models_download', { kinds: null }]);
    renderer.unmount();
  });
});
