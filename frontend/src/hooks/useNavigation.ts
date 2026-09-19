'use client';

import { useCallback, useRef, useState } from 'react';
import { useSidebar, type CurrentMeeting } from '@/components/Sidebar/SidebarProvider';
import { meetingUrl } from '@/lib/meetingCatalog';
import { flushNotes } from '@/services/notePersistenceService';
import { useRouter } from 'next/navigation';

export interface MeetingNavigation {
  isNavigating: boolean;
  navigationError: Error | null;
  navigate: (href: string) => Promise<void>;
  openMeeting: (meeting: CurrentMeeting) => Promise<void>;
}

export const useMeetingNavigation = (): MeetingNavigation => {
  const router = useRouter();
  const { meetings, setCurrentMeeting } = useSidebar();
  const [isNavigating, setIsNavigating] = useState(false);
  const [navigationError, setNavigationError] = useState<Error | null>(null);
  const navigationRequestRef = useRef(0);

  const runNavigation = useCallback(async (href: string, meeting?: CurrentMeeting) => {
    const requestId = ++navigationRequestRef.current;
    setIsNavigating(true);
    setNavigationError(null);
    try {
      await flushNotes();
      if (requestId !== navigationRequestRef.current) return;
      if (meeting) setCurrentMeeting(meeting);
      router.push(href);
    } catch (error) {
      if (requestId !== navigationRequestRef.current) return;
      const navigationError = error instanceof Error ? error : new Error(String(error));
      setNavigationError(navigationError);
      throw navigationError;
    } finally {
      if (requestId === navigationRequestRef.current) setIsNavigating(false);
    }
  }, [router, setCurrentMeeting]);

  const navigate = useCallback(
    (href: string) => runNavigation(href),
    [runNavigation],
  );
  const openMeeting = useCallback(
    (meeting: CurrentMeeting) => {
      const catalogMeeting = meetings.find(({ id }) => id === meeting.id);
      return runNavigation(meetingUrl(meeting.id), { ...catalogMeeting, ...meeting });
    },
    [meetings, runNavigation],
  );

  return { isNavigating, navigationError, navigate, openMeeting };
};

export const useNavigation = (meetingId: string, meetingTitle: string) => {
  const { openMeeting } = useMeetingNavigation();
  return useCallback(
    () => openMeeting({ id: meetingId, title: meetingTitle }),
    [meetingId, meetingTitle, openMeeting],
  );
};
