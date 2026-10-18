import { invoke } from '@tauri-apps/api/core';
import type { LiveSource } from '@/lib/liveSource';
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

/** The recording's live summary snapshot, through a live source. */
export function currentLiveTimeline(source: LiveSource): Promise<TimelineSnapshot | null> {
  return source.invoke<TimelineSnapshot | null>('get_live_timeline');
}

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

/** Why no AI notes are coming while recording, if anything is wrong. */
export function statusNotice(snapshot: TimelineSnapshot | null): { tone: Tone; text: string } | null {
  if (!snapshot) return null;
  switch (snapshot.status) {
    case 'failed':
      return { tone: 'warning', text: 'AI notes paused. They try again with the next minute.' };
    case 'unavailable':
      // With an error, the provider refused the request (key, quota, rate
      // limit); without one, no summary model is set up.
      return snapshot.error
        ? { tone: 'warning', text: snapshot.error }
        : { tone: 'neutral', text: 'AI notes need a summary model. Choose one in Settings → Summary.' };
    case 'disabled':
      return { tone: 'neutral', text: 'AI notes are off. Turn them on in Settings → Summary.' };
    default:
      return null;
  }
}

