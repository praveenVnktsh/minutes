'use client';

import React, { createContext, useContext, useState, useEffect } from 'react';
import { usePathname, useRouter } from 'next/navigation';
import Analytics from '@/lib/analytics';
import { invoke } from '@tauri-apps/api/core';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { flushNotes } from '@/services/notePersistenceService';
import type { SummaryProcessResponse } from '@/types';
import {
  errorMessage,
  meetingUrl,
  mergeMeetingCatalog,
  patchMeeting,
  selectMeetings,
  selectSearchResults,
  type MeetingCatalogEntry,
  type MeetingSearchResult,
  type MeetingVisibility,
} from '@/lib/meetingCatalog';

// Set when a recording starts and cleared when it is saved or discarded. While
// it is present the live recording workspace owns the session.
const ACTIVE_RECORDING_MEETING_ID_KEY = 'active_recording_meeting_id';

interface SidebarItem {
  id: string;
  title: string;
  type: 'folder' | 'file';
  children?: SidebarItem[];
}

export type CurrentMeeting = MeetingCatalogEntry;

// Search result type for transcript search
export type TranscriptSearchResult = MeetingSearchResult;

export type CatalogStatus = 'loading' | 'refreshing' | 'ready' | 'error';
export type SearchStatus = 'idle' | 'searching' | 'success' | 'error';
export type MeetingMutationKind = 'rename' | 'pin' | 'archive';

export interface MeetingMutationStatus {
  status: 'pending' | 'error';
  error: string | null;
}

export type MeetingMutationStates = Record<
  string,
  Partial<Record<MeetingMutationKind, MeetingMutationStatus>>
>;

interface SummaryPoll {
  processId: string;
  timer: NodeJS.Timeout;
  inFlight: boolean;
}

interface SidebarContextType {
  currentMeeting: CurrentMeeting | null;
  setCurrentMeeting: (meeting: CurrentMeeting | null) => void;
  sidebarItems: SidebarItem[];
  isCollapsed: boolean;
  toggleCollapse: () => void;
  meetings: CurrentMeeting[];
  setMeetings: (meetings: CurrentMeeting[]) => void;
  catalogStatus: CatalogStatus;
  catalogError: string | null;
  isCatalogEmpty: boolean;
  selectMeetings: (visibility?: MeetingVisibility) => CurrentMeeting[];
  isMeetingActive: boolean;
  setIsMeetingActive: (active: boolean) => void;
  handleRecordingToggle: () => Promise<void>;
  // Return to the live recording workspace when a recording is in progress.
  openActiveRecordingWorkspace: () => Promise<void>;
  navigationError: string | null;
  searchTranscripts: (query: string) => Promise<void>;
  searchResults: TranscriptSearchResult[];
  isSearching: boolean;
  searchQuery: string;
  searchStatus: SearchStatus;
  searchError: string | null;
  hasNoSearchResults: boolean;
  selectSearchResults: (visibility?: MeetingVisibility) => CurrentMeeting[];
  renameMeeting: (meetingId: string, title: string) => Promise<void>;
  setMeetingPinned: (meetingId: string, pinned: boolean) => Promise<void>;
  setMeetingArchived: (meetingId: string, archived: boolean) => Promise<void>;
  meetingMutations: MeetingMutationStates;
  setServerAddress: (address: string) => void;
  serverAddress: string;
  transcriptServerAddress: string;
  setTranscriptServerAddress: (address: string) => void;
  // Compatibility API: c6 will migrate summary polling to the activity owner.
  startSummaryPolling: (
    meetingId: string,
    processId: string,
    onUpdate: (result: SummaryProcessResponse) => void | Promise<void>
  ) => void;
  stopSummaryPolling: (meetingId: string, processId?: string) => void;
  // Refetch meetings from backend
  refetchMeetings: () => Promise<void>;

}

const SidebarContext = createContext<SidebarContextType | null>(null);

export const useSidebar = () => {
  const context = useContext(SidebarContext);
  if (!context) {
    throw new Error('useSidebar must be used within a SidebarProvider');
  }
  return context;
};

export function SidebarProvider({ children }: { children: React.ReactNode }) {
  const [currentMeeting, setCurrentMeeting] = useState<CurrentMeeting | null>({ id: 'intro-call', title: '+ New Call' });
  const [isCollapsed, setIsCollapsed] = useState(true);
  const [meetings, setMeetings] = useState<CurrentMeeting[]>([]);
  const [isMeetingActive, setIsMeetingActive] = useState(false);
  const [catalogStatus, setCatalogStatus] = useState<CatalogStatus>('loading');
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [searchResults, setSearchResults] = useState<TranscriptSearchResult[]>([]);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchStatus, setSearchStatus] = useState<SearchStatus>('idle');
  const [searchError, setSearchError] = useState<string | null>(null);
  const [meetingMutations, setMeetingMutations] = useState<MeetingMutationStates>({});
  const [navigationError, setNavigationError] = useState<string | null>(null);
  const [serverAddress, setServerAddress] = useState('');
  const [transcriptServerAddress, setTranscriptServerAddress] = useState('');
  const summaryPollsRef = React.useRef(new Map<string, SummaryPoll>());
  const meetingsRef = React.useRef<CurrentMeeting[]>([]);
  const catalogRequestRef = React.useRef(0);
  const catalogLoadedRef = React.useRef(false);
  const searchRequestRef = React.useRef(0);
  const mutationRequestRef = React.useRef(new Map<string, number>());
  const nextMutationRequestRef = React.useRef(0);

  // Use recording state from RecordingStateContext (single source of truth)
  const { isRecording } = useRecordingState();

  const pathname = usePathname();
  const router = useRouter();

  const replaceMeetings = React.useCallback((nextMeetings: CurrentMeeting[]) => {
    const merged = mergeMeetingCatalog(meetingsRef.current, nextMeetings);
    meetingsRef.current = merged;
    setMeetings(merged);
  }, []);

  const updateMeetings = React.useCallback((update: (current: CurrentMeeting[]) => CurrentMeeting[]) => {
    const next = update(meetingsRef.current);
    meetingsRef.current = next;
    setMeetings(next);
  }, []);

  const fetchMeetings = React.useCallback(async () => {
    const requestId = ++catalogRequestRef.current;
    setCatalogStatus(catalogLoadedRef.current ? 'refreshing' : 'loading');
    setCatalogError(null);
    try {
      const response = await invoke<CurrentMeeting[]>('api_get_meetings');
      if (requestId !== catalogRequestRef.current) return;
      replaceMeetings(response);
      catalogLoadedRef.current = true;
      setCatalogStatus('ready');
      Analytics.trackBackendConnection(true);
    } catch (error) {
      if (requestId !== catalogRequestRef.current) return;
      const message = errorMessage(error);
      console.error('Error fetching meetings:', error);
      setCatalogError(message);
      setCatalogStatus('error');
      Analytics.trackBackendConnection(false, message);
    }
  }, [replaceMeetings]);

  useEffect(() => {
    void fetchMeetings();
  }, [fetchMeetings]);

  useEffect(() => {
    const fetchSettings = async () => {
      setServerAddress('http://localhost:5167');
      setTranscriptServerAddress('http://127.0.0.1:8178/stream');
    };
    fetchSettings();
  }, []);

  const sidebarItems: SidebarItem[] = React.useMemo(() => [
    {
      id: 'meetings',
      title: 'Meeting Notes',
      type: 'folder' as const,
      children: [
        ...meetings.map(meeting => ({ id: meeting.id, title: meeting.title, type: 'file' as const }))
      ]
    },
  ], [meetings]);


  const toggleCollapse = () => {
    setIsCollapsed(!isCollapsed);
  };

  // Update current meeting when on home page
  useEffect(() => {
    if (pathname === '/') {
      setCurrentMeeting({ id: 'intro-call', title: '+ New Call' });
    }
  }, [pathname]);

  // A recording session lives on the meeting workspace. If the user navigates
  // away from it, this brings the split screen back instead of starting another
  // recording or showing the idle home page.
  const navigateAfterFlush = React.useCallback(async (navigate: () => void) => {
    setNavigationError(null);
    try {
      await flushNotes();
      navigate();
    } catch (error) {
      setNavigationError(errorMessage(error));
    }
  }, []);

  const openActiveRecordingWorkspace = React.useCallback(async () => {
    if (typeof window === 'undefined') return;
    const meetingId = sessionStorage.getItem(ACTIVE_RECORDING_MEETING_ID_KEY);
    if (!meetingId) return;
    await navigateAfterFlush(() => router.replace(meetingUrl(meetingId)));
  }, [navigateAfterFlush, router]);

  // Function to handle recording toggle from sidebar
  const handleRecordingToggle = async () => {
    if (isRecording) {
      // Recording is already live, so re-enter the workspace rather than
      // starting a second session.
      console.log('Recording in progress - returning to the recording workspace');
      await openActiveRecordingWorkspace();
      return;
    }

    // Check if already on home page
    if (pathname === '/') {
      // Already on home - trigger recording directly via custom event
      console.log('Triggering recording from sidebar (already on home page)');
      window.dispatchEvent(new CustomEvent('start-recording-from-sidebar'));
    } else {
      // Not on home - navigate and use auto-start mechanism
      console.log('Navigating to home page with auto-start flag');
      await navigateAfterFlush(() => {
        sessionStorage.setItem('autoStartRecording', 'true');
        router.push('/');
      });
    }

    // Track recording initiation from sidebar
    Analytics.trackButtonClick('start_recording', 'sidebar');
    // The actual recording start/stop is handled in the Home component
  };

  // Function to search through meeting transcripts
  const searchTranscripts = React.useCallback(async (query: string) => {
    const normalizedQuery = query.trim();
    const requestId = ++searchRequestRef.current;
    setSearchQuery(normalizedQuery);
    setSearchError(null);

    if (!normalizedQuery) {
      setSearchResults([]);
      setSearchStatus('idle');
      return;
    }

    try {
      setSearchStatus('searching');
      const results = await invoke<TranscriptSearchResult[]>('api_search_transcripts', { query: normalizedQuery });
      if (requestId !== searchRequestRef.current) return;
      const knownIds = new Set(meetingsRef.current.map((meeting) => meeting.id));
      // Search rows carry only a title. Keep catalog metadata authoritative and
      // omit unknown identities until a catalog refresh resolves them.
      setSearchResults(results.filter((result) => knownIds.has(result.id)));
      setSearchStatus('success');
    } catch (error) {
      if (requestId !== searchRequestRef.current) return;
      console.error('Error searching transcripts:', error);
      setSearchResults([]);
      setSearchError(errorMessage(error));
      setSearchStatus('error');
    }
  }, []);

  const setMutationStatus = React.useCallback((
    meetingId: string,
    kind: MeetingMutationKind,
    status?: MeetingMutationStatus,
  ) => {
    setMeetingMutations((current) => {
      const meeting = { ...current[meetingId] };
      if (status) meeting[kind] = status;
      else delete meeting[kind];
      if (Object.keys(meeting).length === 0) {
        const { [meetingId]: _removed, ...rest } = current;
        return rest;
      }
      return { ...current, [meetingId]: meeting };
    });
  }, []);

  const mutateMeeting = React.useCallback(async <K extends 'title' | 'pinned' | 'archived'>(
    meetingId: string,
    field: K,
    value: CurrentMeeting[K],
    kind: MeetingMutationKind,
    command: string,
    args: Record<string, unknown>,
  ) => {
    const meeting = meetingsRef.current.find((candidate) => candidate.id === meetingId);
    if (!meeting) throw new Error(`Meeting ${meetingId} is not in the catalog.`);

    const previousValue = meeting[field];
    const mutationKey = `${meetingId}:${kind}`;
    const requestId = ++nextMutationRequestRef.current;
    mutationRequestRef.current.set(mutationKey, requestId);
    updateMeetings((current) => patchMeeting(current, meetingId, { [field]: value }));
    if (field === 'title') {
      setCurrentMeeting((current) => current?.id === meetingId ? { ...current, title: String(value) } : current);
    }
    setMutationStatus(meetingId, kind, { status: 'pending', error: null });

    try {
      await invoke(command, args);
      if (mutationRequestRef.current.get(mutationKey) === requestId) {
        mutationRequestRef.current.delete(mutationKey);
        setMutationStatus(meetingId, kind);
      }
    } catch (error) {
      if (mutationRequestRef.current.get(mutationKey) === requestId) {
        mutationRequestRef.current.delete(mutationKey);
        updateMeetings((current) => current.map((candidate) => {
          if (candidate.id !== meetingId || candidate[field] !== value) return candidate;
          return { ...candidate, [field]: previousValue };
        }));
        if (field === 'title') {
          setCurrentMeeting((current) => (
            current?.id === meetingId && current.title === value
              ? { ...current, title: String(previousValue) }
              : current
          ));
        }
        setMutationStatus(meetingId, kind, { status: 'error', error: errorMessage(error) });
      }
      throw error;
    }
  }, [setMutationStatus, updateMeetings]);

  const renameMeeting = React.useCallback((meetingId: string, title: string) => (
    mutateMeeting(meetingId, 'title', title, 'rename', 'api_save_meeting_title', { meetingId, title })
  ), [mutateMeeting]);

  const setMeetingPinned = React.useCallback((meetingId: string, pinned: boolean) => (
    mutateMeeting(meetingId, 'pinned', pinned, 'pin', 'api_set_meeting_pinned', { meetingId, pinned })
  ), [mutateMeeting]);

  const setMeetingArchived = React.useCallback((meetingId: string, archived: boolean) => (
    mutateMeeting(meetingId, 'archived', archived, 'archive', 'api_set_meeting_archived', { meetingId, archived })
  ), [mutateMeeting]);

  const selectCatalogMeetings = React.useCallback(
    (visibility?: MeetingVisibility) => selectMeetings(meetings, visibility),
    [meetings],
  );
  const selectCatalogSearchResults = React.useCallback(
    (visibility?: MeetingVisibility) => selectSearchResults(searchResults, meetings, visibility),
    [meetings, searchResults],
  );

  // Summary polling management
  const stopSummaryPolling = React.useCallback((meetingId: string, processId?: string) => {
    const poll = summaryPollsRef.current.get(meetingId);
    if (!poll || (processId && poll.processId !== processId)) {
      return;
    }
    clearInterval(poll.timer);
    summaryPollsRef.current.delete(meetingId);
  }, []);

  const startSummaryPolling = React.useCallback((
    meetingId: string,
    processId: string,
    onUpdate: (result: SummaryProcessResponse) => void | Promise<void>
  ) => {
    stopSummaryPolling(meetingId);
    let pollCount = 0;
    const maxPolls = 200;
    const poll = async () => {
      const entry = summaryPollsRef.current.get(meetingId);
      if (!entry || entry.processId !== processId || entry.inFlight) {
        return;
      }
      entry.inFlight = true;
      try {
        pollCount += 1;
        if (pollCount >= maxPolls) {
          await onUpdate({
            status: 'error',
            meetingName: null,
            meeting_id: meetingId,
            start: processId,
            end: null,
            data: null,
            error: 'Summary generation timed out after 15 minutes. Please try again or check your model configuration.',
          });
          if (summaryPollsRef.current.get(meetingId) === entry) {
            stopSummaryPolling(meetingId, processId);
          }
          return;
        }

        const result = await invoke<SummaryProcessResponse>('api_get_summary', { meetingId });
        const current = summaryPollsRef.current.get(meetingId);
        if (current !== entry || result.start !== processId) {
          return;
        }
        await onUpdate(result);
        if (summaryPollsRef.current.get(meetingId) !== entry) return;
        if (
          result.status === 'completed'
          || result.status === 'error'
          || result.status === 'failed'
          || result.status === 'cancelled'
          || (result.status === 'idle' && pollCount > 1)
        ) {
          stopSummaryPolling(meetingId, processId);
        }
      } catch (error) {
        const current = summaryPollsRef.current.get(meetingId);
        if (current !== entry) {
          return;
        }
        try {
          await onUpdate({
            status: 'error',
            meetingName: null,
            meeting_id: meetingId,
            start: processId,
            end: null,
            data: null,
            error: error instanceof Error ? error.message : 'Unknown error',
          });
        } catch (callbackError) {
          console.error('Failed to handle summary polling error:', callbackError);
        } finally {
          if (summaryPollsRef.current.get(meetingId) === entry) {
            stopSummaryPolling(meetingId, processId);
          }
        }
      } finally {
        const current = summaryPollsRef.current.get(meetingId);
        if (current === entry) {
          current.inFlight = false;
        }
      }
    };

    const timer = setInterval(() => void poll(), 5000);
    summaryPollsRef.current.set(meetingId, { processId, timer, inFlight: false });
  }, [stopSummaryPolling]);

  useEffect(() => () => {
    summaryPollsRef.current.forEach(({ timer }) => clearInterval(timer));
    summaryPollsRef.current.clear();
  }, []);



  return (
    <SidebarContext.Provider value={{
      currentMeeting,
      setCurrentMeeting,
      sidebarItems,
      isCollapsed,
      toggleCollapse,
      meetings,
      setMeetings: replaceMeetings,
      catalogStatus,
      catalogError,
      isCatalogEmpty: catalogStatus === 'ready' && meetings.length === 0,
      selectMeetings: selectCatalogMeetings,
      isMeetingActive,
      setIsMeetingActive,
      handleRecordingToggle,
      openActiveRecordingWorkspace,
      navigationError,
      searchTranscripts,
      searchResults,
      isSearching: searchStatus === 'searching',
      searchQuery,
      searchStatus,
      searchError,
      hasNoSearchResults: searchStatus === 'success' && searchResults.length === 0,
      selectSearchResults: selectCatalogSearchResults,
      renameMeeting,
      setMeetingPinned,
      setMeetingArchived,
      meetingMutations,
      setServerAddress,
      serverAddress,
      transcriptServerAddress,
      setTranscriptServerAddress,
      startSummaryPolling,
      stopSummaryPolling,
      refetchMeetings: fetchMeetings,

    }}>
      {children}
    </SidebarContext.Provider>
  );
}
