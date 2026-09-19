import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { MeetingActivitySnapshot } from '@/types/meetingActivity';

let snapshot: MeetingActivitySnapshot;
const durationResolvers: Array<(value: unknown) => void> = [];
const getRecordingState = mock(() => new Promise((resolve) => durationResolvers.push(resolve)));
const listener = mock(async () => () => {});

const originalActivity = { ...await import('./MeetingActivityContext') };
const originalRecordingService = { ...await import('@/services/recordingService') };
const originalToast = { ...await import('sonner') };
afterAll(() => {
  mock.module('./MeetingActivityContext', () => originalActivity);
  mock.module('@/services/recordingService', () => originalRecordingService);
  mock.module('sonner', () => originalToast);
});
mock.module('./MeetingActivityContext', () => ({
  useMeetingActivity: () => ({
    recording: snapshot.recording,
    hydrationStatus: 'ready',
    snapshot,
  }),
}));
mock.module('@/services/recordingService', () => ({
  recordingService: {
    getRecordingState,
    onMicDeviceSwitched: listener,
    onMicSwapFailed: listener,
    onMicUnavailable: listener,
    onMicRecoveryExhausted: listener,
  },
}));
mock.module('sonner', () => ({ toast: { info() {}, error() {} } }));

const { RecordingStateProvider, RecordingStatus, useRecordingState } = await import('./RecordingStateContext');
let state: ReturnType<typeof useRecordingState>;
function Consumer() {
  state = useRecordingState();
  return null;
}

let renderer: ReactTestRenderer | undefined;
const realSetInterval = globalThis.setInterval;
const realClearInterval = globalThis.clearInterval;

function recordingSnapshot(revision: number, sessionId: string): MeetingActivitySnapshot {
  return {
    revision,
    recording: { session_id: sessionId, meeting_id: `meeting-${sessionId}`, status: 'recording', error: null },
    activities: [],
  };
}

beforeEach(() => {
  snapshot = recordingSnapshot(1, 'a');
  durationResolvers.length = 0;
  getRecordingState.mockClear();
  listener.mockClear();
  globalThis.setInterval = ((callback: () => void) => {
    return callback;
  }) as unknown as typeof setInterval;
  globalThis.clearInterval = (() => {}) as typeof clearInterval;
});

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
  globalThis.setInterval = realSetInterval;
  globalThis.clearInterval = realClearInterval;
});

describe('RecordingStateContext activity compatibility', () => {
  test('discards delayed duration reads from an older native session', async () => {
    await act(async () => { renderer = create(<RecordingStateProvider><Consumer /></RecordingStateProvider>); });
    snapshot = recordingSnapshot(2, 'b');
    await act(async () => renderer!.update(<RecordingStateProvider><Consumer /></RecordingStateProvider>));

    await act(async () => durationResolvers[0]({
      session_id: 'a', active_meeting_id: 'meeting-a', is_recording: true, is_paused: false,
      is_active: true, recording_duration: 10, active_duration: 10,
    }));
    expect(state.sessionId).toBe('b');
    expect(state.recordingDuration).toBeNull();

    await act(async () => durationResolvers[1]({
      session_id: 'b', active_meeting_id: 'meeting-b', is_recording: true, is_paused: false,
      is_active: true, recording_duration: 20, active_duration: 18,
    }));
    expect(state.recordingDuration).toBe(20);
    expect(state.activeDuration).toBe(18);
  });

  test('keeps frontend finalization status when native recording becomes terminal', async () => {
    await act(async () => { renderer = create(<RecordingStateProvider><Consumer /></RecordingStateProvider>); });
    await act(async () => state.setStatus(RecordingStatus.SAVING, 'Saving meeting'));
    snapshot = {
      revision: 2,
      recording: null,
      activities: [{
        task_id: 'a', meeting_id: 'meeting-a', kind: 'recording', status: 'ready',
        title: 'Recording', stage: 'complete', progress_percentage: null, message: null,
        error: null, warning: null, controls_available: false, revision: 2,
      }],
    };
    await act(async () => renderer!.update(<RecordingStateProvider><Consumer /></RecordingStateProvider>));
    expect(state.status).toBe(RecordingStatus.SAVING);
    expect(state.isRecording).toBe(false);
  });
});
