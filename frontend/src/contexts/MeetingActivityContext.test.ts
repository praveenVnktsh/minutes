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

  test('stops cancelled work and permits an explicit retry of the same process', async () => {
    let status: SummaryProcessResponse['status'] = 'cancelled';
    const readSummary = mock(async () => summary('meeting-a', 'process-a', status));
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
    store.startSummaryPolling('meeting-a', 'process-a');
    await Promise.resolve();
    await Promise.resolve();
    expect(readSummary).toHaveBeenCalledTimes(2);
    expect(store.getState().summaries[0]?.status).toBe('processing');
  });
});
