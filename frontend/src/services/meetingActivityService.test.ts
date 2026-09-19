import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';

const invoke = mock(async () => ({ revision: 1, recording: null, activities: [] }));
let eventHandler: ((event: { payload: unknown }) => void) | undefined;
const unlisten = mock(() => {});
const listen = mock(async (_name: string, handler: (event: { payload: unknown }) => void) => {
  eventHandler = handler;
  return unlisten;
});

mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('@tauri-apps/api/event', () => ({ listen }));

const { MeetingActivityService } = await import('./meetingActivityService');

describe('MeetingActivityService', () => {
  beforeEach(() => {
    invoke.mockClear();
    listen.mockClear();
    unlisten.mockClear();
    eventHandler = undefined;
  });

  afterAll(() => mock.restore());

  test('subscribes before hydrating and ignores an older hydration snapshot', async () => {
    invoke.mockImplementationOnce(async () => {
      eventHandler?.({ payload: { revision: 2, recording: null, activities: [] } });
      return { revision: 1, recording: null, activities: [] };
    });
    const received: number[] = [];

    const dispose = await new MeetingActivityService().subscribe((snapshot) => {
      received.push(snapshot.revision);
    });

    expect(received).toEqual([2]);
    dispose();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  test('binds meeting identity with camel-case invoke arguments', async () => {
    await new MeetingActivityService().bindActiveRecordingMeeting('session-1', 'meeting-1');
    expect(invoke).toHaveBeenCalledWith('bind_active_recording_meeting', {
      sessionId: 'session-1',
      meetingId: 'meeting-1',
    });
  });

  test('hydrates and claims a pending recording request', async () => {
    const service = new MeetingActivityService();
    await service.getPendingRecordingRequest();
    await service.claimRecordingRequest('request-1', 'recording-controller');

    expect(invoke).toHaveBeenNthCalledWith(1, 'get_pending_recording_request');
    expect(invoke).toHaveBeenNthCalledWith(2, 'claim_recording_request', {
      requestId: 'request-1',
      claimant: 'recording-controller',
    });
  });
});
