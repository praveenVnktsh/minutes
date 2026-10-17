// Run: node --test site/replay/parser.test.js
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const R = require('./parser.js');

const rec = (seq, t, type, name, payload, audio = t) => JSON.stringify({ seq, t_ms: t, audio_ms: audio, type, name, payload });

const body = [
  rec(0, 0, 'meta', 'run-start', { schema_version: 1, audio_name: 'audio.mp4', duration_s: 60 }),
  rec(1, 500, 'event', 'transcript-partial', { source: 'system', text: 'Can you', audio_start_time: 1 }),
  rec(2, 900, 'event', 'transcript-update', { text: 'Can you send the deck?', source: 'system', is_partial: false, audio_start_time: 1, audio_end_time: 3, sequence_id: 1 }),
  rec(3, 901, 'event', 'transcript-partial', { source: 'system', text: '', audio_start_time: 1 }),
  rec(4, 950, 'debug', 'detector', { decision_id: 'line-1000', step: 'confirming', decision: 'rule: yes (question mark), confirming', line_ms: 1000 }),
  rec(5, 990, 'debug', 'detector', { decision_id: 'line-1000', step: 'confirm_yes', decision: 'model: yes', line_ms: 1000 }),
  rec(6, 995, 'debug', 'card', { decision_id: 'line-1000', message_id: 'c1', line_ms: 1000 }),
  rec(7, 996, 'debug', 'copilot-request', { id: 'c1', phase: 'start' }),
  rec(8, 1000, 'event', 'copilot-state', { sessionId: 's', active: true, lineCount: 1, messages: [{ id: 'c1', kind: 'suggestion', audioMs: 1000, prompt: 'Can you send the deck?', reply: '', status: 'pending', cardState: 'active' }] }),
  rec(9, 1100, 'event', 'copilot-delta', { sessionId: 's', id: 'c1', offset: 0, delta: 'Sure, ' }),
  rec(10, 1101, 'debug', 'copilot-request', { id: 'c1', phase: 'first-token', first_token_ms: 105 }),
  rec(11, 1200, 'event', 'copilot-delta', { sessionId: 's', id: 'c1', offset: 6, delta: 'today [00:01].' }),
  rec(12, 1250, 'event', 'copilot-delta', { sessionId: 's', id: 'c1', offset: 3, delta: 'dup' }),
  rec(13, 1300, 'debug', 'copilot-request', { id: 'c1', phase: 'done', status: 'done', kind: 'suggestion', stats: { firstTokenMs: 105 } }),
  rec(14, 2000, 'event', 'live-timeline-update', { status: 'listening', entries: [{ id: 'e1', start_ms: 1000, end_ms: 3000, kind: 'decision', text: 'Send the deck today' }] }),
  rec(15, 2100, 'debug', 'live', { backlog_s: 0.3, rtf: 0.42, llm_busy: true }),
  rec(16, 2200, 'debug', 'sidecar', { phase: 'queued', id: 4, priority: 'Background' }),
  rec(17, 2300, 'debug', 'sidecar', { phase: 'preempt', id: 4, by: 5 }),
  rec(18, 2400, 'debug', 'sidecar', { phase: 'done', id: 4, ok: true, busy_ms: 200 }),
  rec(19, 3000, 'debug', 'phase', { phase: 'summary-start', run: 1 }),
  rec(20, 4000, 'debug', 'summary', { run: 1, status: 'completed', markdown: '# A' }),
  rec(21, 4100, 'debug', 'phase', { phase: 'summary-start', run: 2 }),
  rec(22, 5000, 'debug', 'summary', { run: 2, status: 'completed', markdown: '# B' }),
  '',
].join('\n');

test('parses records and reports bad lines by number', () => {
  const records = R.parseJsonl(body);
  assert.equal(records.length, 23);
  assert.equal(records[2].name, 'transcript-update');
  assert.equal(records[2].i, 2);
  assert.throws(() => R.parseJsonl(`${rec(0, 0, 'meta', 'x', {})}\n{oops`), /line 2/);
});

test('folding to a time gives the recording screen at that time', () => {
  const records = R.parseJsonl(body);
  let state = R.fold(records, R.countAtTime(records, 600));
  assert.equal(state.lines.length, 0);
  assert.equal(state.partials.system.text, 'Can you');
  state = R.fold(records, R.countAtTime(records, 1250));
  assert.equal(state.lines.length, 1);
  assert.equal(state.partials.system, undefined, 'an empty partial clears it');
  const card = state.copilot.messages[0];
  assert.equal(card.reply, 'Sure, today [00:01].', 'deltas apply at their offset, duplicates are skipped');
  assert.equal(card.status, 'streaming');
  assert.ok(R.isQuestionLine(R.questionLineStarts(state.copilot), state.lines[0].start, 'system'));
  assert.ok(!R.isQuestionLine(R.questionLineStarts(state.copilot), state.lines[0].start, 'mic'));
  const end = R.fold(records, records.length);
  assert.equal(end.timeline.entries[0].kind, 'decision');
  assert.deepEqual(end.summaries.map((s) => s.markdown), ['# A', '# B']);
});

test('seeking anywhere, in any order, equals the fold from the start', () => {
  const records = R.parseJsonl(body);
  const index = R.buildIndex(records, 4); // small snapshots to exercise them
  const order = [23, 0, 17, 3, 9, 9, 22, 1, 12, 5, 23, 8];
  let previous = null;
  for (const n of order) {
    const state = R.stateAt(index, n, previous);
    assert.deepEqual(JSON.parse(JSON.stringify(state)), JSON.parse(JSON.stringify(R.fold(records, n))), `n=${n}`);
    previous = state;
  }
  // Snapshots are never mutated by seeking.
  const again = R.stateAt(index, 8);
  assert.deepEqual(JSON.parse(JSON.stringify(again)), JSON.parse(JSON.stringify(R.fold(records, 8))));
});

test('a live run appended in pieces folds like the whole file', () => {
  const whole = R.parseJsonl(body);
  const lines = body.split('\n');
  const index = R.buildIndex([], 4);
  R.extendIndex(index, lines.slice(0, 7).map(R.parseLine).filter(Boolean));
  const midway = R.stateAt(index, index.records.length);
  assert.deepEqual(JSON.parse(JSON.stringify(midway)), JSON.parse(JSON.stringify(R.fold(whole, 7))));
  R.extendIndex(index, lines.slice(7).map(R.parseLine).filter(Boolean));
  assert.equal(index.records.length, whole.length);
  assert.deepEqual(
    JSON.parse(JSON.stringify(R.stateAt(index, whole.length, midway))),
    JSON.parse(JSON.stringify(R.fold(whole, whole.length))),
  );
});

test('meeting-time and wall-time lookups', () => {
  const records = R.parseJsonl(body);
  assert.equal(R.countAtTime(records, 999), 8);
  assert.equal(R.countAtAudio(records, 999), 8);
  assert.equal(R.countAtTime(records, -1), 0);
  assert.equal(R.countAtTime(records, 1e9), records.length);
});

test('lanes link a request from start to done and mark differing summaries', () => {
  const records = R.parseJsonl(body);
  const { lanes, series } = R.buildLanes(records);
  assert.equal(lanes.others.length, 1);
  assert.deepEqual(lanes.others[0].audio, [1000, 3000]);
  const request = lanes.copilot[0];
  assert.deepEqual(request.wall, [996, 1300]);
  assert.equal(request.first.wall, 1101);
  assert.equal(request.tone, 'warning', 'a suggestion draft');
  assert.ok(lanes.detector.some((d) => d.link === 'line-1000' && d.big), 'the card links to its decision');
  assert.equal(lanes.sidecar.filter((s) => s.bar).length, 1);
  assert.ok(lanes.sidecar.some((s) => s.label.startsWith('preempt')));
  assert.equal(lanes.summary[0].tone, 'success');
  assert.deepEqual(lanes.summaries.map((s) => s.differs), [false, true]);
  assert.deepEqual(lanes.summaries[1].wall, [4100, 5000]);
  assert.equal(series[0].llm, true);
});

test('summarize and next-of-name', () => {
  const records = R.parseJsonl(body);
  const s = R.summarize(records);
  assert.equal(s.meta.audio_name, 'audio.mp4');
  assert.equal(s.detector.length, 2);
  assert.equal(s.requests[0].id, 'c1');
  assert.ok(s.names.includes('copilot-delta'));
  assert.equal(R.nextOfName(records, 0, 'copilot-delta', 1), 10);
  assert.equal(R.nextOfName(records, 10, 'copilot-delta', 1), 12);
  assert.equal(R.nextOfName(records, 12, 'copilot-delta', -1), 10);
  assert.equal(R.nextOfName(records, 10, 'copilot-delta', -1), null);
});

test('clock matches the app format', () => {
  assert.equal(R.clock(65_900), '01:05');
  assert.equal(R.clock(3_723_000), '1:02:03');
});

test('the committed sample parses and plays to the end', () => {
  const text = fs.readFileSync(path.join(__dirname, 'sample-events.jsonl'), 'utf8');
  const records = R.parseJsonl(text);
  const index = R.buildIndex(records);
  const state = R.stateAt(index, records.length);
  assert.ok(state.lines.length > 10);
  assert.ok(state.copilot.messages.length >= 3);
  const { lanes } = R.buildLanes(records);
  assert.ok(lanes.copilot.length >= 3);
  assert.ok(lanes.detector.some((d) => d.big));
});

test('the live Copilot cursor is the records before n', () => {
  const records = R.parseJsonl(body);
  assert.deepEqual(R.liveCursor(records, 0), { seq: 0, audio_ms: 0 });
  assert.deepEqual(R.liveCursor(records, 3), { seq: 3, audio_ms: 900 });
});

test('server-sent events are split into whole events', () => {
  const first = R.parseSse('event: start\ndata: {"a":1}\n\n: ping\n\nevent: delta\ndata: {"text":"Hi"}\n\nevent: del');
  assert.deepEqual(first.events, [
    { event: 'start', data: '{"a":1}' },
    { event: 'delta', data: '{"text":"Hi"}' },
  ]);
  assert.equal(first.rest, 'event: del');
  const second = R.parseSse(first.rest + 'ta\r\ndata: a\r\ndata: b\r\n\r\ndata: plain\n\n');
  assert.deepEqual(second.events, [{ event: 'delta', data: 'a\nb' }, { event: 'message', data: 'plain' }]);
  assert.equal(second.rest, '');
});

test('live asks are read from interactive.jsonl', () => {
  const asks = R.parseInteractive('{"prompt":"a","audio_ms":1}\n\n{"prompt":"b"}\n{"half');
  assert.deepEqual(asks.map((a) => a.prompt), ['a', 'b']);
});

test('the recording plays only on the meeting axis, at 4x or slower, inside the recording', () => {
  const base = { available: true, axis: 'audio', speed: 1, playing: true, cursorMs: 5000, durationMs: 60000 };
  assert.deepEqual(R.audioPlan(base), { play: true, rate: 1, reason: 'playing' });
  assert.deepEqual(R.audioPlan({ ...base, speed: 4 }), { play: true, rate: 4, reason: 'playing' });
  assert.equal(R.audioPlan({ ...base, speed: 16 }).reason, 'fast');
  assert.equal(R.audioPlan({ ...base, speed: 16 }).play, false);
  assert.equal(R.audioPlan({ ...base, axis: 'wall' }).reason, 'wall');
  assert.equal(R.audioPlan({ ...base, available: false }).reason, 'unavailable');
  assert.equal(R.audioPlan({ ...base, cursorMs: 60000 }).reason, 'ended');
  assert.deepEqual(R.audioPlan({ ...base, playing: false }), { play: false, rate: 1, reason: 'paused' });
  assert.equal(R.audioPlan({ ...base, durationMs: NaN }).play, true, 'duration not known yet');
});

test('the recording is re-seeked only past 250 ms of drift', () => {
  assert.equal(R.needsResync(10.2, 10000), false);
  assert.equal(R.needsResync(10.3, 10000), true);
  assert.equal(R.needsResync(9.74, 10000), true);
});
