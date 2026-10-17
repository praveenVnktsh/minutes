/*
 * The replay reducer: a TypeScript port of site/replay/parser.js (the static
 * viewer's), kept in step with it by a parity test (reducer.test.ts).
 *
 * Everything the replay shows is derived from the recorder's events.jsonl
 * (site/replay/README.md). The UI state at a cursor is a pure fold of the
 * records up to it:
 *
 *   stateAt(n) = records[0..n).reduce(reduce, initialState())
 *
 * computed from the nearest snapshot (taken every SNAPSHOT_EVERY records), so
 * seeking anywhere, backwards too, is exact and fast.
 */

/* eslint-disable @typescript-eslint/no-explicit-any */

export const SNAPSHOT_EVERY = 400;

export type RecordType = 'meta' | 'event' | 'debug';

export interface ReplayRecord {
  seq?: number;
  t_ms: number;
  audio_ms: number;
  type: RecordType | string;
  name: string;
  payload: any;
  /** Position in the run, set when parsed. */
  i: number;
}

export interface ReplayLine {
  id: string;
  text: string;
  source: string;
  start: number;
  end: number;
  i: number;
}

export interface ReplayPartial {
  source: string;
  text: string;
  start: number;
}

export interface ReplayState {
  n: number;
  tMs: number;
  audioMs: number;
  phase: string;
  lines: ReplayLine[];
  partials: Record<string, ReplayPartial>;
  timeline: any | null;
  copilot: any | null;
  notices: Array<{ i: number; text: string }>;
  progress: { name: string; payload: any } | null;
  finalPass: any | null;
  finalLines: any[] | null;
  summaries: Array<{ run: number; status: string; markdown: string; ms: number; error: string | null }>;
}

function normalize(record: any): ReplayRecord {
  record.t_ms = Number(record.t_ms) || 0;
  record.audio_ms = Number(record.audio_ms) || 0;
  record.type = record.type || 'event';
  record.payload = record.payload ?? null;
  return record as ReplayRecord;
}

export function parseJsonl(text: string): ReplayRecord[] {
  const records: ReplayRecord[] = [];
  const lines = String(text).split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i].trim();
    if (!line) continue;
    let record: any;
    try {
      record = JSON.parse(line);
    } catch (error) {
      throw new Error(`events.jsonl line ${i + 1}: ${(error as Error).message}`);
    }
    if (typeof record !== 'object' || record === null || typeof record.name !== 'string') {
      throw new Error(`events.jsonl line ${i + 1}: not a replay record`);
    }
    records.push(normalize(record));
  }
  // Writes are ordered, but be safe: the fold relies on time order.
  records.sort((a, b) => a.t_ms - b.t_ms || (a.seq ?? 0) - (b.seq ?? 0));
  records.forEach((r, i) => {
    r.i = i;
  });
  return records;
}

/** One events.jsonl line, as parseJsonl reads it (null for a blank line). */
export function parseLine(line: string): ReplayRecord | null {
  const text = String(line).trim();
  if (!text) return null;
  return normalize(JSON.parse(text));
}

/** "MM:SS" or "H:MM:SS" from milliseconds, as the app formats times. */
export function clock(ms: number): string {
  const total = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

const encoder = typeof TextEncoder !== 'undefined' ? new TextEncoder() : null;
function byteLength(text: string): number {
  return encoder ? encoder.encode(text).length : text.length;
}

function clone<T>(value: T): T {
  return value === undefined || value === null ? value : JSON.parse(JSON.stringify(value));
}

export function initialState(): ReplayState {
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
export function applyDelta(snapshot: any, delta: any): boolean {
  if (!snapshot || (delta.sessionId && snapshot.sessionId && snapshot.sessionId !== delta.sessionId)) return false;
  const message = (snapshot.messages || []).find((m: any) => m.id === delta.id);
  if (!message) return false;
  const have = byteLength(message.reply || '');
  if (typeof delta.offset === 'number' && delta.offset !== have) return false;
  message.reply = (message.reply || '') + (delta.delta || '');
  if (message.status === 'pending') message.status = 'streaming';
  return true;
}

/** Folds one record into `state`. The state passed in is owned by the fold
 * (a fresh clone or the previous step's result), never a snapshot. */
export function reduce(state: ReplayState, record: ReplayRecord): ReplayState {
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

/** Folds the first `n` records from scratch: the definition the index matches. */
export function fold(records: ReplayRecord[], n: number): ReplayState {
  const state = initialState();
  for (let i = 0; i < n; i += 1) reduce(state, records[i]);
  return state;
}

export interface ReplayIndex {
  records: ReplayRecord[];
  snapshots: Array<{ n: number; state: ReplayState }>;
  every: number;
  running: ReplayState;
}

/** Snapshots every SNAPSHOT_EVERY records, for fast exact seeking. */
export function buildIndex(records: ReplayRecord[], every = SNAPSHOT_EVERY): ReplayIndex {
  const index: ReplayIndex = { records: [], snapshots: [{ n: 0, state: initialState() }], every, running: initialState() };
  extendIndex(index, records);
  return index;
}

/** Appends records (a live run's new lines) to an index. They must not be
 * older than what it holds. */
export function extendIndex(index: ReplayIndex, more: ReplayRecord[]): ReplayIndex {
  for (const record of more) {
    record.i = index.records.length;
    index.records.push(record);
    reduce(index.running, record);
    if (index.records.length % index.every === 0) {
      index.snapshots.push({ n: index.records.length, state: clone(index.running) });
    }
  }
  return index;
}

/** The state after the first `n` records. `from` (a state this function
 * returned earlier) is reused, and advanced in place, when it lies before
 * `n` and after the nearest snapshot. */
export function stateAt(index: ReplayIndex, n: number, from?: ReplayState | null): ReplayState {
  const { records, snapshots, every } = index;
  n = Math.max(0, Math.min(n, records.length));
  const snap = snapshots[Math.min(Math.floor(n / every), snapshots.length - 1)];
  let state: ReplayState;
  if (from && from.n <= n && from.n >= snap.n) state = from;
  else state = clone(snap.state);
  for (let i = state.n; i < n; i += 1) reduce(state, records[i]);
  return state;
}

function upperBound(records: ReplayRecord[], value: number, key: 't_ms' | 'audio_ms'): number {
  let lo = 0;
  let hi = records.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (records[mid][key] <= value) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/** How many records have t_ms ≤ tMs. */
export function countAtTime(records: ReplayRecord[], tMs: number): number {
  return upperBound(records, tMs, 't_ms');
}

/** How many records have audio_ms ≤ audioMs (meeting time never goes back). */
export function countAtAudio(records: ReplayRecord[], audioMs: number): number {
  return upperBound(records, audioMs, 'audio_ms');
}

/*
 * Lanes. Every item has a span on both axes, `wall: [t0, t1]` (ms since the
 * replay started) and `audio: [a0, a1]` (meeting time), the record that
 * starts it (`i`) and a tone for its colour.
 */
export type LaneTone = 'neutral' | 'subtle' | 'info' | 'success' | 'warning' | 'error';

export interface LaneItem {
  i: number;
  wall: [number, number];
  audio: [number, number];
  tone: LaneTone;
  label: string;
  bar?: boolean;
  big?: boolean;
  differs?: boolean;
  link?: string | null;
  id?: string;
  first?: { wall: number; audio: number } | null;
}

export interface SeriesPoint {
  wall: number;
  audio: number;
  backlog: number;
  rtf: number | null;
  llm: boolean;
  i: number;
}

export const LANES: Array<[string, string]> = [
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

const DETECTOR_TONES: Record<string, LaneTone> = {
  rule_reject: 'neutral',
  skipped: 'subtle',
  confirming: 'info',
  confirm_yes: 'warning',
  confirm_no: 'neutral',
  confirm_unknown: 'error',
  confirm_failed: 'error',
  confirm_timeout: 'error',
};

export interface Lanes {
  lanes: Record<string, LaneItem[]>;
  series: SeriesPoint[];
  order: Array<[string, string]>;
}

export function buildLanes(records: ReplayRecord[]): Lanes {
  const lanes: Record<string, LaneItem[]> = Object.fromEntries(LANES.map(([id]) => [id, []]));
  const series: SeriesPoint[] = [];
  const open = {
    copilot: new Map<any, { start: ReplayRecord; first: ReplayRecord | null }>(),
    timeline: new Map<any, ReplayRecord>(),
    sidecar: new Map<string, { queued: ReplayRecord; priority: any }>(),
  };
  let finalStart: ReplayRecord | null = null;
  let summaryStart: ReplayRecord | null = null;
  const summaryRuns: string[] = [];
  const seenEntries = new Set<string>();
  const end = records.length ? records[records.length - 1] : ({ t_ms: 0, audio_ms: 0 } as ReplayRecord);
  const span = (r: ReplayRecord) => ({ wall: [r.t_ms, r.t_ms] as [number, number], audio: [r.audio_ms, r.audio_ms] as [number, number] });
  const from = (r0: ReplayRecord, r1: ReplayRecord) => ({ wall: [r0.t_ms, r1.t_ms] as [number, number], audio: [r0.audio_ms, r1.audio_ms] as [number, number] });

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
        lanes.partial.push({ i: r.i, tone: 'subtle', label: `partial: ${p.text}`, ...span(r) });
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
        lanes.final.push({ i: r.i, tone: 'info', label: `${r.name}: ${p.stage || p.status || ''} ${p.progress ?? ''}%`, ...span(r) });
      } else if (r.name === 'diarization-progress' || r.name === 'diarization-complete' || r.name === 'diarization-status-changed') {
        lanes.diarization.push({ i: r.i, tone: r.name === 'diarization-complete' ? 'success' : 'info', label: `${r.name} ${p.stage || p.status || ''}`, ...span(r) });
      } else if (r.name === 'final-transcript-pass-complete') {
        lanes.final.push({ i: r.i, tone: p.outcome === 'replaced' ? 'success' : 'warning', label: `final pass: ${p.outcome}`, ...span(r) });
      } else if (r.name === 'transcription-error') {
        lanes.log.push({ i: r.i, tone: 'error', label: p.userMessage || p.error || 'transcription error', ...span(r) });
      }
      continue;
    }
    if (r.type !== 'debug') continue;
    switch (r.name) {
      case 'phase': {
        lanes.phase.push({ i: r.i, tone: 'subtle', label: `phase: ${p.phase}`, ...span(r) });
        if (p.phase === 'final-pass-start') finalStart = r;
        if (p.phase === 'final-pass-done' && finalStart) {
          lanes.final.push({ i: finalStart.i, tone: 'info', bar: true, label: 'final pass (and diarization)', ...from(finalStart, r) });
        }
        if (p.phase === 'summary-start') summaryStart = r;
        break;
      }
      case 'summary': {
        summaryRuns.push(p.markdown || '');
        const differs = summaryRuns.length > 1 && summaryRuns[summaryRuns.length - 1] !== summaryRuns[0];
        const start = summaryStart || r;
        lanes.summaries.push({
          i: r.i, bar: true, tone: p.status !== 'completed' ? 'error' : differs ? 'warning' : 'success',
          label: `summary run ${p.run}: ${p.status}${differs ? ' (differs from run 1)' : ''}`, differs,
          ...from(start, r),
        });
        summaryStart = null;
        break;
      }
      case 'detector':
        lanes.detector.push({
          i: r.i, tone: DETECTOR_TONES[p.step || p.counter] || 'neutral',
          label: `${p.step || p.counter}: ${p.decision || ''} (line ${clock(p.line_ms)})`,
          link: p.decision_id || null,
          ...span(r),
        });
        break;
      case 'card':
        lanes.detector.push({ i: r.i, tone: 'warning', label: `card opened (${p.decision_id})`, link: p.decision_id, big: true, ...span(r) });
        break;
      case 'copilot-request': {
        if (p.phase === 'start') open.copilot.set(p.id, { start: r, first: null });
        else if (p.phase === 'first-token') {
          const o = open.copilot.get(p.id);
          if (o) o.first = r;
        } else if (p.phase === 'done') {
          const o = open.copilot.get(p.id) || { start: r, first: null };
          open.copilot.delete(p.id);
          const tone: LaneTone = p.status === 'done' ? (p.kind === 'suggestion' ? 'warning' : 'info') : p.status === 'stopped' ? 'neutral' : 'error';
          lanes.copilot.push({
            i: o.start.i, bar: true, tone, id: p.id,
            first: o.first ? { wall: o.first.t_ms, audio: o.first.audio_ms } : null,
            label: `${p.kind || 'request'} ${p.status}${p.stats && p.stats.firstTokenMs != null ? ` · first words ${p.stats.firstTokenMs} ms` : ''}${p.error ? ` · ${p.error}` : ''}`,
            ...from(o.start, r),
          });
        }
        break;
      }
      case 'timeline-request': {
        if (p.phase === 'start') open.timeline.set(p.id, r);
        else if (p.phase === 'done') {
          const s = open.timeline.get(p.id) || r;
          open.timeline.delete(p.id);
          lanes['timeline-request'].push({
            i: s.i, bar: true, tone: p.outcome === 'done' ? 'success' : p.outcome === 'preempted' ? 'warning' : 'error',
            label: `summary window ${p.id}: ${p.outcome}`,
            ...from(s, r),
          });
        }
        break;
      }
      case 'sidecar': {
        const phase = p.phase || p.what;
        const key = `${p.id}`;
        if (phase === 'queued') open.sidecar.set(key, { queued: r, priority: p.priority });
        else if (phase === 'done' && open.sidecar.has(key)) {
          const o = open.sidecar.get(key)!;
          open.sidecar.delete(key);
          lanes.sidecar.push({
            i: o.queued.i, bar: true, tone: p.ok === false ? 'error' : o.priority === 'Interactive' ? 'info' : o.priority === 'Background' ? 'subtle' : 'neutral',
            label: `request ${p.id ?? '–'} (${o.priority || '?'}) ${p.ok === false ? 'failed' : 'done'} in ${p.busy_ms} ms`,
            ...from(o.queued, r),
          });
        } else if (phase !== 'acquired' && phase !== 'queued' && phase !== 'done') {
          lanes.sidecar.push({
            i: r.i, tone: phase === 'preempt' || phase === 'timeline-preempted' ? 'warning' : phase === 'timeout' ? 'error' : 'subtle',
            label: `${phase}${p.id != null ? ` ${p.id}` : ''}${p.message ? `: ${p.message}` : ''}`, big: phase === 'preempt',
            ...span(r),
          });
        }
        break;
      }
      case 'live':
        series.push({ wall: r.t_ms, audio: r.audio_ms, backlog: Number(p.backlog_s) || 0, rtf: p.rtf == null ? null : Number(p.rtf), llm: !!p.llm_busy, i: r.i });
        break;
      case 'log':
        lanes.log.push({ i: r.i, tone: p.level === 'ERROR' ? 'error' : 'warning', label: `${p.level}: ${p.message}`, ...span(r) });
        break;
      default:
        break;
    }
  }
  // Requests still open at the end ran past the recording.
  for (const [, o] of open.copilot) {
    lanes.copilot.push({ i: o.start.i, bar: true, tone: 'error', label: 'copilot request never finished', ...from(o.start, end) });
  }
  return { lanes, series, order: LANES };
}

export interface DetectorRow { i: number; tMs: number; audioMs: number; step: string; decision: string; lineMs: number; id: string }
export interface SidecarRow { i: number; tMs: number; audioMs: number; what: string; id: any; priority: string; detail: string }
export interface RequestRow { i: number; tMs: number; audioMs: number; [key: string]: any }

export interface RunSummary {
  meta: any;
  endMs: number;
  endAudioMs: number;
  detector: DetectorRow[];
  sidecar: SidecarRow[];
  requests: RequestRow[];
  names: string[];
}

/** Everything the header, scorecard and logs show, computed once. */
export function summarize(records: ReplayRecord[]): RunSummary {
  const meta = (records.find((r) => r.type === 'meta' && r.name === 'run-start') || ({} as ReplayRecord)).payload || {};
  const last = records.length ? records[records.length - 1] : { t_ms: 0, audio_ms: 0 };
  const detector: DetectorRow[] = [];
  const sidecar: SidecarRow[] = [];
  const requests: RequestRow[] = [];
  for (const r of records) {
    const p = r.payload || {};
    if (r.type !== 'debug') continue;
    if (r.name === 'detector') detector.push({ i: r.i, tMs: r.t_ms, audioMs: r.audio_ms, step: p.step || p.counter, decision: p.decision, lineMs: p.line_ms, id: p.decision_id });
    if (r.name === 'sidecar') sidecar.push({ i: r.i, tMs: r.t_ms, audioMs: r.audio_ms, what: p.phase || p.what, id: p.id, priority: p.priority, detail: p.message || (p.waited_ms != null ? `waited ${p.waited_ms} ms` : p.busy_ms != null ? `${p.busy_ms} ms` : p.by != null ? `by ${p.by}` : '') });
    if (r.name === 'copilot-request' && p.phase === 'done') requests.push({ ...p, i: r.i, tMs: r.t_ms, audioMs: r.audio_ms });
  }
  const names = [...new Set(records.map((r) => r.name))].sort();
  return { meta, endMs: last.t_ms, endAudioMs: last.audio_ms, detector, sidecar, requests, names };
}

/** The next/previous record (count) named `name` from count `n`. */
export function nextOfName(records: ReplayRecord[], n: number, name: string, direction: number): number | null {
  if (direction > 0) {
    for (let i = n; i < records.length; i += 1) if (records[i].name === name) return i + 1;
  } else {
    for (let i = n - 2; i >= 0; i -= 1) if (records[i].name === name) return i + 1;
  }
  return null;
}

/** What the live Copilot needs to rebuild its inputs at cursor `n`: the
 * records before it (by `seq`) and its meeting time. */
export function liveCursor(records: ReplayRecord[], n: number): { seq: number; audio_ms: number } {
  const last = n > 0 ? records[n - 1] : null;
  return {
    seq: last ? (Number.isFinite(last.seq) ? (last.seq as number) : last.i) + 1 : 0,
    audio_ms: last ? last.audio_ms : 0,
  };
}

/** The questions asked live (`interactive.jsonl`), oldest first. */
export function parseInteractive(text: string): any[] {
  const asks: any[] = [];
  for (const line of String(text || '').split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const entry = JSON.parse(line);
      if (entry && typeof entry === 'object') asks.push(entry);
    } catch {
      /* a line being written */
    }
  }
  return asks;
}

/** Start times (ms) of lines a suggestion card answers (lib/copilot.ts). */
export function questionLineStarts(snapshot: any): number[] {
  if (!snapshot) return [];
  return (snapshot.messages || [])
    .filter((m: any) => m.kind === 'suggestion' && m.cardState !== 'not_for_me')
    .map((m: any) => m.audioMs);
}
