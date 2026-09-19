import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';

const invoke = mock(async () => ({ session_id: 'session-1' }));
let eventHandler: ((event: { payload: unknown }) => void) | undefined;
const listen = mock(async (_name: string, handler: (event: { payload: unknown }) => void) => {
  eventHandler = handler;
  return () => {};
});

mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('@tauri-apps/api/event', () => ({ listen }));

const { RecordingService } = await import('./recordingService');

describe('RecordingService identity contract', () => {
  beforeEach(() => {
    invoke.mockClear();
    listen.mockClear();
    eventHandler = undefined;
  });

  afterAll(() => mock.restore());

  test('returns the exact native session token and forwards request correlation', async () => {
    const result = await new RecordingService().startRecordingWithDevices(
      null,
      null,
      'Planning',
      'request-1'
    );

    expect(result).toEqual({ session_id: 'session-1' });
    expect(invoke).toHaveBeenCalledWith('start_recording_with_devices_and_meeting', {
      micDeviceName: null,
      systemDeviceName: null,
      meetingName: 'Planning',
      requestId: 'request-1',
    });
  });

  test('preserves the session token from recording-started events', async () => {
    const received: string[] = [];
    await new RecordingService().onRecordingStarted((payload) => {
      received.push(payload.session_id);
    });

    eventHandler?.({ payload: { session_id: 'session-event' } });
    expect(received).toEqual(['session-event']);
  });
});
