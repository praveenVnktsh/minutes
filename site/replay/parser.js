/*
 * Replay viewer: parsing, the reducer and the timeline lanes.
 *
 * Everything on screen is derived from the recorder's events.jsonl (see
 * README.md). The UI state at a cursor is a pure fold of the records up to it:
 *
 *   stateAt(n) = records[0..n).reduce(reduce, initialState())
 *
 * computed from the nearest snapshot (taken every SNAPSHOT_EVERY records), so
 * seeking anywhere, backwards too, is exact and fast.
 *
 * `reduce` folds the recorded Tauri events into what the app's recording
 * screen keeps: transcript lines and partials (LiveTranscriptPanel), the live
 * summary snapshot (LiveSummaryPanel) and the Copilot snapshot with streamed
 * deltas applied (CopilotPanel; same rules as frontend/src/lib/copilot.ts).
 *
 * Plain script: works from file:// and in Node (`node --test site/replay/parser.test.js`).
 */
(function (root) {
  'use strict';

  const SNAPSHOT_EVERY = 400;

  function parseJsonl(text) {
    const records = [];
    const lines = String(text).split(/\r?\n/);
    for (let i = 0; i < lines.length; i += 1) {
      const line = lines[i].trim();
      if (!line) continue;
      let record;
      try {
        record = JSON.parse(line);
      } catch (error) {
        throw new Error(`events.jsonl line ${i + 1}: ${error.message}`);
      }
      if (typeof record !== 'object' || record === null || typeof record.name !== 'string') {
        throw new Error(`events.jsonl line ${i + 1}: not a replay record`);
      }
      record.t_ms = Number(record.t_ms) || 0;
      record.audio_ms = Number(record.audio_ms) || 0;
      record.type = record.type || 'event';
      record.payload = record.payload ?? null;
      records.push(record);
    }
    // Writes are ordered, but be safe: the fold relies on time order.
    records.sort((a, b) => a.t_ms - b.t_ms || (a.seq ?? 0) - (b.seq ?? 0));
    records.forEach((r, i) => { r.i = i; });
    return records;
  }

  /** "MM:SS" or "H:MM:SS" from milliseconds, as the app formats times. */
  function clock(ms) {
    const total = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    const pad = (n) => String(n).padStart(2, '0');
    return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
  }

  function byteLength(text) {
    return typeof TextEncoder !== 'undefined'
      ? new TextEncoder().encode(text).length
      : Buffer.byteLength(text, 'utf8');
  }

  function clone(value) {
    return value === undefined || value === null ? value : JSON.parse(JSON.stringify(value));
  }

  function initialState() {
    return {
      n: 0,
      tMs: 0,
      audioMs: 0,
      phase: 'idle',
      lines: [],
      partials: {},
      timeline: null,
      copilot: null,
      notices: [],
      progress: null,
      finalPass: null,
      finalLines: null,
      summaries: [],
    };
  }

  /** frontend/src/lib/copilot.ts applyDelta, on a snapshot the fold owns. */
  function applyDelta(snapshot, delta) {
    if (!snapshot || (delta.sessionId && snapshot.sessionId && snapshot.sessionId !== delta.sessionId)) return false;
    const message = (snapshot.messages || []).find((m) => m.id === delta.id);
    if (!message) return false;
    const have = byteLength(message.reply || '');
    if (typeof delta.offset === 'number' && delta.offset !== have) return false;
    message.reply = (message.reply || '') + (delta.delta || '');
    if (message.status === 'pending') message.status = 'streaming';
    return true;
  }

  /** Folds one record into `state`. The state passed in is owned by the fold
   * (a fresh clone or the previous step's result), never a snapshot. */
  function reduce(state, record) {
    state.n += 1;
    state.tMs = record.t_ms;
    state.audioMs = Math.max(state.audioMs, record.audio_ms || 0);
    const p = record.payload || {};
    if (record.type === 'debug') {
      switch (record.name) {
        case 'phase':
          state.phase = p.phase || state.phase;
          break;
        case 'summary':
          state.summaries.push({ run: p.run, status: p.status, markdown: p.markdown || '', ms: p.ms, error: p.error || null });
          break;
        case 'final-transcript':
          state.finalLines = clone(p.lines || []);
          break;
        default:
          break;
      }
      return state;
    }
    if (record.type !== 'event') return state;
    switch (record.name) {
      case 'transcript-update':
        if (!p.is_partial && String(p.text || '').trim()) {
          state.lines.push({
            id: `${p.sequence_id ?? state.lines.length}-${state.lines.length}`,
            text: p.text,
            source: p.source || 'system',
            start: Number(p.audio_start_time) || 0,
            end: Number(p.audio_end_time) || 0,
            i: record.i,
          });
        }
        break;
      case 'transcript-partial': {
        const source = p.source || 'system';
        if (String(p.text || '').trim()) state.partials[source] = { source, text: p.text, start: p.audio_start_time };
        else delete state.partials[source];
        break;
      }
      case 'live-timeline-update':
        state.timeline = clone(p);
        break;
      case 'copilot-state':
        state.copilot = clone(p);
        break;
      case 'copilot-delta':
        applyDelta(state.copilot, p);
        break;
      case 'transcription-error':
      case 'transcription-warning':
        state.notices.push({ i: record.i, text: p.userMessage || p.error || String(p) });
        break;
      case 'retranscription-progress':
      case 'transcription-queue-progress':
      case 'diarization-progress':
        state.progress = { name: record.name, payload: clone(p) };
        break;
      case 'final-transcript-pass-complete':
        state.finalPass = clone(p);
        break;
      default:
        break;
    }
    return state;
  }

  /** Folds all of `records` from scratch: the definition the index matches. */
  function fold(records, n) {
    const state = initialState();
    for (let i = 0; i < n; i += 1) reduce(state, records[i]);
    return state;
  }

  /** Snapshots every SNAPSHOT_EVERY records, for fast exact seeking. */
  function buildIndex(records, every = SNAPSHOT_EVERY) {
    const snapshots = [{ n: 0, state: initialState() }];
    const running = initialState();
    for (let i = 0; i < records.length; i += 1) {
      reduce(running, records[i]);
      if ((i + 1) % every === 0) snapshots.push({ n: i + 1, state: clone(running) });
    }
    return { records, snapshots, every };
  }

  /** The state after the first `n` records. `from` (a state this function
   * returned earlier) is reused when it lies before `n`. */
  function stateAt(index, n, from) {
    const { records, snapshots, every } = index;
    n = Math.max(0, Math.min(n, records.length));
    const snap = snapshots[Math.min(Math.floor(n / every), snapshots.length - 1)];
    let state;
    if (from && from.n <= n && from.n >= snap.n) state = from;
    else state = clone(snap.state);
    for (let i = state.n; i < n; i += 1) reduce(state, records[i]);
    return state;
  }

  /** How many records have t_ms ≤ tMs. */
  function countAtTime(records, tMs) {
    let lo = 0;
    let hi = records.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (records[mid].t_ms <= tMs) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  /** How many records have audio_ms ≤ audioMs (meeting time never goes back). */
  function countAtAudio(records, audioMs) {
    let lo = 0;
    let hi = records.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (records[mid].audio_ms <= audioMs) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  /** Start times (ms) of lines a suggestion card answers (lib/copilot.ts). */
  function questionLineStarts(snapshot) {
    if (!snapshot) return [];
    return (snapshot.messages || [])
      .filter((m) => m.kind === 'suggestion' && m.cardState !== 'not_for_me')
      .map((m) => m.audioMs);
  }

  function isQuestionLine(starts, startSeconds, source) {
    if (source !== 'system' || typeof startSeconds !== 'number') return false;
    const ms = Math.round(startSeconds * 1000);
    return starts.some((start) => Math.abs(start - ms) <= 50);
  }

  /*
   * Lanes. Every item has a span on both axes, `wall: [t0, t1]` (ms since the
   * replay started) and `audio: [a0, a1]` (meeting time), the record that
   * starts it (`i`) and a tone for its colour.
   */
  const LANES = [
    ['phase', 'Run'],
    ['you', 'You'],
    ['others', 'Others'],
    ['partial', 'Partials'],
    ['summary', 'Live summary'],
    ['timeline-request', 'Summary requests'],
    ['detector', 'Detector'],
    ['copilot', 'Copilot'],
    ['sidecar', 'llama-helper'],
    ['live', 'Backlog / RTF'],
    ['final', 'Final pass'],
    ['diarization', 'Diarization'],
    ['summaries', 'Summary runs'],
    ['log', 'Warnings'],
  ];

  const DETECTOR_TONES = {
    rule_reject: 'neutral',
    skipped: 'subtle',
    confirming: 'info',
    confirm_yes: 'warning',
    confirm_no: 'neutral',
    confirm_unknown: 'error',
    confirm_failed: 'error',
    confirm_timeout: 'error',
  };

  function buildLanes(records) {
    const lanes = Object.fromEntries(LANES.map(([id]) => [id, []]));
    const series = [];
    const open = { copilot: new Map(), timeline: new Map(), sidecar: new Map() };
    let finalStart = null;
    let summaryStart = null;
    const summaryRuns = [];
    const seenEntries = new Set();
    const end = records.length ? records[records.length - 1] : { t_ms: 0, audio_ms: 0 };
    const span = (r) => ({ wall: [r.t_ms, r.t_ms], audio: [r.audio_ms, r.audio_ms] });
    const from = (r0, r1) => ({ wall: [r0.t_ms, r1.t_ms], audio: [r0.audio_ms, r1.audio_ms] });

    for (const r of records) {
      const p = r.payload || {};
      if (r.type === 'event') {
        if (r.name === 'transcript-update' && !p.is_partial && String(p.text || '').trim()) {
          const a0 = (Number(p.audio_start_time) || 0) * 1000;
          const a1 = (Number(p.audio_end_time) || 0) * 1000;
          lanes[p.source === 'mic' ? 'you' : 'others'].push({
            i: r.i, wall: [r.t_ms, r.t_ms], audio: [a0, Math.max(a0, a1)], tone: p.source === 'mic' ? 'info' : 'success',
            label: `[${clock(a0)}] ${p.text}`,
          });
        } else if (r.name === 'transcript-partial' && String(p.text || '').trim()) {
          lanes.partial.push(Object.assign({ i: r.i, tone: 'subtle', label: `partial: ${p.text}` }, span(r)));
        } else if (r.name === 'live-timeline-update') {
          for (const e of p.entries || []) {
            if (seenEntries.has(e.id)) continue;
            seenEntries.add(e.id);
            lanes.summary.push({
              i: r.i, wall: [r.t_ms, r.t_ms], audio: [e.start_ms, Math.max(e.start_ms, e.end_ms)],
              tone: e.kind === 'decision' ? 'success' : e.kind === 'action' ? 'info' : 'neutral',
              label: `${e.kind}: ${e.text}`,
            });
          }
        } else if (r.name === 'retranscription-progress' || r.name === 'transcription-queue-progress') {
          lanes.final.push(Object.assign({ i: r.i, tone: 'info', label: `${r.name}: ${p.stage || p.status || ''} ${p.progress ?? ''}%` }, span(r)));
        } else if (r.name === 'diarization-progress' || r.name === 'diarization-complete' || r.name === 'diarization-status-changed') {
          lanes.diarization.push(Object.assign({ i: r.i, tone: r.name === 'diarization-complete' ? 'success' : 'info', label: `${r.name} ${p.stage || p.status || ''}` }, span(r)));
        } else if (r.name === 'final-transcript-pass-complete') {
          lanes.final.push(Object.assign({ i: r.i, tone: p.outcome === 'replaced' ? 'success' : 'warning', label: `final pass: ${p.outcome}` }, span(r)));
        } else if (r.name === 'transcription-error') {
          lanes.log.push(Object.assign({ i: r.i, tone: 'error', label: p.userMessage || p.error || 'transcription error' }, span(r)));
        }
        continue;
      }
      if (r.type !== 'debug') continue;
      switch (r.name) {
        case 'phase': {
          lanes.phase.push(Object.assign({ i: r.i, tone: 'subtle', label: `phase: ${p.phase}` }, span(r)));
          if (p.phase === 'final-pass-start') finalStart = r;
          if (p.phase === 'final-pass-done' && finalStart) {
            lanes.final.push(Object.assign({ i: finalStart.i, tone: 'info', bar: true, label: 'final pass (and diarization)' }, from(finalStart, r)));
          }
          if (p.phase === 'summary-start') summaryStart = r;
          break;
        }
        case 'summary': {
          summaryRuns.push(p.markdown || '');
          const differs = summaryRuns.length > 1 && summaryRuns[summaryRuns.length - 1] !== summaryRuns[0];
          const start = summaryStart || r;
          lanes.summaries.push(Object.assign({
            i: r.i, bar: true, tone: p.status !== 'completed' ? 'error' : differs ? 'warning' : 'success',
            label: `summary run ${p.run}: ${p.status}${differs ? ' (differs from run 1)' : ''}`, differs,
          }, from(start, r)));
          summaryStart = null;
          break;
        }
        case 'detector':
          lanes.detector.push(Object.assign({
            i: r.i, tone: DETECTOR_TONES[p.step || p.counter] || 'neutral',
            label: `${p.step || p.counter}: ${p.decision || ''} (line ${clock(p.line_ms)})`,
            link: p.decision_id || null,
          }, span(r)));
          break;
        case 'card':
          lanes.detector.push(Object.assign({ i: r.i, tone: 'warning', label: `card opened (${p.decision_id})`, link: p.decision_id, big: true }, span(r)));
          break;
        case 'copilot-request': {
          if (p.phase === 'start') open.copilot.set(p.id, { start: r, first: null });
          else if (p.phase === 'first-token') {
            const o = open.copilot.get(p.id);
            if (o) o.first = r;
          } else if (p.phase === 'done') {
            const o = open.copilot.get(p.id) || { start: r, first: null };
            open.copilot.delete(p.id);
            const tone = p.status === 'done' ? (p.kind === 'suggestion' ? 'warning' : 'info') : p.status === 'stopped' ? 'neutral' : 'error';
            lanes.copilot.push(Object.assign({
              i: o.start.i, bar: true, tone, id: p.id,
              first: o.first ? { wall: o.first.t_ms, audio: o.first.audio_ms } : null,
              label: `${p.kind || 'request'} ${p.status}${p.stats && p.stats.firstTokenMs != null ? ` · first words ${p.stats.firstTokenMs} ms` : ''}${p.error ? ` · ${p.error}` : ''}`,
            }, from(o.start, r)));
          }
          break;
        }
        case 'timeline-request': {
          if (p.phase === 'start') open.timeline.set(p.id, r);
          else if (p.phase === 'done') {
            const s = open.timeline.get(p.id) || r;
            open.timeline.delete(p.id);
            lanes['timeline-request'].push(Object.assign({
              i: s.i, bar: true, tone: p.outcome === 'done' ? 'success' : p.outcome === 'preempted' ? 'warning' : 'error',
              label: `summary window ${p.id}: ${p.outcome}`,
            }, from(s, r)));
          }
          break;
        }
        case 'sidecar': {
          const phase = p.phase || p.what;
          const key = `${p.id}`;
          if (phase === 'queued') open.sidecar.set(key, { queued: r, priority: p.priority });
          else if (phase === 'done' && open.sidecar.has(key)) {
            const o = open.sidecar.get(key);
            open.sidecar.delete(key);
            lanes.sidecar.push(Object.assign({
              i: o.queued.i, bar: true, tone: p.ok === false ? 'error' : o.priority === 'Interactive' ? 'info' : o.priority === 'Background' ? 'subtle' : 'neutral',
              label: `request ${p.id ?? '–'} (${o.priority || '?'}) ${p.ok === false ? 'failed' : 'done'} in ${p.busy_ms} ms`,
            }, from(o.queued, r)));
          } else if (phase !== 'acquired' && phase !== 'queued' && phase !== 'done') {
            lanes.sidecar.push(Object.assign({ i: r.i, tone: phase === 'preempt' || phase === 'timeline-preempted' ? 'warning' : phase === 'timeout' ? 'error' : 'subtle', label: `${phase}${p.id != null ? ` ${p.id}` : ''}${p.message ? `: ${p.message}` : ''}`, big: phase === 'preempt' }, span(r)));
          }
          break;
        }
        case 'live':
          series.push({ wall: r.t_ms, audio: r.audio_ms, backlog: Number(p.backlog_s) || 0, rtf: p.rtf == null ? null : Number(p.rtf), llm: !!p.llm_busy, i: r.i });
          break;
        case 'log':
          lanes.log.push(Object.assign({ i: r.i, tone: p.level === 'ERROR' ? 'error' : 'warning', label: `${p.level}: ${p.message}` }, span(r)));
          break;
        default:
          break;
      }
    }
    // Requests still open at the end ran past the recording.
    for (const [, o] of open.copilot) {
      lanes.copilot.push(Object.assign({ i: o.start.i, bar: true, tone: 'error', label: 'copilot request never finished' }, from(o.start, end)));
    }
    return { lanes, series, order: LANES };
  }

  /** Everything the header, scorecard and logs show, computed once. */
  function summarize(records) {
    const meta = (records.find((r) => r.type === 'meta' && r.name === 'run-start') || {}).payload || {};
    const last = records.length ? records[records.length - 1] : { t_ms: 0, audio_ms: 0 };
    const detector = [];
    const sidecar = [];
    const requests = [];
    for (const r of records) {
      const p = r.payload || {};
      if (r.type !== 'debug') continue;
      if (r.name === 'detector') detector.push({ i: r.i, tMs: r.t_ms, audioMs: r.audio_ms, step: p.step || p.counter, decision: p.decision, lineMs: p.line_ms, id: p.decision_id });
      if (r.name === 'sidecar') sidecar.push({ i: r.i, tMs: r.t_ms, audioMs: r.audio_ms, what: p.phase || p.what, id: p.id, priority: p.priority, detail: p.message || (p.waited_ms != null ? `waited ${p.waited_ms} ms` : p.busy_ms != null ? `${p.busy_ms} ms` : p.by != null ? `by ${p.by}` : '') });
      if (r.name === 'copilot-request' && p.phase === 'done') requests.push(Object.assign({ i: r.i, tMs: r.t_ms, audioMs: r.audio_ms }, p));
    }
    const names = [...new Set(records.map((r) => r.name))].sort();
    return { meta, endMs: last.t_ms, endAudioMs: last.audio_ms, detector, sidecar, requests, names };
  }

  /** The next/previous record (count) named `name` from count `n`. */
  function nextOfName(records, n, name, direction) {
    if (direction > 0) {
      for (let i = n; i < records.length; i += 1) if (records[i].name === name) return i + 1;
    } else {
      for (let i = n - 2; i >= 0; i -= 1) if (records[i].name === name) return i + 1;
    }
    return null;
  }

  const api = {
    SNAPSHOT_EVERY,
    parseJsonl,
    clock,
    applyDelta,
    initialState,
    reduce,
    fold,
    buildIndex,
    stateAt,
    countAtTime,
    countAtAudio,
    questionLineStarts,
    isQuestionLine,
    buildLanes,
    summarize,
    nextOfName,
    LANES,
  };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.ReplayParser = api;
})(typeof window !== 'undefined' ? window : globalThis);
