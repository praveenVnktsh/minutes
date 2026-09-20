import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { MeetingActivitySnapshot } from '../../src/types/meetingActivity';

let activityListener: ((snapshot: MeetingActivitySnapshot) => void) | undefined;
const unlisten = mock(() => {});
const subscribe = mock(async (listener: (snapshot: MeetingActivitySnapshot) => void) => {
  activityListener = listener;
  listener({ revision: 1, recording: null, activities: [] });
  return unlisten;
});

const originalService = { ...await import('../../src/services/meetingActivityService') };
afterAll(() => mock.module('../../src/services/meetingActivityService', () => originalService));
mock.module('../../src/services/meetingActivityService', () => ({
  meetingActivityService: { subscribe },
}));

const { useTranscriptionProgress } = await import('../../src/hooks/useTranscriptionProgress');

function View({ meetingId }: { meetingId?: string }) {
  const progress = useTranscriptionProgress(meetingId);
  const percent = progress?.indeterminate ? '?' : progress?.percent;
  return <output>{progress ? `${percent}|${progress.stageLabel}|${progress.message ?? ''}` : 'none'}</output>;
}

let renderer: ReactTestRenderer | undefined;
let nextRevision = 10;
const text = () => JSON.stringify(renderer!.toJSON());

async function show(meetingId?: string) {
  await act(async () => {
    const view = <View meetingId={meetingId} />;
    if (renderer) renderer.update(view);
    else renderer = create(view);
    await Promise.resolve();
  });
}

function snapshot(revision: number, meetingId: string | null, progress: number | null, stage = 'transcribing'): MeetingActivitySnapshot {
  return {
    revision,
    recording: null,
    activities: [{
      task_id: `task-${revision}`, meeting_id: meetingId, kind: 'retranscription',
      status: 'transcribing', title: 'Meeting', stage, progress_percentage: progress,
      message: 'Working', error: null, warning: null, controls_available: true, revision,
    }],
  };
}

beforeEach(() => {
  activityListener = undefined;
  subscribe.mockClear();
  unlisten.mockClear();
});

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

describe('useTranscriptionProgress', () => {
  test('hydrates reported native progress for the matching meeting', async () => {
    await show('meeting-a');
    await act(async () => activityListener?.(snapshot(++nextRevision, 'meeting-a', 50)));
    expect(text()).toContain('50|Transcribing|Working');
  });

  test('ignores another meeting and activities without authoritative identity', async () => {
    await show('meeting-a');
    await act(async () => activityListener?.(snapshot(++nextRevision, 'meeting-b', 80)));
    expect(text()).toContain('none');
    await act(async () => activityListener?.(snapshot(++nextRevision, null, 90)));
    expect(text()).toContain('none');
  });

  test('surfaces a pass with no reported percentage as indeterminate, not as zero', async () => {
    await show('meeting-a');
    await act(async () => activityListener?.(snapshot(++nextRevision, 'meeting-a', null, 'queued')));
    expect(text()).toContain('?|Queued|Working');
  });

  test('surfaces a queued pass that has not reported a stage yet', async () => {
    await show('meeting-a');
    await act(async () => {
      const pending = snapshot(++nextRevision, 'meeting-a', null);
      pending.activities[0].status = 'queued';
      pending.activities[0].stage = null;
      activityListener?.(pending);
    });
    expect(text()).toContain('?|Queued|Working');
  });

  test('ignores out-of-order snapshots and cleans up the shared listener', async () => {
    await show('meeting-a');
    const latestRevision = ++nextRevision;
    await act(async () => activityListener?.(snapshot(latestRevision, 'meeting-a', 70)));
    await act(async () => activityListener?.(snapshot(latestRevision - 1, 'meeting-a', 20)));
    expect(text()).toContain('70|Transcribing|Working');
    await act(async () => renderer!.unmount());
    renderer = undefined;
    expect(unlisten).toHaveBeenCalledTimes(1);
  });
});
