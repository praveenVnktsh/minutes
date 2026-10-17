/*
 * The replay's live source: what the recording screen's real components
 * (useCopilot, useLiveTimeline, the live partials, the waveform, CopilotPanel)
 * listen to and invoke while they play a run back, instead of Tauri.
 *
 * - `listen` hands a new listener the state at the cursor for its event
 *   straight away, so a component mounted mid-replay is hydrated exactly as if
 *   it had been listening all along.
 * - `advance` (playing forward) delivers the recorded events between the old
 *   and new cursor in order, as the app received them; a long jump is
 *   delivered as the coalesced state instead.
 * - `seek` (either way) delivers the state at the new cursor as one
 *   consistent snapshot. When a slice goes back to "none" (before the first
 *   Copilot or live summary event), which no event can express, the epoch is
 *   bumped so the workspace remounts and rehydrates.
 * - `invoke` answers reads from the fold, routes Copilot asks to the live
 *   Copilot (the backend) and never reaches a recording, transcription,
 *   summary or database command.
 *
 * Live asks appear in the Copilot panel after the recorded messages.
 */

/* eslint-disable @typescript-eslint/no-explicit-any */

import type { LiveEvent, LiveSource } from '@/lib/liveSource';
import {
  COPILOT_DELTA_EVENT,
  COPILOT_STATE_EVENT,
  QUICK_ACTIONS,
  type CopilotMessage,
  type CopilotSnapshot,
} from '@/lib/copilot';
import { LIVE_TIMELINE_UPDATE_EVENT } from '@/lib/liveTimeline';
import { TRANSCRIPT_PARTIAL_EVENT } from '@/lib/liveTranscriptionModels';
import type { Transcript } from '@/types';
import {
  clock,
  liveCursor,
  reduce,
  stateAt,
  type ReplayIndex,
  type ReplayLine,
  type ReplayRecord,
  type ReplayState,
} from './reducer';

/** Above this many records, a forward step is delivered as the coalesced state. */
export const INCREMENTAL_LIMIT = 300;

/** Events the source delivers to the live components. */
const DELIVERED = new Set([
  COPILOT_STATE_EVENT,
  COPILOT_DELTA_EVENT,
  LIVE_TIMELINE_UPDATE_EVENT,
  TRANSCRIPT_PARTIAL_EVENT,
]);

/** One live Copilot ask, as `replay_copilot_ask` takes it. */
export interface LiveAskRequest {
  audio_ms: number;
  seq: number;
  kind: 'ask' | 'action';
  text?: string;
  action?: string;
}

/** The Rust side of live asks (Tauri commands in the app, a fake in tests). */
export interface ReplayBackend {
  ask(request: LiveAskRequest, requestId: string): Promise<void>;
  stop(): Promise<unknown>;
}

/** A live ask's progress, for the debug drawer. */
export interface LiveAskProgress {
  id: string;
  audioMs: number;
  phase: 'sent' | 'queued' | 'context' | 'warming' | 'streaming' | 'done' | 'error' | 'stopped';
  context: { lines: number; timeline: number; notes_chars: number; as_of_ms: number } | null;
  firstTokenMs: number | null;
  entry: any | null;
}

/** What React renders from the source (useSyncExternalStore snapshot). */
export interface ReplayView {
  version: number;
  epoch: number;
  n: number;
  tMs: number;
  audioMs: number;
  phase: string;
  transcripts: Transcript[];
  notes: string;
  live: LiveAskProgress[];
}

type Handler = (event: LiveEvent<any>) => void;

let fallbackIds = 0;
function newRequestId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') return crypto.randomUUID();
  fallbackIds += 1;
  return `replay-ask-${Date.now()}-${fallbackIds}`;
}

const transcriptCache = new WeakMap<ReplayLine, Transcript>();

/** A folded line as the transcript context carries it (TranscriptContext's shape). */
export function lineToTranscript(line: ReplayLine): Transcript {
  let transcript = transcriptCache.get(line);
  if (!transcript) {
    transcript = {
      id: line.id,
      text: line.text,
      timestamp: clock(line.start * 1000),
      speaker: line.source === 'mic' || line.source === 'system' ? line.source : undefined,
      sequence_id: line.i,
      chunk_start_time: line.start,
      is_partial: false,
      confidence: 1,
      audio_start_time: line.start,
      audio_end_time: line.end,
      duration: Math.max(0, line.end - line.start),
    };
    transcriptCache.set(line, transcript);
  }
  return transcript;
}

function clone<T>(value: T): T {
  return value === undefined || value === null ? value : JSON.parse(JSON.stringify(value));
}

/** The notes Copilot was given, by record (`script` notes records). */
function notesIndex(records: ReplayRecord[]): Array<{ i: number; text: string }> {
  const out: Array<{ i: number; text: string }> = [];
  for (const r of records) {
    if (r.type === 'debug' && r.name === 'script' && r.payload?.kind === 'notes' && typeof r.payload.text === 'string') {
      out.push({ i: r.i, text: r.payload.text });
    }
  }
  return out;
}

export class ReplaySource implements LiveSource {
  readonly kind = 'replay' as const;
  private readonly listeners = new Map<string, Set<Handler>>();
  private readonly subscribers = new Set<() => void>();
  private state: ReplayState;
  private readonly notes: Array<{ i: number; text: string }>;
  /** What the listeners currently hold. */
  private shown = { copilot: false, timeline: false, partials: new Set<string>() };
  private liveMessages: CopilotMessage[] = [];
  private liveProgress: LiveAskProgress[] = [];
  private view: ReplayView;
  private version = 0;
  private epoch = 0;
  private disposed = false;
  /** Called when a live ask starts (the page pauses playback). */
  onLiveAsk: (() => void) | null = null;

  constructor(
    readonly index: ReplayIndex,
    private readonly backend: ReplayBackend,
    private readonly sessionFallback = 'replay',
  ) {
    this.state = stateAt(index, 0, null);
    this.notes = notesIndex(index.records);
    this.view = this.buildView();
  }

  get records(): ReplayRecord[] {
    return this.index.records;
  }

  /** The cursor: how many records have happened. */
  get n(): number {
    return this.state.n;
  }

  get current(): ReplayState {
    return this.state;
  }

  // -------------------------------------------------------------------------
  // LiveSource
  // -------------------------------------------------------------------------

  listen<T>(event: string, handler: (event: LiveEvent<T>) => void): Promise<() => void> {
    const set = this.listeners.get(event) ?? new Set<Handler>();
    this.listeners.set(event, set);
    const entry = handler as Handler;
    set.add(entry);
    // Hydrate after the caller has stored its unlisten function.
    queueMicrotask(() => {
      if (!set.has(entry) || this.disposed) return;
      for (const payload of this.statePayloads(event)) entry({ payload });
    });
    return Promise.resolve(() => {
      set.delete(entry);
    });
  }

  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    return this.handle(command, args ?? {}) as Promise<T>;
  }

  private async handle(command: string, args: Record<string, unknown>): Promise<unknown> {
    switch (command) {
      case 'copilot_get_state':
        return this.composeCopilot(clone(this.state.copilot));
      case 'get_live_timeline':
        return this.state.timeline;
      case 'get_recording_level':
        // No samples in a replay: a gentle level while someone is speaking.
        return Object.keys(this.state.partials).length > 0 ? 0.03 + 0.02 * Math.abs(Math.sin(this.state.tMs / 180)) : 0.001;
      case 'set_live_transcription_enabled':
      case 'copilot_set_notes':
      case 'copilot_dismiss_card':
        return null;
      case 'get_copilot_settings':
        return { suggestionsEnabled: this.composeCopilot(clone(this.state.copilot))?.suggestionsEnabled ?? true, debug: true, model: null };
      case 'set_copilot_settings':
        throw new Error('Suggestions are as recorded in a replay');
      case 'copilot_ask':
        return this.askLive((args.action as string | null) ?? null, (args.question as string | null) ?? null);
      case 'copilot_stop': {
        if (this.liveMessages.some((m) => m.id === args.id)) await this.backend.stop();
        return null;
      }
      case 'copilot_retry': {
        const message = this.liveMessages.find((m) => m.id === args.id);
        if (!message) throw new Error('Only questions asked in this replay can be retried');
        return this.askLive(message.action, message.kind === 'ask' ? message.prompt : null);
      }
      case 'copilot_shorter':
        throw new Error('Shorter is not available in a replay');
      default:
        throw new Error(`Not available in a replay: ${command}`);
    }
  }

  // -------------------------------------------------------------------------
  // Cursor
  // -------------------------------------------------------------------------

  /** Moves to cursor `n` (either way) and delivers the state there. */
  seek(n: number): void {
    const target = Math.max(0, Math.min(n, this.index.records.length));
    if (target === this.state.n) return;
    this.state = stateAt(this.index, target, target > this.state.n ? this.state : null);
    this.deliverState();
    this.publish();
  }

  /** Plays forward to cursor `n`, delivering the events on the way. */
  advance(n: number): void {
    const target = Math.max(0, Math.min(n, this.index.records.length));
    if (target <= this.state.n) {
      if (target < this.state.n) this.seek(target);
      return;
    }
    const from = this.state.n;
    if (target - from > INCREMENTAL_LIMIT) {
      this.seek(target);
      return;
    }
    const delivered: ReplayRecord[] = [];
    for (let i = from; i < target; i += 1) {
      const record = this.index.records[i];
      reduce(this.state, record);
      if (record.type === 'event' && DELIVERED.has(record.name)) delivered.push(record);
    }
    for (const record of delivered) this.deliverRecord(record);
    this.publish();
  }

  // -------------------------------------------------------------------------
  // React
  // -------------------------------------------------------------------------

  subscribe = (fn: () => void): (() => void) => {
    this.subscribers.add(fn);
    return () => {
      this.subscribers.delete(fn);
    };
  };

  getView = (): ReplayView => this.view;

  dispose(): void {
    this.disposed = true;
    this.listeners.clear();
    this.subscribers.clear();
  }

  /** A live ask's progress from Rust (`replay-copilot` events). */
  handleLiveEvent(requestId: string, event: string, data: any): void {
    const message = this.liveMessages.find((m) => m.id === requestId);
    const progress = this.liveProgress.find((p) => p.id === requestId);
    if (!message || !progress) return;
    let changed = true;
    switch (event) {
      case 'queued':
        progress.phase = 'queued';
        break;
      case 'context':
        progress.phase = 'context';
        progress.context = data;
        break;
      case 'warming':
        progress.phase = 'warming';
        break;
      case 'start':
        progress.phase = 'streaming';
        this.updateLive(requestId, {
          status: message.status === 'pending' ? 'streaming' : message.status,
          provider: data?.provider ?? message.provider,
          model: data?.model ?? message.model,
        });
        break;
      case 'first-token':
        progress.firstTokenMs = typeof data?.ms === 'number' ? data.ms : null;
        break;
      case 'delta': {
        const offset = Number(data?.offset);
        const text = String(data?.text ?? '');
        const current = this.liveMessages.find((m) => m.id === requestId)!;
        const sessionId = this.sessionId();
        this.updateLive(requestId, {
          reply: current.reply + text,
          status: current.status === 'pending' ? 'streaming' : current.status,
        });
        this.emit(COPILOT_DELTA_EVENT, { sessionId, id: requestId, offset, delta: text });
        changed = false;
        break;
      }
      case 'done': {
        const status = data?.status === 'done' || data?.status === 'stopped' || data?.status === 'error' ? data.status : 'done';
        progress.phase = status === 'error' ? 'error' : status === 'stopped' ? 'stopped' : 'done';
        progress.entry = data;
        this.updateLive(requestId, {
          status,
          reply: typeof data?.reply === 'string' ? data.reply : message.reply,
          prompt: typeof data?.prompt === 'string' && data.prompt ? data.prompt : message.prompt,
          error: data?.error ?? null,
          stats: data?.stats ?? null,
          provider: data?.provider ?? message.provider,
          model: data?.model ?? message.model,
        });
        break;
      }
      case 'error':
        progress.phase = 'error';
        this.updateLive(requestId, { status: 'error', error: String(data?.message ?? 'Copilot failed') });
        break;
      default:
        changed = false;
    }
    if (changed) this.emitCopilot();
    this.publish();
  }

  // -------------------------------------------------------------------------
  // Internals
  // -------------------------------------------------------------------------

  private sessionId(): string {
    return this.state.copilot?.sessionId ?? this.sessionFallback;
  }

  private async askLive(action: string | null, question: string | null): Promise<string> {
    if (!action && !question?.trim()) throw new Error('Type a question first');
    const cursor = liveCursor(this.index.records, this.state.n);
    const id = newRequestId();
    const label = action ? QUICK_ACTIONS.find((a) => a.id === action)?.label ?? action : question!.trim();
    this.liveMessages = [
      ...this.liveMessages,
      {
        id,
        kind: action ? 'action' : 'ask',
        action: (action as CopilotMessage['action']) ?? null,
        prompt: label,
        audioMs: cursor.audio_ms,
        reply: '',
        status: 'pending',
        error: null,
        canUseBuiltin: false,
        cardState: null,
        provider: null,
        model: null,
        stats: null,
        createdAt: new Date().toISOString(),
      },
    ];
    this.liveProgress = [
      ...this.liveProgress,
      { id, audioMs: cursor.audio_ms, phase: 'sent', context: null, firstTokenMs: null, entry: null },
    ];
    this.emitCopilot();
    this.publish();
    this.onLiveAsk?.();
    const request: LiveAskRequest = action
      ? { audio_ms: cursor.audio_ms, seq: cursor.seq, kind: 'action', action }
      : { audio_ms: cursor.audio_ms, seq: cursor.seq, kind: 'ask', text: question!.trim() };
    void this.backend.ask(request, id).catch((error) => {
      this.handleLiveEvent(id, 'error', { message: error instanceof Error ? error.message : String(error) });
    });
    return id;
  }

  private updateLive(id: string, patch: Partial<CopilotMessage>): void {
    this.liveMessages = this.liveMessages.map((m) => (m.id === id ? { ...m, ...patch } : m));
  }

  /**
   * The snapshot the Copilot panel shows: the recorded one with this
   * session's live asks after it. Once there are lines, the panel is active so
   * questions can be asked at any moment of the run.
   */
  private composeCopilot(recorded: CopilotSnapshot | null): CopilotSnapshot | null {
    const lines = this.state.lines.length;
    if (!recorded && lines === 0 && this.liveMessages.length === 0) return null;
    const base: CopilotSnapshot = recorded ?? {
      sessionId: this.sessionFallback,
      active: false,
      lineCount: 0,
      messages: [],
      suggestionsEnabled: true,
      debugEnabled: true,
      debug: {
        last: null,
        detector: { rulePass: 0, ruleReject: 0, confirmYes: 0, confirmNo: 0, confirmUnknown: 0, skipped: 0, falseAlarms: 0, last: null },
        liveBacklogSecs: 0,
      },
    };
    if (lines === 0 && this.liveMessages.length === 0) return base;
    return {
      ...base,
      active: true,
      lineCount: Math.max(base.lineCount ?? 0, lines),
      messages: [...(base.messages ?? []), ...this.liveMessages],
    };
  }

  /** Payloads that bring a fresh listener of `event` to the cursor's state. */
  private statePayloads(event: string): unknown[] {
    switch (event) {
      case COPILOT_STATE_EVENT: {
        const snapshot = this.composeCopilot(clone(this.state.copilot));
        return snapshot ? [snapshot] : [];
      }
      case LIVE_TIMELINE_UPDATE_EVENT:
        return this.state.timeline ? [this.state.timeline] : [];
      case TRANSCRIPT_PARTIAL_EVENT:
        return Object.values(this.state.partials).map((p) => ({ source: p.source, text: p.text, audio_start_time: p.start }));
      default:
        return [];
    }
  }

  private emit(event: string, payload: unknown): void {
    const set = this.listeners.get(event);
    if (!set) return;
    for (const handler of [...set]) handler({ payload });
  }

  private emitCopilot(): void {
    const snapshot = this.composeCopilot(clone(this.state.copilot));
    if (snapshot) {
      this.emit(COPILOT_STATE_EVENT, snapshot);
      this.shown.copilot = true;
    }
  }

  private deliverRecord(record: ReplayRecord): void {
    const p = record.payload;
    switch (record.name) {
      case COPILOT_STATE_EVENT: {
        const snapshot = this.composeCopilot(p);
        if (snapshot) this.emit(COPILOT_STATE_EVENT, snapshot);
        this.shown.copilot = true;
        break;
      }
      case COPILOT_DELTA_EVENT:
        this.emit(COPILOT_DELTA_EVENT, p);
        break;
      case LIVE_TIMELINE_UPDATE_EVENT:
        this.emit(LIVE_TIMELINE_UPDATE_EVENT, p);
        this.shown.timeline = true;
        break;
      case TRANSCRIPT_PARTIAL_EVENT: {
        this.emit(TRANSCRIPT_PARTIAL_EVENT, p);
        const source = p?.source || 'system';
        if (String(p?.text || '').trim()) this.shown.partials.add(source);
        else this.shown.partials.delete(source);
        break;
      }
      default:
        break;
    }
    // A delta before any state, or lines making the composed snapshot
    // appear, are covered by the composed snapshot below.
    if (!this.shown.copilot && this.composeCopilot(null)) this.emitCopilot();
  }

  /** Delivers the cursor's state as one snapshot (after a seek or a jump). */
  private deliverState(): void {
    const copilot = this.composeCopilot(clone(this.state.copilot));
    const timeline = this.state.timeline;
    if ((!copilot && this.shown.copilot) || (!timeline && this.shown.timeline)) {
      // "None" cannot be delivered: remount the workspace, which rehydrates.
      this.epoch += 1;
      this.shown = {
        copilot: Boolean(copilot),
        timeline: Boolean(timeline),
        partials: new Set(Object.keys(this.state.partials)),
      };
      return;
    }
    if (copilot) {
      this.emit(COPILOT_STATE_EVENT, copilot);
      this.shown.copilot = true;
    }
    if (timeline) {
      this.emit(LIVE_TIMELINE_UPDATE_EVENT, timeline);
      this.shown.timeline = true;
    }
    for (const source of [...this.shown.partials]) {
      if (!this.state.partials[source]) {
        this.emit(TRANSCRIPT_PARTIAL_EVENT, { source, text: '', audio_start_time: 0 });
      }
    }
    for (const partial of Object.values(this.state.partials)) {
      this.emit(TRANSCRIPT_PARTIAL_EVENT, { source: partial.source, text: partial.text, audio_start_time: partial.start });
    }
    this.shown.partials = new Set(Object.keys(this.state.partials));
  }

  private notesAt(n: number): string {
    let text = '';
    for (const entry of this.notes) {
      if (entry.i < n) text = entry.text;
      else break;
    }
    return text;
  }

  private buildView(): ReplayView {
    const state = this.state;
    return {
      version: this.version,
      epoch: this.epoch,
      n: state.n,
      tMs: state.tMs,
      audioMs: state.audioMs,
      phase: state.phase,
      transcripts: state.lines.map(lineToTranscript),
      notes: this.notesAt(state.n),
      live: this.liveProgress.map((p) => ({ ...p })),
    };
  }

  private publish(): void {
    this.version += 1;
    const previous = this.view;
    const next = this.buildView();
    // Keep the transcript array's identity when no line was added or removed,
    // so the transcript does not re-render on every frame.
    if (
      previous.transcripts.length === next.transcripts.length
      && previous.transcripts[previous.transcripts.length - 1] === next.transcripts[next.transcripts.length - 1]
    ) {
      next.transcripts = previous.transcripts;
    }
    this.view = next;
    for (const fn of [...this.subscribers]) fn();
  }
}
