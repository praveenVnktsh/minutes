'use client';

import { useEffect, useRef } from 'react';
import { useSidebar } from '../Sidebar/SidebarProvider';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';

/**
 * Background transcription no longer shows toast UI: the meeting workspace owns the
 * progress surface now. We still need to fan out queue completion so the open
 * meeting refetches its transcript and auto-summary can start.
 */
export function useTranscriptionProgressToast() {
  const { refetchMeetings } = useSidebar();
  const { snapshot } = useMeetingActivity();
  const seenReadyTasks = useRef(new Set<string>());

  useEffect(() => {
    const newlyReady = snapshot.activities.filter((activity) => (
      activity.status === 'ready'
      && activity.meeting_id
      && !seenReadyTasks.current.has(activity.task_id)
    ));
    snapshot.activities.forEach((activity) => {
      if (activity.status === 'ready') seenReadyTasks.current.add(activity.task_id);
    });
    if (newlyReady.length > 0) {
      void refetchMeetings();
      // Compatibility only: the current workspace uses this to refetch its
      // transcript. Activity ownership itself never consumes DOM events.
      newlyReady.forEach((activity) => {
        window.dispatchEvent(new CustomEvent('meetily:transcription-complete', {
          detail: { meetingId: activity.meeting_id },
        }));
      });
    }
  }, [refetchMeetings, snapshot.activities]);

  return {};
}

export function TranscriptionProgressToastProvider() {
  useTranscriptionProgressToast();
  return null;
}
