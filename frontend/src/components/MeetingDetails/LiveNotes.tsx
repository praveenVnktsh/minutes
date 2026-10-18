'use client';

import { LiveNotesPad } from '@/components/LiveNotesPad';
import { useLiveTimeline } from '@/hooks/useLiveTimeline';
import { statusNotice } from '@/lib/liveTimeline';

/** The notes while recording: the user's lines, with the live notes written
 * in as AI lines under their topics whenever the user pauses typing. */
export function LiveNotes() {
  const timeline = useLiveTimeline();
  return <LiveNotesPad bare aiEntries={timeline?.entries ?? []} />;
}

/** One quiet line when no AI notes are coming (no model, turned off, paused). */
export function LiveNotesNotice() {
  const notice = statusNotice(useLiveTimeline());
  return notice ? <span role="status" title={notice.text}>{notice.text}</span> : null;
}
