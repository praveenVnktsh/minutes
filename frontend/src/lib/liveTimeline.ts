import { invoke } from '@tauri-apps/api/core';
import type { Tone } from '@/lib/theme-classes';

/** One live summary entry. Times are audio time from the recording start. */
export type TimelineKind = 'point' | 'decision' | 'action';

export interface TimelineEntry {
  id: string;
  start_ms: number;
  end_ms: number;
  kind: TimelineKind;
  text: string;
  speaker: string | null;
  provider: string;
  model: string;
  created_at: string;
  /** The section title the entry sits under. Absent on entries made before
   * topics existed. */
  topic?: string | null;
}

/** A stored entry also carries its meeting. */
export interface StoredTimelineEntry extends TimelineEntry {
  meeting_id: string;
}

export type TimelineStatus =
  | 'listening'
  | 'generating'
  | 'failed'
  | 'unavailable'
  | 'disabled'
  | 'stopped';

/** `live-timeline-update` payload and `get_live_timeline` result. */
export interface TimelineSnapshot {
  session_id: string;
  meeting_id: string | null;
  status: TimelineStatus;
  error: string | null;
  entries: TimelineEntry[];
}

export interface LiveSummarySettings {
  enabled: boolean;
}

export const LIVE_TIMELINE_UPDATE_EVENT = 'live-timeline-update';
/** Window event the transcript view handles: scroll to `seconds`. */
export const JUMP_TO_TIME_EVENT = 'meetily:jump-to-time';

export const liveTimelineApi = {
  current: () => invoke<TimelineSnapshot | null>('get_live_timeline'),
  listForMeeting: (meetingId: string) =>
    invoke<StoredTimelineEntry[]>('list_meeting_timeline_entries', { meetingId }),
  getSettings: () => invoke<LiveSummarySettings>('get_live_summary_settings'),
  setSettings: (enabled: boolean) =>
    invoke<LiveSummarySettings>('set_live_summary_settings', { enabled }),
};

/** `mm:ss`, or `h:mm:ss` past the hour. */
export function formatTimelineTime(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const pad = (value: number) => String(value).padStart(2, '0');
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(seconds)}` : `${pad(minutes)}:${pad(seconds)}`;
}

/** The label and tone a kind is marked with; points are unmarked. */
export function kindMarker(kind: TimelineKind): { label: string; tone: Tone } | null {
  switch (kind) {
    case 'decision':
      return { label: 'Decision', tone: 'success' };
    case 'action':
      return { label: 'Action', tone: 'info' };
    default:
      return null;
  }
}

/** A run of consecutive entries under one topic. `topic` is null for
 * entries made before topics existed, which render without a header. */
export interface TimelineSection<T extends TimelineEntry = TimelineEntry> {
  topic: string | null;
  entries: T[];
}

/** Entries (in time order) grouped into sections: a new section starts when
 * an entry names a different topic. An entry without a topic stays in the
 * section before it. */
export function groupByTopic<T extends TimelineEntry>(entries: T[]): TimelineSection<T>[] {
  const sections: TimelineSection<T>[] = [];
  for (const entry of entries) {
    const topic = entry.topic?.trim() || null;
    const last = sections[sections.length - 1];
    if (last && (topic === null || topic === last.topic)) {
      last.entries.push(entry);
    } else {
      sections.push({ topic, entries: [entry] });
    }
  }
  return sections;
}

/** The text an entry shows: an action the user owns reads "You — task".
 * Other speakers are never shown; the note states what was said. */
export function entryDisplayText(entry: Pick<TimelineEntry, 'kind' | 'text' | 'speaker'>): string {
  if (entry.kind === 'action' && entry.speaker === 'You' && !/^you\b/i.test(entry.text)) {
    return `You — ${entry.text}`;
  }
  return entry.text;
}

/** Whether a snapshot belongs on the panel showing `current`: a newer
 * session replaces it, an older one's late update is ignored. */
export function acceptSnapshot(
  current: TimelineSnapshot | null,
  next: TimelineSnapshot,
): TimelineSnapshot | null {
  if (!current || current.session_id === next.session_id) return next;
  // A different session: only a live one replaces what is shown.
  return next.status === 'stopped' ? current : next;
}

/** The notice the live panel shows for a status, if any. */
export function statusNotice(snapshot: TimelineSnapshot | null): { tone: Tone; text: string } | null {
  if (!snapshot) return null;
  switch (snapshot.status) {
    case 'failed':
      return { tone: 'warning', text: 'Live summary paused. It will try again with the next minute of conversation.' };
    case 'unavailable':
      // With an error, the provider refused the request (key, quota, rate
      // limit); without one, no summary model is set up.
      return snapshot.error
        ? { tone: 'warning', text: snapshot.error }
        : { tone: 'neutral', text: 'Live summary needs a summary model. Choose one in Settings → Summary.' };
    case 'disabled':
      return { tone: 'neutral', text: 'Live summary is off. Turn it on in Settings → Summary.' };
    default:
      return null;
  }
}

/** Ask the transcript view to scroll to a time. */
export function jumpToTime(ms: number): void {
  window.dispatchEvent(new CustomEvent(JUMP_TO_TIME_EVENT, { detail: { seconds: ms / 1000 } }));
}
