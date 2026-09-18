import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

type Handler = (event: { payload: unknown }) => void;

// Capture the handlers the hook registers so a test can drive the two event
// streams directly. Own process per file (scripts/test-each.mjs) means this mock
// cannot leak into another test file.
const handlers = new Map<string, Set<Handler>>();
const listen = mock(async (event: string, handler: Handler) => {
  const set = handlers.get(event) ?? new Set<Handler>();
  set.add(handler);
  handlers.set(event, set);
  return () => {
    set.delete(handler);
  };
});

const originalEvent = { ...await import('@tauri-apps/api/event') };
afterAll(() => {
  mock.module('@tauri-apps/api/event', () => originalEvent);
});
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen }));

const { useTranscriptionProgress } = await import('../../src/hooks/useTranscriptionProgress');

function View({ meetingId }: { meetingId?: string }) {
  const progress = useTranscriptionProgress(meetingId);
  return (
    <output>
      {progress
        ? `${progress.percent}|${progress.stageLabel}|${progress.message ?? ''}`
        : 'none'}
    </output>
  );
}

let renderer: ReactTestRenderer | undefined;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

async function show(meetingId?: string) {
  await act(async () => {
    const view = <View meetingId={meetingId} />;
    if (renderer) renderer.update(view);
    else renderer = create(view);
  });
  await flush();
}

async function emit(event: string, payload: unknown) {
  await act(async () => {
    for (const handler of handlers.get(event) ?? []) handler({ payload });
  });
}

const text = () => JSON.stringify(renderer!.toJSON());

beforeEach(() => {
  handlers.clear();
  listen.mockClear();
});

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

describe('useTranscriptionProgress', () => {
  test('maps a retranscription stage onto the overall percentage', async () => {
    await show('meeting-a');
    await emit('retranscription-progress', {
      meeting_id: 'meeting-a',
      stage: 'transcribing',
      progress_percentage: 50,
      message: 'Transcribing segment 1 of 2',
    });
    expect(text()).toContain('43|Transcribing|Transcribing segment 1 of 2');
  });

  test('continues into diarization stages after transcribing', async () => {
    await show('meeting-a');
    await emit('retranscription-progress', {
      meeting_id: 'meeting-a',
      stage: 'transcribing',
      progress_percentage: 100,
      message: null,
    });
    await emit('diarization-progress', {
      meeting_id: 'meeting-a',
      stage: 'segmenting',
      progress_percentage: 40,
      message: 'Detecting and clustering speakers...',
    });
    expect(text()).toContain('91|Detecting speaker segments|Detecting and clustering speakers...');
  });

  test('reports completion at 100%', async () => {
    await show('meeting-a');
    await emit('retranscription-progress', {
      meeting_id: 'meeting-a',
      stage: 'complete',
      progress_percentage: 100,
      message: 'Retranscription complete',
    });
    expect(text()).toContain('100|Finishing up|Retranscription complete');
  });

  test('ignores progress for another meeting', async () => {
    await show('meeting-a');
    await emit('retranscription-progress', {
      meeting_id: 'meeting-b',
      stage: 'transcribing',
      progress_percentage: 80,
      message: null,
    });
    expect(text()).toContain('none');
  });

  test('clears progress when the workspace switches meeting', async () => {
    await show('meeting-a');
    await emit('diarization-progress', {
      meeting_id: 'meeting-a',
      stage: 'clustering',
      progress_percentage: 70,
      message: null,
    });
    expect(text()).toContain('96|Clustering speakers|');

    await show('meeting-b');
    expect(text()).toContain('none');
  });
});
