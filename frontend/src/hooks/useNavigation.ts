'use client';

import { useCallback, useState } from 'react';
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
  const { setCurrentMeeting } = useSidebar();
  const [isNavigating, setIsNavigating] = useState(false);
  const [navigationError, setNavigationError] = useState<Error | null>(null);

  const runNavigation = useCallback(async (href: string, meeting?: CurrentMeeting) => {
    setIsNavigating(true);
    setNavigationError(null);
    try {
      await flushNotes();
      if (meeting) setCurrentMeeting(meeting);
      router.push(href);
    } catch (error) {
      const navigationError = error instanceof Error ? error : new Error(String(error));
      setNavigationError(navigationError);
      throw navigationError;
    } finally {
      setIsNavigating(false);
    }
  }, [router, setCurrentMeeting]);

  const navigate = useCallback(
    (href: string) => runNavigation(href),
    [runNavigation],
  );
  const openMeeting = useCallback(
    (meeting: CurrentMeeting) => runNavigation(meetingUrl(meeting.id), meeting),
    [runNavigation],
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
