'use client';

import { LiveNotesPad } from '@/components/LiveNotesPad';
import { useLiveTimeline } from '@/components/MeetingDetails/LiveSummaryPanel';
import { useConfig } from '@/contexts/ConfigContext';

/** The Raw notes while recording. With "Write live notes into my notes"
 * (Settings → Beta) the live notes are added to them as AI lines. */
export function LiveRawNotes() {
  const { betaFeatures } = useConfig();
  const timeline = useLiveTimeline();
  const aiEntries = betaFeatures.liveNotesInRawNotes ? timeline?.entries ?? [] : undefined;
  return <LiveNotesPad bare aiEntries={aiEntries} />;
}
