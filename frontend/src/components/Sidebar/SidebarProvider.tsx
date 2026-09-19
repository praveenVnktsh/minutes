'use client';

import React, { createContext, useContext, useState, useEffect } from 'react';
import { usePathname, useRouter } from 'next/navigation';
import Analytics from '@/lib/analytics';
import { invoke } from '@tauri-apps/api/core';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { flushNotes } from '@/services/notePersistenceService';
import type { SummaryProcessResponse } from '@/types';
import { meetingActivityStore } from '@/contexts/MeetingActivityContext';
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

type MutableMeetingField = 'title' | 'pinned' | 'archived';

interface MeetingMutationQueue {
  acknowledgedValue: CurrentMeeting[MutableMeetingField];
  latestRequestId: number;
  pendingCount: number;
  lastSuccessRevision: number;
  tail: Promise<void>;
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
  const meetingsRef = React.useRef<CurrentMeeting[]>([]);
  const catalogRequestRef = React.useRef(0);
  const catalogLoadedRef = React.useRef(false);
  const searchRequestRef = React.useRef(0);
  const mutationQueuesRef = React.useRef(new Map<string, MeetingMutationQueue>());
  const nextMutationRequestRef = React.useRef(0);
  const mutationSuccessRevisionRef = React.useRef(0);
  const navigationRequestRef = React.useRef(0);

  // Use recording state from RecordingStateContext (single source of truth)
  const { isRecording } = useRecordingState();

  const pathname = usePathname();
  const router = useRouter();

  const replaceMeetings = React.useCallback((
    nextMeetings: CurrentMeeting[],
    preserveSuccessesAfter = Number.POSITIVE_INFINITY,
  ) => {
    const currentById = new Map(meetingsRef.current.map((meeting) => [meeting.id, meeting]));
    const merged = mergeMeetingCatalog(meetingsRef.current, nextMeetings);
    const protectedMeetingIds = new Set<string>();

    for (const meeting of merged) {
      const current = currentById.get(meeting.id);
      if (!current) continue;
      for (const [field, kind] of [
        ['title', 'rename'],
        ['pinned', 'pin'],
        ['archived', 'archive'],
      ] as const) {
        const queue = mutationQueuesRef.current.get(`${meeting.id}:${kind}`);
        if (!queue) continue;
        if (queue.pendingCount > 0 || queue.lastSuccessRevision > preserveSuccessesAfter) {
          meeting[field] = current[field] as never;
          protectedMeetingIds.add(meeting.id);
        } else {
          queue.acknowledgedValue = meeting[field];
        }
      }
    }

    for (const meeting of meetingsRef.current) {
      if (!merged.some(({ id }) => id === meeting.id)) {
        const hasProtectedMutation = [...mutationQueuesRef.current.entries()].some(([key, queue]) => (
          key.startsWith(`${meeting.id}:`)
          && (queue.pendingCount > 0 || queue.lastSuccessRevision > preserveSuccessesAfter)
        ));
        if (hasProtectedMutation) protectedMeetingIds.add(meeting.id);
      }
    }
    for (const meetingId of protectedMeetingIds) {
      if (!merged.some(({ id }) => id === meetingId)) {
        const current = currentById.get(meetingId);
        if (current) merged.push(current);
      }
    }

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
    const mutationSuccessAtStart = mutationSuccessRevisionRef.current;
    setCatalogStatus(catalogLoadedRef.current ? 'refreshing' : 'loading');
    setCatalogError(null);
    try {
      const response = await invoke<CurrentMeeting[]>('api_get_meetings');
      if (requestId !== catalogRequestRef.current) return;
      replaceMeetings(response, mutationSuccessAtStart);
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
    const requestId = ++navigationRequestRef.current;
    setNavigationError(null);
    try {
      await flushNotes();
      if (requestId !== navigationRequestRef.current) return;
      navigate();
    } catch (error) {
      if (requestId !== navigationRequestRef.current) return;
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

  const mutateMeeting = React.useCallback(<K extends MutableMeetingField>(
    meetingId: string,
    field: K,
    value: CurrentMeeting[K],
    kind: MeetingMutationKind,
    command: string,
    args: Record<string, unknown>,
  ): Promise<void> => {
    const meeting = meetingsRef.current.find((candidate) => candidate.id === meetingId);
    if (!meeting) return Promise.reject(new Error(`Meeting ${meetingId} is not in the catalog.`));

    const mutationKey = `${meetingId}:${kind}`;
    const requestId = ++nextMutationRequestRef.current;
    let queue = mutationQueuesRef.current.get(mutationKey);
    if (!queue) {
      queue = {
        acknowledgedValue: meeting[field],
        latestRequestId: requestId,
        pendingCount: 0,
        lastSuccessRevision: 0,
        tail: Promise.resolve(),
      };
      mutationQueuesRef.current.set(mutationKey, queue);
    }
    queue.latestRequestId = requestId;
    queue.pendingCount += 1;
    updateMeetings((current) => patchMeeting(current, meetingId, { [field]: value }));
    if (field === 'title') {
      setCurrentMeeting((current) => current?.id === meetingId ? { ...current, title: String(value) } : current);
    }
    setMutationStatus(meetingId, kind, { status: 'pending', error: null });

    const operation = queue.tail.then(async () => {
      try {
        await invoke(command, args);
        queue.acknowledgedValue = value;
        queue.lastSuccessRevision = ++mutationSuccessRevisionRef.current;
        if (queue.latestRequestId === requestId) {
          setMutationStatus(meetingId, kind);
        }
      } catch (error) {
        if (queue.latestRequestId === requestId) {
          const acknowledgedValue = queue.acknowledgedValue;
          updateMeetings((current) => patchMeeting(current, meetingId, { [field]: acknowledgedValue }));
          if (field === 'title') {
            setCurrentMeeting((current) => current?.id === meetingId
              ? { ...current, title: String(acknowledgedValue) }
              : current);
          }
          setMutationStatus(meetingId, kind, { status: 'error', error: errorMessage(error) });
        }
        throw error;
      } finally {
        queue.pendingCount -= 1;
      }
    });
    queue.tail = operation.catch(() => {});
    return operation;
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

  // Compatibility facade. The process-wide activity registry owns polling so
  // route unmounts and multiple consumers cannot create competing timers.
  const stopSummaryPolling = React.useCallback((meetingId: string, processId?: string) => {
    meetingActivityStore.stopSummaryPolling(meetingId, processId);
  }, []);

  const startSummaryPolling = React.useCallback((
    meetingId: string,
    processId: string,
    onUpdate: (result: SummaryProcessResponse) => void | Promise<void>
  ) => {
    meetingActivityStore.startSummaryPolling(meetingId, processId, onUpdate);
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
