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
