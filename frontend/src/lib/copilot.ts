import { invoke } from '@tauri-apps/api/core';
import { formatTimelineTime, JUMP_TO_TIME_EVENT } from '@/lib/liveTimeline';

/** The quick actions on the Copilot panel, in display order. */
export const QUICK_ACTIONS = [
  { id: 'what_to_say', label: 'What should I say?' },
  { id: 'explain', label: 'Explain that' },
  { id: 'catch_up', label: 'Catch me up' },
  { id: 'follow_up', label: 'Draft follow-up' },
] as const;

export type QuickActionId = (typeof QUICK_ACTIONS)[number]['id'] | 'shorter';
export type CopilotMessageKind = 'ask' | 'action' | 'suggestion';
export type CopilotStatus = 'pending' | 'streaming' | 'done' | 'stopped' | 'error';
export type CardState = 'active' | 'dismissed' | 'not_for_me';

export interface CopilotStats {
  firstTokenMs: number | null;
  totalMs: number;
  tokensPerSecond: number | null;
  promptTokens: number | null;
  reusedTokens: number | null;
}

export interface CopilotMessage {
  id: string;
  kind: CopilotMessageKind;
  action: QuickActionId | null;
  prompt: string;
  audioMs: number;
  reply: string;
  status: CopilotStatus;
  error: string | null;
  canUseBuiltin: boolean;
  cardState: CardState | null;
  provider: string | null;
  model: string | null;
  stats: CopilotStats | null;
  createdAt: string;
}

export interface DetectorStats {
  rulePass: number;
  ruleReject: number;
  confirmYes: number;
  confirmNo: number;
  confirmUnknown: number;
  skipped: number;
  falseAlarms: number;
  last: string | null;
}

/** `copilot-state` payload and `copilot_get_state` result. */
export interface CopilotSnapshot {
  sessionId: string;
  active: boolean;
  lineCount: number;
  messages: CopilotMessage[];
  suggestionsEnabled: boolean;
  debugEnabled: boolean;
  debug: {
    last: CopilotStats | null;
    detector: DetectorStats;
    liveBacklogSecs: number;
  };
}

/** `copilot-delta` payload: `delta` belongs at byte `offset` of the reply. */
export interface CopilotDelta {
  sessionId: string;
  id: string;
  offset: number;
  delta: string;
}

export interface CopilotModelChoice {
  provider: string;
  model: string;
}

export interface CopilotSettings {
  suggestionsEnabled: boolean;
  debug: boolean;
  model: CopilotModelChoice | null;
}

export const COPILOT_STATE_EVENT = 'copilot-state';
export const COPILOT_DELTA_EVENT = 'copilot-delta';

export const copilotApi = {
  state: () => invoke<CopilotSnapshot | null>('copilot_get_state'),
  ask: (action: QuickActionId | null, question: string | null) =>
    invoke<string>('copilot_ask', { action, question }),
  stop: (id: string) => invoke<void>('copilot_stop', { id }),
  retry: (id: string, useBuiltin: boolean) => invoke<void>('copilot_retry', { id, useBuiltin }),
  shorter: (id: string) => invoke<string>('copilot_shorter', { id }),
  dismissCard: (id: string, notForMe: boolean) => invoke<void>('copilot_dismiss_card', { id, notForMe }),
  setNotes: (text: string) => invoke<void>('copilot_set_notes', { text }),
  getSettings: () => invoke<CopilotSettings>('get_copilot_settings'),
  setSettings: (settings: CopilotSettings) => invoke<CopilotSettings>('set_copilot_settings', { settings }),
};

/** UTF-8 byte length, the unit Rust's `String::len` (the delta offset) uses. */
function byteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

/**
 * Applies a streamed chunk. Snapshots and deltas can cross (a snapshot may
 * already contain a chunk whose delta arrives later), so each delta says
 * where it goes: a chunk that is already there is skipped, and one that would
 * leave a gap is dropped until the next snapshot fills it in.
 */
export function applyDelta(snapshot: CopilotSnapshot | null, delta: CopilotDelta): CopilotSnapshot | null {
  if (!snapshot || snapshot.sessionId !== delta.sessionId) return snapshot;
  let changed = false;
  const messages = snapshot.messages.map((message) => {
    if (message.id !== delta.id) return message;
    const have = byteLength(message.reply);
    if (delta.offset !== have) return message;
    changed = true;
    return {
      ...message,
      reply: message.reply + delta.delta,
      status: message.status === 'pending' ? 'streaming' as const : message.status,
    };
  });
  return changed ? { ...snapshot, messages } : snapshot;
}

/** Whether `next` replaces `current` (another session's late update must not). */
export function acceptCopilotSnapshot(current: CopilotSnapshot | null, next: CopilotSnapshot): CopilotSnapshot {
  if (current && current.sessionId !== next.sessionId && current.active && !next.active) return current;
  return next;
}

const CITATION = /\[(\d{1,2}:)?(\d{1,2}):(\d{2})\](?!\()/g;

/** Seconds from a `[MM:SS]` / `[H:MM:SS]` citation, or null. */
export function citationSeconds(text: string): number | null {
  const match = /^\[(?:(\d{1,2}):)?(\d{1,2}):(\d{2})\]$/.exec(text.trim());
  if (!match) return null;
  const [, hours, minutes, seconds] = match;
  const value = Number(hours ?? 0) * 3600 + Number(minutes) * 60 + Number(seconds);
  return Number(seconds) < 60 ? value : null;
}

/** The link target a citation becomes in Markdown. */
export const CITATION_HREF_PREFIX = '#copilot-t=';

/** Turns `[MM:SS]` citations into Markdown links the renderer makes clickable. */
export function linkifyCitations(markdown: string): string {
  return markdown.replace(CITATION, (whole) => {
    const seconds = citationSeconds(whole);
    return seconds === null ? whole : `[${whole}](${CITATION_HREF_PREFIX}${seconds})`;
  });
}

/** Seconds from a linkified citation's href, or null for any other link. */
export function citationHrefSeconds(href: string | undefined): number | null {
  if (!href?.startsWith(CITATION_HREF_PREFIX)) return null;
  const seconds = Number(href.slice(CITATION_HREF_PREFIX.length));
  return Number.isFinite(seconds) && seconds >= 0 ? seconds : null;
}

/** Scroll the transcript (live or final) to a cited moment. */
export function jumpToSeconds(seconds: number): void {
  window.dispatchEvent(new CustomEvent(JUMP_TO_TIME_EVENT, { detail: { seconds } }));
}

/** "LIVE · [25:31] · WHAT SHOULD I SAY?" for a saved Copilot moment. */
export function liveMomentLabel(kind: string | null | undefined, label: string | null | undefined, audioMs: number | null | undefined): string | null {
  if (!kind?.startsWith('live_')) return null;
  const parts = ['LIVE'];
  if (typeof audioMs === 'number') parts.push(`[${formatTimelineTime(audioMs)}]`);
  if (label) parts.push(label.toUpperCase());
  else if (kind === 'live_ask') parts.push('QUESTION');
  return parts.join(' · ');
}

/** The header of a suggestion card. */
export function cardHeader(message: Pick<CopilotMessage, 'audioMs'>): string {
  return `OTHERS ASKED YOU · [${formatTimelineTime(message.audioMs)}]`;
}

/** Start times (ms) of the transcript lines a suggestion card answers. */
export function questionLineStarts(snapshot: CopilotSnapshot | null): number[] {
  if (!snapshot) return [];
  return snapshot.messages
    .filter((m) => m.kind === 'suggestion' && m.cardState !== 'not_for_me')
    .map((m) => m.audioMs);
}

/** Whether a transcript segment (seconds, speaker) is a line a card answers. */
export function isQuestionLine(starts: number[], startSeconds: number | undefined, speaker: string | undefined): boolean {
  if (speaker !== 'system' || typeof startSeconds !== 'number') return false;
  const ms = Math.round(startSeconds * 1000);
  return starts.some((start) => Math.abs(start - ms) <= 50);
}

/** The developer debug line under the input. */
export function debugLine(snapshot: CopilotSnapshot): string {
  const last = snapshot.debug.last;
  const d = snapshot.debug.detector;
  const parts: string[] = [];
  if (last) {
    parts.push(`first word ${last.firstTokenMs == null ? '–' : `${(last.firstTokenMs / 1000).toFixed(2)}s`}`);
    parts.push(`${last.tokensPerSecond == null ? '–' : last.tokensPerSecond.toFixed(1)} tok/s`);
    parts.push(`reused ${last.reusedTokens ?? '–'}/${last.promptTokens ?? '–'}`);
  } else {
    parts.push('no answers yet');
  }
  parts.push(`detector ${d.rulePass}✓/${d.ruleReject}✗ rule, ${d.confirmYes}✓/${d.confirmNo}✗/${d.confirmUnknown}? model, ${d.skipped} skipped`);
  parts.push(`false alarms ${d.falseAlarms}`);
  parts.push(`live backlog ${snapshot.debug.liveBacklogSecs.toFixed(1)}s`);
  if (d.last) parts.push(`last: ${d.last}`);
  return parts.join(' · ');
}
