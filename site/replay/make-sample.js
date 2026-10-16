// Generates the viewer's demo run from a made-up meeting (a product launch
// sync). Nothing here comes from a real recording.
//
//   node site/replay/make-sample.js
//
// Writes sample-events.jsonl and sample-data.js (the same events embedded,
// so "Load sample" also works when the page is opened from disk).
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const LINES = [
  [4, 'mic', "Okay, let's get started. This is the launch sync for Lumen."],
  [11, 'system', 'Morning. Engineering is mostly done, one blocker left on offline sync.'],
  [19, 'system', 'Design still owes the onboarding screens, waiting on copy.'],
  [27, 'mic', 'How long does the sync fix take, days or a week?'],
  [34, 'system', 'About three days with review. Ravi has a branch already.'],
  [43, 'mic', "I'll chase the copy today and get it to you by Thursday."],
  [52, 'system', 'If copy lands Thursday we can have screens Monday.'],
  [61, 'mic', 'So the launch moves from the third to the tenth.'],
  [69, 'system', 'The tenth works for us. Support wants a week of notice.'],
  [78, 'system', "Agreed, the tenth. Let's lock it."],
  [86, 'mic', "Great. I'll send the announcement to support today."],
  [97, 'system', 'Pricing next. Free tier at launch or hold it back?'],
  [106, 'mic', "I'd hold it back a month, metering isn't built yet."],
  [117, 'system', 'Makes sense, metering is two sprints out anyway.'],
  [126, 'system', 'Can you put together a quick model of paid only versus freemium?'],
  [137, 'mic', "Yes, I'll ask Priya, she built the last one."],
  [149, 'system', 'Beta feedback: about two hundred responses.'],
  [158, 'system', 'Biggest complaint is battery drain during background sync.'],
  [168, 'mic', 'Same root cause as the sync bug?'],
  [176, 'system', 'Probably, the retry loop wakes up too often.'],
  [186, 'system', 'Second complaint: people cannot find the export button.'],
  [195, 'system', 'We could move it into the share menu, half a day of work.'],
  [204, 'mic', "Let's do it. Okay, I think that's everything."],
  [212, 'system', 'Right? Thanks all.'],
];

const records = [];
let seq = 0;
const push = (t, audio, type, name, payload) => {
  records.push({ seq: seq++, t_ms: Math.round(t), audio_ms: Math.round(audio), type, name, payload });
};

const DURATION_S = 220;
push(0, 0, 'meta', 'run-start', {
  schema_version: 1,
  audio_name: 'sample (synthetic product-launch meeting)',
  duration_s: DURATION_S,
  fed_s: DURATION_S,
  speed: 1,
  live_engine: 'nemotron-streaming / nemotron-speech-streaming-en-0.6b-560ms',
  summary_model: 'builtin-ai / qwen3.5:4b',
  limitations: ['Synthetic demo data: no real meeting.'],
});
push(10, 0, 'debug', 'phase', { phase: 'live-start', streaming: true });

const session = 'recording-sample';
let helperId = 0;
let sequence = 0;
const copilot = { sessionId: session, active: true, lineCount: 0, messages: [], suggestionsEnabled: true, debugEnabled: true, debug: { last: null, detector: { rulePass: 0, ruleReject: 0, confirmYes: 0, confirmNo: 0, confirmUnknown: 0, skipped: 0, falseAlarms: 0, last: null }, liveBacklogSecs: 0.2 } };
// Messages carry `_t` (when they appear) so a snapshot never shows the future.
const snap = (t, audio) => {
  const payload = JSON.parse(JSON.stringify(copilot));
  payload.messages = payload.messages.filter((m) => (m._t ?? 0) <= t).map(({ _t, ...m }) => m);
  push(t, audio, 'event', 'copilot-state', payload);
};
const stream = (t0, audio, id, text, stats) => {
  const message = copilot.messages.find((m) => m.id === id);
  push(t0 - stats.firstTokenMs, audio, 'debug', 'copilot-request', { id, phase: 'start' });
  push(t0 - stats.firstTokenMs + 2, audio, 'debug', 'sidecar', { phase: 'queued', id: ++helperId, priority: message.kind === 'suggestion' ? 'Normal' : 'Interactive' });
  push(t0 + 1, audio, 'debug', 'copilot-request', { id, phase: 'first-token', first_token_ms: stats.firstTokenMs });
  let offset = 0;
  const words = text.match(/\S+\s*/g) || [];
  words.forEach((word, i) => {
    push(t0 + i * 90, audio, 'event', 'copilot-delta', { sessionId: session, id, offset, delta: word });
    offset += Buffer.byteLength(word);
  });
  message.reply = text;
  message.status = 'done';
  message.stats = stats;
  copilot.debug.last = stats;
  snap(t0 + words.length * 90 + 20, audio);
  push(t0 + words.length * 90 + 10, audio, 'debug', 'sidecar', { phase: 'done', id: helperId, ok: true, busy_ms: stats.totalMs });
  push(t0 + words.length * 90 + 120, audio, 'debug', 'copilot-request', { id, phase: 'done', kind: message.kind, status: 'done', error: null, stats });
};

const entries = [];
const timeline = (t, audio, status) => push(t, audio, 'event', 'live-timeline-update', { session_id: session, meeting_id: 'sample', status, error: null, entries: JSON.parse(JSON.stringify(entries)) });

LINES.forEach(([start, source, text], index) => {
  const end = start + Math.max(2, text.split(' ').length * 0.35);
  const tFinal = (end + 0.9) * 1000;
  // Partials while the line is spoken.
  const words = text.split(' ');
  for (let w = 2; w < words.length; w += 3) {
    const at = (start + (end - start) * (w / words.length)) * 1000 + 300;
    push(at, at - 300, 'event', 'transcript-partial', { source, text: words.slice(0, w).join(' '), audio_start_time: start });
  }
  push(tFinal, end * 1000, 'event', 'transcript-update', {
    text, timestamp: '', source, sequence_id: ++sequence, chunk_start_time: start, is_partial: false,
    confidence: 0.9, audio_start_time: start, audio_end_time: end, duration: end - start,
  });
  push(tFinal + 1, end * 1000, 'event', 'transcript-partial', { source, text: '', audio_start_time: start });
  if (index === 0) {
    copilot.lineCount = 1;
    snap(tFinal + 5, end * 1000);
  }
  if (source === 'system') {
    const d = copilot.debug.detector;
    const question = /\?\s*$/.test(text) || /^can you/i.test(text);
    if (!question) {
      d.ruleReject += 1;
      d.last = 'rule: no (statement)';
      push(tFinal + 30, end * 1000, 'debug', 'detector', { decision_id: `line-${start * 1000}`, step: 'rule_reject', decision: d.last, line_ms: start * 1000 });
    } else if (text === 'Right? Thanks all.') {
      d.ruleReject += 1;
      d.last = 'rule: no (tag question)';
      push(tFinal + 30, end * 1000, 'debug', 'detector', { decision_id: `line-${start * 1000}`, step: 'rule_reject', decision: d.last, line_ms: start * 1000 });
    } else {
      d.rulePass += 1;
      d.last = 'rule: yes (question mark), confirming';
      push(tFinal + 30, end * 1000, 'debug', 'detector', { decision_id: `line-${start * 1000}`, step: 'confirming', decision: d.last, line_ms: start * 1000 });
      const yes = /^can you/i.test(text);
      if (yes) d.confirmYes += 1; else d.confirmNo += 1;
      d.last = `model: ${yes ? 'yes' : 'no'}`;
      push(tFinal + 850, end * 1000, 'debug', 'detector', { decision_id: `line-${start * 1000}`, step: yes ? 'confirm_yes' : 'confirm_no', decision: d.last, line_ms: start * 1000 });
      if (yes) {
        push(tFinal + 880, end * 1000, 'debug', 'card', { decision_id: `line-${start * 1000}`, message_id: 'card-1', line_ms: start * 1000 });
        copilot.messages.push({ _t: tFinal + 900, id: 'card-1', kind: 'suggestion', action: null, prompt: text, audioMs: start * 1000, reply: '', status: 'pending', error: null, canUseBuiltin: false, cardState: 'active', provider: 'builtin-ai', model: 'qwen3.5:4b', stats: null, createdAt: '' });
        snap(tFinal + 900, end * 1000);
        stream(tFinal + 1700, end * 1000, 'card-1', "Sure — I'll ask Priya for a paid-only vs freemium model and share it by end of next week [02:06].", { firstTokenMs: 780, totalMs: 2400, tokensPerSecond: 21.4, promptTokens: 1420, reusedTokens: 1180 });
      }
    }
  }
  if ([9, 15, 22].includes(index)) {
    timeline(tFinal + 200, end * 1000, 'generating');
    push(tFinal + 190, end * 1000, 'debug', 'timeline-request', { id: index, phase: 'start' });
    push(tFinal + 3090, end * 1000, 'debug', 'timeline-request', { id: index, phase: 'done', outcome: 'done' });
    if (index === 9) entries.push({ id: 't1', start_ms: 61000, end_ms: 82000, kind: 'decision', text: 'Launch moves from the third to the tenth', speaker: 'Others' });
    if (index === 15) {
      entries.push({ id: 't2', start_ms: 86000, end_ms: 95000, kind: 'action', text: 'You send the announcement to support today', speaker: 'You' });
      entries.push({ id: 't3', start_ms: 106000, end_ms: 120000, kind: 'point', text: 'Free tier held back a month until metering exists', speaker: null });
    }
    if (index === 22) entries.push({ id: 't4', start_ms: 195000, end_ms: 204000, kind: 'action', text: 'Move export into the share menu', speaker: null });
    timeline(tFinal + 3100, end * 1000, 'listening');
  }
});

// Scripted asks: an honesty probe, a quick action, and one that errors.
push(60000, 59000, 'debug', 'script', { at_ms: 59000, kind: 'ask', tag: 'honesty', text: 'What did they say about the Mars office budget?', message_id: 'ask-1', error: null });
copilot.messages.splice(0, 0, { _t: 60020, id: 'ask-1', kind: 'ask', action: null, prompt: 'What did they say about the Mars office budget?', audioMs: 59000, reply: '', status: 'pending', error: null, canUseBuiltin: false, cardState: null, provider: 'builtin-ai', model: 'qwen3.5:4b', stats: null, createdAt: '' });
snap(60020, 59000);
stream(61100, 59000, 'ask-1', "That hasn't come up in the meeting so far.", { firstTokenMs: 1080, totalMs: 1900, tokensPerSecond: 19.8, promptTokens: 980, reusedTokens: 0 });

push(150000, 149000, 'debug', 'script', { at_ms: 149000, kind: 'action', tag: 'what-to-say', text: 'what_to_say', message_id: 'ask-2', error: null });
copilot.messages.push({ _t: 150020, id: 'ask-2', kind: 'action', action: 'what_to_say', prompt: 'What should I say?', audioMs: 149000, reply: '', status: 'pending', error: null, canUseBuiltin: false, cardState: null, provider: 'builtin-ai', model: 'qwen3.5:4b', stats: null, createdAt: '' });
snap(150020, 149000);
stream(150900, 149000, 'ask-2', '- Confirm the launch date: **the tenth** [01:18].\n- Offer to own the pricing model with Priya [02:17].\n- Ask whether the battery drain shares the sync bug\'s root cause [02:48].', { firstTokenMs: 880, totalMs: 3100, tokensPerSecond: 22.6, promptTokens: 1510, reusedTokens: 1240 });
push(151000, 150000, 'debug', 'sidecar', { phase: 'preempt', id: 14, by: 2 });
push(151020, 150000, 'debug', 'sidecar', { what: 'timeline-preempted', message: 'Live summary: Copilot needed the model, retrying this window' });

push(190000, 189000, 'debug', 'script', { at_ms: 189000, kind: 'ask', tag: null, text: 'Who owns the export change?', message_id: 'ask-3', error: null });
copilot.messages.push({ _t: 190010, id: 'ask-3', kind: 'ask', action: null, prompt: 'Who owns the export change?', audioMs: 189000, reply: '', status: 'error', error: 'Copilot timed out waiting for the model (15 s).', canUseBuiltin: false, cardState: null, provider: 'builtin-ai', model: 'qwen3.5:4b', stats: null, createdAt: '' });
snap(205000, 204000);
push(190010, 189000, 'debug', 'copilot-request', { id: 'ask-3', phase: 'start' });
push(205050, 204000, 'debug', 'copilot-request', { id: 'ask-3', phase: 'done', kind: 'ask', status: 'error', error: 'Copilot timed out waiting for the model (15 s).' });
push(205100, 204000, 'debug', 'log', { level: 'WARN', target: 'app_lib::copilot::core', message: 'Copilot request timed out' });

// Health samples.
for (let t = 1; t <= DURATION_S; t += 1) {
  const spike = t > 150 && t < 158 ? 1.6 : 0;
  push(t * 1000 + 3, t * 1000, 'debug', 'live', {
    backlog_s: 0.25 + 0.15 * Math.sin(t / 7) + spike,
    rtf: t < 11 ? null : 0.55 + 0.08 * Math.sin(t / 13) + (spike ? 0.3 : 0),
    rtf_total: 0.57,
    llm_busy: (t > 59 && t < 62) || (t > 150 && t < 154),
    busy_s: t * 0.57,
    decoded_audio_s: t,
  });
  if (t % 5 === 0) push(t * 1000 + 4, t * 1000, 'debug', 'memory', { app_rss_mb: 1480 + (t % 30), helper_rss_mb: t > 55 ? 3350 : 0 });
}
push(56000, 55000, 'debug', 'sidecar', { what: 'spawn', message: 'Spawning llama-helper sidecar' });

const endT = DURATION_S * 1000;
push(endT + 100, endT, 'debug', 'phase', { phase: 'audio-end' });
push(endT + 120, endT, 'debug', 'phase', { phase: 'stop' });
copilot.active = false;
snap(endT + 150, endT);
timeline(endT + 2000, endT, 'stopped');
push(endT + 3000, endT, 'debug', 'phase', { phase: 'stopped' });
push(endT + 6000, endT, 'debug', 'phase', { phase: 'final-pass-start' });
push(endT + 60000, endT, 'event', 'final-transcript-pass-complete', { meeting_id: 'sample', task_id: 'task-1', outcome: 'replaced', provider: 'whisper', model: 'large-v3-turbo-q5_0', warning: null });
push(endT + 60010, endT, 'debug', 'phase', { phase: 'final-pass-done' });
push(endT + 60020, endT, 'debug', 'final-transcript', {
  lines: LINES.map(([start, source, text]) => ({ start, end: start + Math.max(2, text.split(' ').length * 0.35), speaker: source === 'mic' ? 'You' : 'Speaker 1', text })),
});
const summary = '# Lumen launch sync\n\n## Summary\nThe launch moves to the tenth; the free tier waits a month.\n\n## Decisions\n- Launch on the tenth\n\n## Action items\n- You: announce to support today\n- Priya: pricing model by end of next week\n';
push(endT + 61000, endT, 'debug', 'phase', { phase: 'summary-start', run: 1 });
push(endT + 95000, endT, 'debug', 'summary', { run: 1, status: 'completed', markdown: summary, error: null, ms: 34000 });
push(endT + 95010, endT, 'debug', 'phase', { phase: 'summary-start', run: 2 });
push(endT + 128000, endT, 'debug', 'summary', { run: 2, status: 'completed', markdown: summary, error: null, ms: 33000 });
push(endT + 128100, endT, 'debug', 'phase', { phase: 'run-end' });
push(endT + 128200, endT, 'meta', 'run-end', {});

records.sort((a, b) => a.t_ms - b.t_ms || a.seq - b.seq);
records.forEach((r, i) => { r.seq = i; });
const jsonl = records.map((r) => JSON.stringify(r)).join('\n') + '\n';
fs.writeFileSync(path.join(__dirname, 'sample-events.jsonl'), jsonl);
let report = null;
try {
  report = JSON.parse(fs.readFileSync(path.join(__dirname, 'sample-report.json'), 'utf8'));
} catch (_) {
  // Made by `replay --rescore site/replay/sample-events.jsonl`.
}
fs.writeFileSync(
  path.join(__dirname, 'sample-data.js'),
  '// Generated by make-sample.js from synthetic data. Do not edit.\n'
    + `window.REPLAY_SAMPLE = ${JSON.stringify({ events: jsonl, report })};\n`,
);
console.log(`wrote ${records.length} records`);
