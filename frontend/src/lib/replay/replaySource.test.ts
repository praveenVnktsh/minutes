import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { applyDelta, acceptCopilotSnapshot, type CopilotSnapshot } from '@/lib/copilot';
import { acceptSnapshot, type TimelineSnapshot } from '@/lib/liveTimeline';
import { applyPartial, type LivePartials } from '@/lib/liveTranscriptionModels';
import { buildIndex, fold, parseJsonl } from './reducer';
import { INCREMENTAL_LIMIT, ReplaySource, type LiveAskRequest } from './replaySource';

/* eslint-disable @typescript-eslint/no-explicit-any */

const sample = readFileSync(fileURLToPath(new URL('../../../../site/replay/sample-events.jsonl', import.meta.url)), 'utf8');
const records = parseJsonl(sample);
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const plain = (value: unknown) => JSON.parse(JSON.stringify(value));

function backend() {
  const asks: Array<{ request: LiveAskRequest; id: string }> = [];
  let stops = 0;
  return {
    asks,
    stops: () => stops,
    ask: async (request: LiveAskRequest, id: string) => { asks.push({ request, id }); },
    stop: async () => { stops += 1; },
  };
}

/** A listener that folds what it hears the way the app's hooks do. */
async function mount(source: ReplaySource) {
  const seen = { copilot: null as CopilotSnapshot | null, timeline: null as TimelineSnapshot | null, partials: {} as LivePartials };
  await source.listen<CopilotSnapshot>('copilot-state', ({ payload }) => { seen.copilot = acceptCopilotSnapshot(seen.copilot, payload); });
  await source.listen<any>('copilot-delta', ({ payload }) => { seen.copilot = applyDelta(seen.copilot, payload); });
  await source.listen<TimelineSnapshot>('live-timeline-update', ({ payload }) => { seen.timeline = acceptSnapshot(seen.timeline, payload); });
  await source.listen<any>('transcript-partial', ({ payload }) => { seen.partials = applyPartial(seen.partials, payload); });
  await flush();
  return seen;
}

function expected(n: number) {
  const state = fold(records, n);
  const partials: LivePartials = {};
  for (const p of Object.values(state.partials)) partials[p.source] = { text: p.text.trim(), start: p.start };
  return { state, partials };
}

const lastWith = (name: string) => records.findLastIndex((r) => r.name === name) + 1;

const topicRecords = parseJsonl([
  { seq: 0, t_ms: 0, audio_ms: 0, type: 'meta', name: 'run-start', payload: { schema_version: 1 } },
  {
    seq: 1, t_ms: 10, audio_ms: 64_000, type: 'event', name: 'live-timeline-update',
    payload: {
      session_id: 's', meeting_id: 'm', status: 'listening', error: null,
      entries: [
        { id: 'a', start_ms: 5_000, end_ms: 9_000, kind: 'point', text: 'Sync bug is the last blocker', speaker: null, provider: 'p', model: 'm', created_at: '', topic: 'Launch Plan' },
        { id: 'b', start_ms: 62_000, end_ms: 70_000, kind: 'point', text: 'Free tier held back', speaker: null, provider: 'p', model: 'm', created_at: '', topic: 'Pricing' },
      ],
    },
  },
].map((r) => JSON.stringify(r)).join('\n'));

describe('ReplaySource with topics', () => {
  test('the live summary it serves and sends keeps each entry\'s topic', async () => {
    const source = new ReplaySource(buildIndex(topicRecords, 50), backend());
    source.seek(topicRecords.length);
    const read = await source.invoke<TimelineSnapshot | null>('get_live_timeline');
    expect(read?.entries.map((e) => e.topic)).toEqual(['Launch Plan', 'Pricing']);
    const seen = await mount(source);
    expect(seen.timeline?.entries.map((e) => e.topic)).toEqual(['Launch Plan', 'Pricing']);
  });
});

describe('ReplaySource', () => {
  test('a listener mounted mid-run is hydrated to the cursor', async () => {
    const source = new ReplaySource(buildIndex(records, 50), backend());
    const n = lastWith('live-timeline-update') + 3;
    source.seek(n);
    const seen = await mount(source);
    const { state, partials } = expected(n);
    expect(plain(seen.timeline)).toEqual(plain(state.timeline));
    expect(seen.partials).toEqual(partials);
    expect(seen.copilot?.messages.map((m) => m.id)).toEqual(state.copilot.messages.map((m: any) => m.id));
    expect(seen.copilot?.messages.map((m) => m.reply)).toEqual(state.copilot.messages.map((m: any) => m.reply));
    expect(source.getView().transcripts.length).toBe(state.lines.length);
  });

  test('playing forward delivers the recorded events and ends where the fold ends', async () => {
    const source = new ReplaySource(buildIndex(records, 50), backend());
    const seen = await mount(source);
    for (let n = 0; n <= records.length; n += 7) source.advance(n);
    source.advance(records.length);
    const { state, partials } = expected(records.length);
    expect(plain(seen.timeline)).toEqual(plain(state.timeline));
    expect(seen.partials).toEqual(partials);
    expect(seen.copilot?.messages.map((m) => m.reply)).toEqual(state.copilot.messages.map((m: any) => m.reply));
  });

  test('seeking backwards and forwards delivers a consistent snapshot every time', async () => {
    const source = new ReplaySource(buildIndex(records, 50), backend());
    let seen = await mount(source);
    let epoch = source.getView().epoch;
    for (const n of [records.length, 400, 120, 450, 60, records.length - 2, 300]) {
      source.seek(n);
      if (source.getView().epoch !== epoch) {
        // The workspace remounts: fresh listeners hydrate.
        epoch = source.getView().epoch;
        seen = await mount(source);
      }
      const { state, partials } = expected(n);
      expect(plain(seen.timeline)).toEqual(plain(state.timeline));
      expect(seen.partials).toEqual(partials);
      expect((seen.copilot?.messages ?? []).map((m) => m.reply)).toEqual((state.copilot?.messages ?? []).map((m: any) => m.reply));
    }
  });

  test('a jump longer than the incremental limit is delivered as the state', async () => {
    const source = new ReplaySource(buildIndex(records, 50), backend());
    const deltas: unknown[] = [];
    await source.listen('copilot-delta', ({ payload }) => deltas.push(payload));
    await flush();
    source.advance(Math.min(records.length, INCREMENTAL_LIMIT + 50));
    expect(deltas.length).toBe(0);
  });

  test('reads come from the fold and writes never reach a real command', async () => {
    const source = new ReplaySource(buildIndex(records, 50), backend());
    source.seek(lastWith('live-timeline-update'));
    expect(plain(await source.invoke('get_live_timeline'))).toEqual(plain(fold(records, source.n).timeline));
    expect(await source.invoke('set_live_transcription_enabled', { enabled: false })).toBeNull();
    for (const command of ['start_recording', 'stop_recording', 'save_transcript', 'api_save_meeting', 'copilot_shorter']) {
      await expect(source.invoke(command)).rejects.toThrow();
    }
  });

  test('a live ask goes to the backend at the cursor and streams into the panel', async () => {
    const fake = backend();
    const source = new ReplaySource(buildIndex(records, 50), fake);
    const n = lastWith('transcript-update');
    source.seek(n);
    let paused = 0;
    source.onLiveAsk = () => { paused += 1; };
    const seen = await mount(source);
    const id = await source.invoke<string>('copilot_ask', { action: null, question: '  What did they ask?  ' });
    expect(paused).toBe(1);
    expect(fake.asks).toHaveLength(1);
    expect(fake.asks[0].id).toBe(id);
    expect(fake.asks[0].request).toEqual({ audio_ms: records[n - 1].audio_ms, seq: (records[n - 1].seq ?? n - 1) + 1, kind: 'ask', text: 'What did they ask?' });
    expect(seen.copilot?.active).toBe(true);
    expect(seen.copilot?.messages.at(-1)).toMatchObject({ id, prompt: 'What did they ask?', status: 'pending' });

    source.handleLiveEvent(id, 'start', { provider: 'builtin-ai', model: 'm' });
    source.handleLiveEvent(id, 'delta', { offset: 0, text: 'They asked ' });
    source.handleLiveEvent(id, 'delta', { offset: 11, text: 'about [00:05].' });
    expect(seen.copilot?.messages.at(-1)).toMatchObject({ status: 'streaming', reply: 'They asked about [00:05].' });
    source.handleLiveEvent(id, 'done', { status: 'done', reply: 'They asked about [00:05].', stats: { firstTokenMs: 900 } });
    expect(seen.copilot?.messages.at(-1)).toMatchObject({ status: 'done', stats: { firstTokenMs: 900 } });
    expect(source.getView().live[0].phase).toBe('done');

    // Seeking keeps this session's asks after the recorded messages.
    source.seek(n - 5);
    expect(seen.copilot?.messages.at(-1)?.id).toBe(id);

    await source.invoke('copilot_stop', { id });
    expect(fake.stops()).toBe(1);
    await source.invoke('copilot_stop', { id: 'recorded' });
    expect(fake.stops()).toBe(1);
  });
});
