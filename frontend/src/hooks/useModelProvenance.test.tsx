import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { MeetingModelProvenance } from './useModelProvenance';

const invoke = mock(async (..._args: unknown[]): Promise<unknown> => ({
  transcription: null,
  diarization: null,
  summary: null,
}));

type Handler = (event: { payload: { meeting_id?: string } }) => void;
const handlers = new Map<string, Handler>();
const unlisten = mock(() => {});
const listen = mock(async (event: string, handler: Handler) => {
  handlers.set(event, handler);
  return unlisten;
});

mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('@tauri-apps/api/event', () => ({ listen }));

const { useModelProvenance } = await import('./useModelProvenance');

let hook: MeetingModelProvenance | null;
let renderer: ReactTestRenderer | undefined;

function Probe({ meetingId }: { meetingId: string | undefined }) {
  hook = useModelProvenance(meetingId);
  return null;
}

async function render(meetingId: string | undefined) {
  await act(async () => {
    renderer = create(<Probe meetingId={meetingId} />);
  });
}

async function emit(event: string, payload: { meeting_id?: string } = {}) {
  await act(async () => {
    handlers.get(event)?.({ payload });
  });
}

beforeEach(() => {
  invoke.mockClear();
  listen.mockClear();
  unlisten.mockClear();
  handlers.clear();
});

afterEach(async () => {
  if (renderer) {
    const current = renderer;
    renderer = undefined;
    await act(async () => current.unmount());
  }
});

afterAll(() => mock.restore());

describe('useModelProvenance', () => {
  test('returns null and does not call invoke without a meeting id', async () => {
    await render(undefined);
    expect(hook).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
    expect(listen).not.toHaveBeenCalled();
  });

  test('fetches provenance for the given meeting', async () => {
    invoke.mockImplementationOnce(async () => ({
      transcription: { provider: 'whisper', model: 'large-v3-turbo' },
      diarization: null,
      summary: null,
    }));
    await render('meeting-1');
    expect(invoke).toHaveBeenCalledWith('get_meeting_model_provenance', { meetingId: 'meeting-1' });
    expect(hook).toEqual({
      transcription: { provider: 'whisper', model: 'large-v3-turbo' },
      diarization: null,
      summary: null,
    });
  });

  test('listens for every provenance-changing event and refetches on a matching meeting id', async () => {
    await render('meeting-1');
    expect([...handlers.keys()]).toEqual([
      'diarization-status-changed',
      'diarization-complete',
      'retranscription-complete',
      'import-complete',
    ]);

    invoke.mockClear();
    invoke.mockImplementationOnce(async () => ({
      transcription: null,
      diarization: { engine: 'pyannote', segmentation_model: 'seg-1', embedding_model: 'emb-1' },
      summary: null,
    }));
    await emit('diarization-complete', { meeting_id: 'meeting-1' });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(hook?.diarization).toEqual({ engine: 'pyannote', segmentation_model: 'seg-1', embedding_model: 'emb-1' });
  });

  test('ignores events for a different meeting', async () => {
    await render('meeting-1');
    invoke.mockClear();
    await emit('retranscription-complete', { meeting_id: 'meeting-2' });
    expect(invoke).not.toHaveBeenCalled();
  });

  test('errors from invoke are swallowed and leave provenance null', async () => {
    const warn = mock(() => {});
    const realWarn = console.warn;
    console.warn = warn;
    invoke.mockImplementationOnce(async () => {
      throw new Error('backend unavailable');
    });
    await render('meeting-1');
    console.warn = realWarn;
    expect(hook).toBeNull();
    expect(warn).toHaveBeenCalled();
  });

  test('refetches when the meeting id changes and unlistens the old listeners', async () => {
    await render('meeting-1');
    expect(invoke).toHaveBeenCalledWith('get_meeting_model_provenance', { meetingId: 'meeting-1' });

    await act(async () => {
      renderer!.update(<Probe meetingId="meeting-2" />);
    });
    expect(unlisten).toHaveBeenCalledTimes(4);
    expect(invoke).toHaveBeenCalledWith('get_meeting_model_provenance', { meetingId: 'meeting-2' });
  });

  test('unmount unlistens every registered event', async () => {
    await render('meeting-1');
    await act(async () => renderer!.unmount());
    renderer = undefined;
    expect(unlisten).toHaveBeenCalledTimes(4);
  });
});
