import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';

const invoke = mock(async () => []);
const listen = mock(async () => () => {});

mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('@tauri-apps/api/event', () => ({ listen }));

const { TranscriptService } = await import('./transcriptService');

describe('TranscriptService history contract', () => {
  beforeEach(() => invoke.mockClear());
  afterAll(() => mock.restore());

  test('preserves the no-argument active-session history call', async () => {
    await new TranscriptService().getTranscriptHistory();

    expect(invoke).toHaveBeenCalledWith('get_transcript_history', undefined);
  });

  test('requests completed history by exact native session ID', async () => {
    await new TranscriptService().getTranscriptHistory('session-complete');

    expect(invoke).toHaveBeenCalledWith('get_transcript_history', {
      sessionId: 'session-complete',
    });
  });
});
