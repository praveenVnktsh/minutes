import { describe, expect, mock, test } from 'bun:test';
import type { MeetingActivitySnapshot } from '@/types/meetingActivity';
import type { SummaryProcessResponse } from '@/types';
import { MeetingActivityStore } from './MeetingActivityContext';

const empty = (revision: number): MeetingActivitySnapshot => ({ revision, recording: null, activities: [] });
const summary = (meetingId: string, processId: string, status: SummaryProcessResponse['status']): SummaryProcessResponse => ({
  meeting_id: meetingId,
  start: processId,
  status,
  meetingName: null,
  end: null,
  data: status === 'completed' ? { markdown: 'Ready' } : null,
  error: status === 'failed' ? 'Model failed' : null,
});
const request = (meetingId = 'meeting-a') => ({
  meetingId,
  text: 'Transcript',
  model: 'ollama',
  modelName: 'model',
  chunkSize: 40000,
  overlap: 1000,
  customPrompt: '',
  templateId: 'standard_meeting',
  summaryLanguage: 'en',
});

describe('MeetingActivityStore', () => {
  test('keeps the newest snapshot when hydration races an event', async () => {
    const service = {
      subscribe: mock(async (listener: (value: MeetingActivitySnapshot) => void) => {
        listener(empty(3));
        listener(empty(2));
        return () => {};
      }),
    };
    const store = new MeetingActivityStore({ service });
    const unsubscribe = store.subscribe(() => {});
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(store.getState().snapshot.revision).toBe(3);
    expect(store.getState().hydrationStatus).toBe('ready');
    unsubscribe();
  });

  test('retains native failure/cancellation and accepts a higher-revision retry', async () => {
    let publish!: (value: MeetingActivitySnapshot) => void;
    const store = new MeetingActivityStore({
      service: { subscribe: async (listener) => { publish = listener; return () => {}; } },
    });
    const unsubscribe = store.subscribe(() => {});
    await Promise.resolve();
    const activity = {
      task_id: 'task-a', meeting_id: 'meeting-a', kind: 'retranscription' as const,
      title: 'A', stage: null, progress_percentage: null, message: null,
      warning: null, controls_available: false,
    };
    publish({ revision: 2, recording: null, activities: [{ ...activity, status: 'failed', error: 'decode failed', revision: 2 }] });
    expect(store.getState().snapshot.activities[0]?.status).toBe('failed');
    publish({ revision: 3, recording: null, activities: [{ ...activity, status: 'cancelled', error: null, revision: 3 }] });
    expect(store.getState().snapshot.activities[0]?.status).toBe('cancelled');
    publish({ revision: 4, recording: null, activities: [{ ...activity, status: 'queued', error: null, revision: 4 }] });
    expect(store.getState().snapshot.activities[0]?.status).toBe('queued');
    unsubscribe();
  });

  test('disposes a listener that resolves after the last consumer unmounts', async () => {
    let resolve!: (unlisten: () => void) => void;
    const nativeUnlisten = mock(() => {});
    const store = new MeetingActivityStore({
      service: { subscribe: () => new Promise((done) => { resolve = done; }) },
    });
    const unsubscribe = store.subscribe(() => {});
    unsubscribe();
    resolve(nativeUnlisten);
    await Promise.resolve();
    expect(nativeUnlisten).toHaveBeenCalledTimes(1);
  });

  test('polls distinct meeting/process pairs once and retains terminal outcomes', async () => {
    const responses = new Map([
      ['meeting-a', summary('meeting-a', 'process-a', 'completed')],
      ['meeting-b', summary('meeting-b', 'process-b', 'failed')],
    ]);
    const timers = new Set<() => void>();
    const readSummary = mock(async (meetingId: string) => responses.get(meetingId)!);
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary,
      setIntervalFn: (callback) => { timers.add(callback); return callback; },
      clearIntervalFn: (callback) => timers.delete(callback as () => void),
    });
    const first = mock(() => {});
    const duplicate = mock(() => {});
    store.startSummaryPolling('meeting-a', 'process-a', first);
    store.startSummaryPolling('meeting-a', 'process-a', duplicate);
    store.startSummaryPolling('meeting-b', 'process-b');
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(readSummary).toHaveBeenCalledTimes(2);
    expect(first).toHaveBeenCalledTimes(1);
    expect(duplicate).toHaveBeenCalledTimes(1);
    expect(store.getState().summaries.map(({ status }) => status).sort()).toEqual(['completed', 'failed']);
    expect(timers.size).toBe(0);
  });

  test('stops cancelled work and can track a later process for the same meeting', async () => {
    let status: SummaryProcessResponse['status'] = 'cancelled';
    let processId = 'process-a';
    const readSummary = mock(async () => summary('meeting-a', processId, status));
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary,
      setIntervalFn: (callback) => callback,
      clearIntervalFn: () => {},
    });
    store.startSummaryPolling('meeting-a', 'process-a');
    await Promise.resolve();
    await Promise.resolve();
    expect(store.getState().summaries[0]?.status).toBe('cancelled');

    status = 'processing';
    processId = 'process-b';
    store.startSummaryPolling('meeting-a', 'process-b');
    await Promise.resolve();
    await Promise.resolve();
    expect(readSummary).toHaveBeenCalledTimes(2);
    expect(store.getState().summaries.at(-1)?.status).toBe('processing');
  });

  test('serializes competing starts and registers the native process before resolving', async () => {
    let resolveStart!: (value: { message: string; process_id: string }) => void;
    const startSummary = mock(() => new Promise<{ message: string; process_id: string }>((resolve) => {
      resolveStart = resolve;
    }));
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: async () => summary('meeting-a', '', 'idle'),
      startSummary,
      setIntervalFn: (callback) => callback,
      clearIntervalFn: () => {},
    });
    const first = store.startSummary(request());
    const second = store.startSummary(request());
    await Promise.resolve();
    expect(startSummary).toHaveBeenCalledTimes(1);
    resolveStart({ message: 'started', process_id: 'process-a' });
    expect(await first).toEqual(await second);
    expect(store.getState().summaries.at(-1)?.processId).toBe('process-a');
  });

  test('publishes a cancellable queued attempt for the full pre-start read lifetime', async () => {
    let resolveRead!: (value: SummaryProcessResponse) => void;
    let resolveStart!: (value: { message: string; process_id: string }) => void;
    let firstRead = true;
    const cancelSummary = mock(async () => ({
      cancelled: true, message: 'cancelled', meeting_id: 'meeting-a',
    }));
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: () => {
        if (firstRead) {
          firstRead = false;
          return new Promise((resolve) => { resolveRead = resolve; });
        }
        return Promise.resolve(summary('meeting-a', 'new-process', 'cancelled'));
      },
      startSummary: () => new Promise((resolve) => { resolveStart = resolve; }),
      cancelSummary,
      setIntervalFn: (callback) => callback,
      clearIntervalFn: () => {},
    });

    const starting = store.startSummary({ ...request(), replaceExisting: true });
    expect(store.getState().summaries.at(-1)).toMatchObject({
      meetingId: 'meeting-a', processId: null, status: 'queued',
    });
    const stopping = store.cancelPendingSummaryStart('meeting-a');
    resolveRead(summary('meeting-a', 'old-process', 'completed'));
    await Promise.resolve();
    resolveStart({ message: 'started', process_id: 'new-process' });

    expect((await starting).processId).toBe('new-process');
    expect(await stopping).toBe(true);
    expect(cancelSummary).toHaveBeenCalledWith('meeting-a', 'new-process');
  });

  test('retains start errors for explicit retry and records authoritative cancellation', async () => {
    let starts = 0;
    let stored = summary('meeting-a', '', 'idle');
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: async () => stored,
      startSummary: async () => {
        if (starts++ === 0) throw new Error('provider offline');
        stored = summary('meeting-a', 'process-a', 'processing');
        return { message: 'started', process_id: 'process-a' };
      },
      cancelSummary: async () => ({
        cancelled: true, message: 'cancelled', meeting_id: 'meeting-a',
      }),
      setIntervalFn: (callback) => callback,
      clearIntervalFn: () => {},
    });
    await expect(store.startSummary(request())).rejects.toThrow('provider offline');
    expect(store.getState().summaries.at(-1)?.error).toBe('provider offline');
    expect((await store.retrySummary('meeting-a')).processId).toBe('process-a');
    stored = summary('meeting-a', 'process-a', 'cancelled');
    expect(await store.cancelSummary('meeting-a', 'process-a')).toBe(true);
    expect(store.getState().summaries.at(-1)?.status).toBe('cancelled');
  });

  test('keeps polling after a transport error without fabricating native failure', async () => {
    const timers = new Set<() => void>();
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: async () => { throw new Error('database busy'); },
      setIntervalFn: (callback) => { timers.add(callback); return callback; },
      clearIntervalFn: (callback) => timers.delete(callback as () => void),
    });
    store.startSummaryPolling('meeting-a', 'process-a');
    await new Promise((resolve) => setTimeout(resolve, 0));
    const retained = store.getState().summaries.at(-1);
    expect(retained?.status).toBe('queued');
    expect(retained?.reconciliationError).toBe('database busy');
    expect(timers.size).toBe(1);
  });

  test('retains preparation failures and retries the same automatic request explicitly', async () => {
    let shouldFail = true;
    const startSummary = mock(async () => ({ message: 'started', process_id: 'process-a' }));
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: async () => summary('meeting-a', '', 'idle'),
      startSummary,
      setIntervalFn: (callback) => callback,
      clearIntervalFn: () => {},
    });
    const prepare = async () => {
      if (shouldFail) throw new Error('notes could not be flushed');
      return request();
    };
    await expect(store.prepareAndStartSummary('meeting-a', prepare)).rejects.toThrow('notes could not be flushed');
    expect(store.getState().summaries.at(-1)?.status).toBe('failed');
    shouldFail = false;
    expect((await store.retrySummary('meeting-a')).processId).toBe('process-a');
    expect(startSummary).toHaveBeenCalledTimes(1);
  });

  test('unsubscribes one observer without stopping another observer or the owner poll', async () => {
    let current = summary('meeting-a', 'process-a', 'processing');
    const timers = new Set<() => void>();
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: async () => current,
      setIntervalFn: (callback) => { timers.add(callback); return callback; },
      clearIntervalFn: (callback) => timers.delete(callback as () => void),
    });
    const first = mock(() => {});
    const second = mock(() => {});
    const unsubscribeFirst = store.startSummaryPolling('meeting-a', 'process-a', first);
    store.startSummaryPolling('meeting-a', 'process-a', second);
    await new Promise((resolve) => setTimeout(resolve, 0));
    first.mockClear();
    second.mockClear();
    unsubscribeFirst();
    current = summary('meeting-a', 'process-a', 'completed');
    await Promise.all([...timers].map((timer) => timer()));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
    expect(timers.size).toBe(0);
  });

  test('keeps the newer nanosecond process authoritative when an older response hydrates later', async () => {
    const older = '2026-09-18T10:00:00.123456700Z';
    const newer = '2026-09-18T10:00:00.123456789Z';
    const timers = new Set<() => void>();
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: async () => summary('meeting-a', newer, 'processing'),
      setIntervalFn: (callback) => { timers.add(callback); return callback; },
      clearIntervalFn: (callback) => timers.delete(callback as () => void),
    });
    store.hydrateSummary(summary('meeting-a', newer, 'processing'));
    const authoritative = store.hydrateSummary(summary('meeting-a', older, 'pending'));
    await Promise.resolve();

    expect(authoritative?.start).toBe(newer);
    expect(store.getState().summaries.at(-1)?.processId).toBe(newer);
    expect(timers.size).toBe(1);
  });

  test('terminal hydration stops its poll and a late processing read cannot revive it', async () => {
    const processId = '2026-09-18T10:00:00.123456789Z';
    let resolveRead!: (value: SummaryProcessResponse) => void;
    const timers = new Set<() => void>();
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: () => new Promise((resolve) => { resolveRead = resolve; }),
      setIntervalFn: (callback) => { timers.add(callback); return callback; },
      clearIntervalFn: (callback) => timers.delete(callback as () => void),
    });
    store.startSummaryPolling('meeting-a', processId);
    store.hydrateSummary(summary('meeting-a', processId, 'completed'));
    resolveRead(summary('meeting-a', processId, 'processing'));
    await Promise.resolve();
    await Promise.resolve();

    expect(store.getState().summaries.at(-1)?.status).toBe('completed');
    expect(timers.size).toBe(0);
  });

  test('moves observers to a newer terminal process instead of waiting on the superseded poll', async () => {
    const older = '2026-09-18T10:00:00.123456700Z';
    const newer = '2026-09-18T10:00:00.123456789Z';
    const listener = mock(() => {});
    const timers = new Set<() => void>();
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: async () => summary('meeting-a', newer, 'completed'),
      setIntervalFn: (callback) => { timers.add(callback); return callback; },
      clearIntervalFn: (callback) => timers.delete(callback as () => void),
    });
    store.startSummaryPolling('meeting-a', older, listener);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(listener).toHaveBeenCalledWith(expect.objectContaining({ start: newer, status: 'completed' }));
    expect(store.getState().summaries.some((item) => item.processId === newer && item.status === 'completed')).toBe(true);
    expect(timers.size).toBe(0);
  });

  test('terminal hydration immediately resolves observers of an older process', async () => {
    const older = '2026-09-18T10:00:00.123456700Z';
    const newer = '2026-09-18T10:00:00.123456789Z';
    const listener = mock(() => {});
    const timers = new Set<() => void>();
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: () => new Promise(() => {}),
      setIntervalFn: (callback) => { timers.add(callback); return callback; },
      clearIntervalFn: (callback) => timers.delete(callback as () => void),
    });
    store.startSummaryPolling('meeting-a', older, listener);
    store.hydrateSummary(summary('meeting-a', newer, 'completed'));
    await Promise.resolve();
    await Promise.resolve();

    expect(listener).toHaveBeenCalledWith(expect.objectContaining({ start: newer, status: 'completed' }));
    expect(timers.size).toBe(0);
  });

  test('does not attach a late poll failure to retained state after that poll was removed', async () => {
    let rejectRead!: (error: Error) => void;
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: () => new Promise((_, reject) => { rejectRead = reject; }),
      setIntervalFn: (callback) => callback,
      clearIntervalFn: () => {},
    });
    store.startSummaryPolling('meeting-a', 'process-a');
    store.stopSummaryPolling('meeting-a', 'process-a');
    rejectRead(new Error('late transport failure'));
    await Promise.resolve();
    await Promise.resolve();

    expect(store.getState().summaries.at(-1)?.reconciliationError).toBeNull();
  });

  test('defers a retained listener so a synchronous throw cannot escape subscription', async () => {
    const store = new MeetingActivityStore({ service: { subscribe: async () => () => {} } });
    store.hydrateSummary(summary('meeting-a', 'process-a', 'completed'));
    expect(() => store.startSummaryPolling('meeting-a', 'process-a', () => {
      throw new Error('consumer failed');
    })).not.toThrow();
    await Promise.resolve();
    await Promise.resolve();
  });

  test('cleans up a terminal poll and dispatches observers independently', async () => {
    const timers = new Set<() => void>();
    const second = mock(() => {});
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: async () => summary('meeting-a', 'process-a', 'completed'),
      setIntervalFn: (callback) => { timers.add(callback); return callback; },
      clearIntervalFn: (callback) => timers.delete(callback as () => void),
    });
    store.startSummaryPolling('meeting-a', 'process-a', () => new Promise(() => {}));
    store.startSummaryPolling('meeting-a', 'process-a', second);
    await Promise.resolve();
    await Promise.resolve();

    expect(second).toHaveBeenCalledWith(expect.objectContaining({ status: 'completed' }));
    expect(timers.size).toBe(0);
  });

  test('a start requested after start settles but before cancellation settles waits for a distinct process', async () => {
    const firstProcess = '2026-09-18T10:00:00.123456700Z';
    const secondProcess = '2026-09-18T10:00:00.123456789Z';
    const starts: Array<(value: { message: string; process_id: string }) => void> = [];
    let stored = summary('meeting-a', '', 'idle');
    let resolveCancellation!: (value: { cancelled: boolean; message: string; meeting_id: string }) => void;
    const store = new MeetingActivityStore({
      service: { subscribe: async () => () => {} },
      readSummary: async () => stored,
      startSummary: () => new Promise((resolve) => { starts.push(resolve); }),
      cancelSummary: () => new Promise((resolve) => { resolveCancellation = resolve; }),
      setIntervalFn: (callback) => callback,
      clearIntervalFn: () => {},
    });
    const first = store.startSummary(request());
    await Promise.resolve();
    const cancellation = store.cancelPendingSummaryStart('meeting-a');
    stored = summary('meeting-a', firstProcess, 'processing');
    starts[0]!({ message: 'started', process_id: firstProcess });
    expect((await first).processId).toBe(firstProcess);
    await Promise.resolve();
    let secondSettled = false;
    const second = store.startSummary({ ...request(), customPrompt: 'new request' })
      .finally(() => { secondSettled = true; });
    await Promise.resolve();
    expect(secondSettled).toBe(false);
    expect(starts).toHaveLength(1);

    resolveCancellation({ cancelled: true, message: 'cancelled', meeting_id: 'meeting-a' });
    expect(await cancellation).toBe(true);
    await Promise.resolve();
    expect(starts).toHaveLength(2);
    starts[1]!({ message: 'started', process_id: secondProcess });

    expect((await second).processId).toBe(secondProcess);
  });
});
