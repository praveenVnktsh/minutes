import type { MeetingActivity, RecordingActivity } from '@/types/meetingActivity';

/** What `api_discard_meeting` returns once the meeting's rows are deleted. */
export interface DiscardMeetingResult {
  /** The recording folder that could not be removed, if removing it failed. */
  folder_not_removed: string | null;
  folder_error: string | null;
}

// The same reasons the Rust core refuses a delete with (api.rs), so the UI can
// explain before the user confirms. The core's check is the authoritative one.
export const DELETE_BLOCKED_RECORDING = 'This meeting is still recording. Stop the recording before deleting it.';
export const DELETE_BLOCKED_TRANSCRIBING = 'Wait for transcription to finish before deleting this meeting.';
export const DELETE_BLOCKED_SUMMARY = 'Wait for the AI notes to finish before deleting this meeting.';

const FINISHED_ACTIVITY = new Set(['ready', 'failed', 'cancelled']);
const RUNNING_SUMMARY = new Set(['queued', 'processing']);

export interface MeetingDeleteState {
  recording: RecordingActivity | null;
  activities: MeetingActivity[];
  summaryStatus?: string | null;
}

/** Why this meeting cannot be deleted right now, or null when it can. */
export function meetingDeleteBlockedReason(meetingId: string, state: MeetingDeleteState): string | null {
  if (state.recording?.meeting_id === meetingId) return DELETE_BLOCKED_RECORDING;
  if (state.activities.some((activity) => (
    activity.meeting_id === meetingId && !FINISHED_ACTIVITY.has(activity.status)
  ))) return DELETE_BLOCKED_TRANSCRIBING;
  if (state.summaryStatus && RUNNING_SUMMARY.has(state.summaryStatus)) return DELETE_BLOCKED_SUMMARY;
  return null;
}

/** The warning to show when the meeting was deleted but its folder was not. */
export function folderNotRemovedMessage(result: DiscardMeetingResult | null | undefined): string | null {
  if (!result?.folder_not_removed) return null;
  return `The meeting was deleted, but its recording folder couldn't be removed: ${result.folder_not_removed}`;
}
