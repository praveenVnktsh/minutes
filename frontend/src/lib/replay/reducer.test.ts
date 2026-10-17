import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import * as R from './reducer';

/* eslint-disable @typescript-eslint/no-explicit-any */

const require = createRequire(import.meta.url);
const site = require('../../../../site/replay/parser.js');
const samplePath = fileURLToPath(new URL('../../../../site/replay/sample-events.jsonl', import.meta.url));
const sample = readFileSync(samplePath, 'utf8');

const plain = (value: unknown) => JSON.parse(JSON.stringify(value));

describe('the replay reducer matches site/replay/parser.js', () => {
  test('parsing, every fold step, lanes and the summary are the same', () => {
    const ours = R.parseJsonl(sample);
    const theirs = site.parseJsonl(sample);
    expect(ours.length).toBeGreaterThan(100);
    expect(plain(ours)).toEqual(plain(theirs));

    const a = R.initialState();
    const b = site.initialState();
    for (let i = 0; i < ours.length; i += 1) {
      R.reduce(a, ours[i]);
      site.reduce(b, theirs[i]);
      if (i % 25 === 0 || i === ours.length - 1) expect(plain(a)).toEqual(plain(b));
    }
    expect(plain(R.buildLanes(ours))).toEqual(plain(site.buildLanes(theirs)));
    expect(plain(R.summarize(ours))).toEqual(plain(site.summarize(theirs)));
    for (const n of [0, 1, 50, ours.length]) {
      expect(R.liveCursor(ours, n)).toEqual(site.liveCursor(theirs, n));
      expect(R.nextOfName(ours, n, 'copilot-state', 1)).toBe(site.nextOfName(theirs, n, 'copilot-state', 1));
      expect(R.nextOfName(ours, n, 'copilot-state', -1)).toBe(site.nextOfName(theirs, n, 'copilot-state', -1));
    }
    const end = ours[ours.length - 1];
    for (const ms of [0, 1000, end.audio_ms / 2, end.audio_ms]) {
      expect(R.countAtAudio(ours, ms)).toBe(site.countAtAudio(theirs, ms));
      expect(R.countAtTime(ours, ms)).toBe(site.countAtTime(theirs, ms));
    }
    expect(R.clock(3_725_000)).toBe(site.clock(3_725_000));
    expect(R.parseInteractive('{"a":1}\n\n{bad\n{"b":2}')).toEqual(site.parseInteractive('{"a":1}\n\n{bad\n{"b":2}'));
  });

  test('copilot deltas are applied at their byte offset, as lib/copilot.ts does', () => {
    const snapshot: any = { sessionId: 's', messages: [{ id: 'm', reply: 'é', status: 'pending' }] };
    expect(R.applyDelta(snapshot, { sessionId: 's', id: 'm', offset: 1, delta: 'x' })).toBe(false);
    expect(R.applyDelta(snapshot, { sessionId: 's', id: 'm', offset: 2, delta: 'x' })).toBe(true);
    expect(snapshot.messages[0]).toEqual({ id: 'm', reply: 'éx', status: 'streaming' });
    expect(R.applyDelta(snapshot, { sessionId: 'other', id: 'm', offset: 3, delta: 'y' })).toBe(false);
  });
});

/** A short run whose live summary entries carry topics, with a topic step
 * traced next to its window's request. */
const topicRun = [
  { seq: 0, t_ms: 0, audio_ms: 0, type: 'meta', name: 'run-start', payload: { schema_version: 1 } },
  { seq: 1, t_ms: 10, audio_ms: 60_000, type: 'debug', name: 'timeline-request', payload: { id: 2, phase: 'start', start_ms: 60_000, end_ms: 120_000 } },
  { seq: 2, t_ms: 11, audio_ms: 60_000, type: 'debug', name: 'timeline-request', payload: { id: '2-topic', phase: 'start', kind: 'topic' } },
  { seq: 3, t_ms: 900, audio_ms: 61_000, type: 'debug', name: 'timeline-request', payload: { id: '2-topic', phase: 'done', kind: 'topic', outcome: 'done', topic: 'Pricing', new_topic: true } },
  { seq: 4, t_ms: 4000, audio_ms: 64_000, type: 'debug', name: 'timeline-request', payload: { id: 2, phase: 'done', outcome: 'done' } },
  {
    seq: 5, t_ms: 4001, audio_ms: 64_000, type: 'event', name: 'live-timeline-update',
    payload: {
      session_id: 's', meeting_id: 'm', status: 'listening', error: null,
      entries: [
        { id: 'a', start_ms: 5_000, end_ms: 9_000, kind: 'point', text: 'Sync bug is the last blocker', speaker: null, provider: 'builtin-ai', model: 'm', created_at: '', topic: 'Launch Plan' },
        { id: 'b', start_ms: 62_000, end_ms: 70_000, kind: 'action', text: 'introduce You to the founders', speaker: 'Others', provider: 'builtin-ai', model: 'm', created_at: '', topic: 'Pricing' },
      ],
    },
  },
].map((r) => JSON.stringify(r)).join('\n');

describe('topics in a replay', () => {
  test('the reducer and site/replay keep every entry\'s topic, and trace the topic step', () => {
    const ours = R.parseJsonl(topicRun);
    const state = R.fold(ours, ours.length);
    expect(state.timeline.entries.map((e: any) => e.topic)).toEqual(['Launch Plan', 'Pricing']);
    expect(plain(state)).toEqual(plain(site.fold(site.parseJsonl(topicRun), ours.length)));
    const labels = R.buildLanes(ours).lanes['timeline-request'].map((l: any) => l.label);
    expect(labels).toContain('summary window 2-topic: done');
    expect(labels).toContain('summary window 2: done');
  });
});

describe('seeking is exact in both directions', () => {
  const records = R.parseJsonl(sample);
  const index = R.buildIndex(records, 37);

  test('any order of seeks equals the fold from the start', () => {
    let state: R.ReplayState | null = null;
    const order = [records.length, 3, 200, 199, 0, 450, 37, 38, 36, records.length - 1, 120, 5];
    for (const n of order) {
      state = R.stateAt(index, n, state);
      expect(plain(state)).toEqual(plain(R.fold(records, n)));
    }
  });

  test('seeking never changes the snapshots it starts from', () => {
    const before = plain(index.snapshots);
    let state: R.ReplayState | null = null;
    for (const n of [80, 10, 300, 75]) state = R.stateAt(index, n, state);
    expect(plain(index.snapshots)).toEqual(before);
  });
});
