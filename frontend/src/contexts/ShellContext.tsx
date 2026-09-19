'use client';

import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { usePathname } from 'next/navigation';
import { useRecordingController } from '@/contexts/RecordingControllerContext';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useMeetingNavigation } from '@/hooks/useNavigation';
import { useImportDialog } from '@/contexts/ImportDialogContext';
import { useConfig } from '@/contexts/ConfigContext';
import { settingsHref } from '@/components/settings/settingsSections';
import { useSidebar, type CurrentMeeting } from '@/components/Sidebar/SidebarProvider';
import { applyTheme, persistAndBroadcastTheme, readTheme, type AppTheme } from '@/lib/theme';

const COLLAPSED_KEY = 'meetily:sidebar-collapsed';

export class ShellNavigationError extends Error {
  constructor(error: unknown) {
    super(error instanceof Error ? error.message : String(error));
    this.name = 'ShellNavigationError';
  }
}

export function handleShellActionError(error: unknown, report: (message: string) => void): void {
  if (error instanceof ShellNavigationError) return;
  report(error instanceof Error ? error.message : String(error));
}

/** Below this viewport width the app switches to a compact, collapsed layout. */
export const COMPACT_BREAKPOINT = 1280;

interface ShellContextValue {
  collapsed: boolean;
  setCollapsed: (value: boolean) => void;
  toggleCollapsed: () => void;
  theme: AppTheme;
  setTheme: (theme: AppTheme) => void;
  toggleTheme: () => void;
  /** True when the window is narrow; the sidebar auto-collapses and docks stack. */
  compact: boolean;
  recordingActionLabel: 'New meeting' | 'Return to recording' | 'Stop recording' | 'Starting meeting' | 'Finishing meeting';
  recordingActionDisabled: boolean;
  runRecordingAction: () => Promise<void>;
  importActionLabel: 'Import recording' | 'Enable audio import';
  runImportAction: () => Promise<void>;
  navigate: (href: string) => Promise<void>;
  openMeeting: (meeting: CurrentMeeting) => Promise<void>;
  isNavigating: boolean;
  navigationError: Error | null;
  retryNavigation: () => Promise<void>;
  meetingSearchQuery: string;
  setMeetingSearchQuery: (query: string) => void;
  isMeetingSearchPending: boolean;
  focusMeetingsSearch: () => Promise<void>;
  meetingSearchFocusRequest: number | null;
  consumeMeetingSearchFocus: (requestId: number) => void;
}

interface MeetingSearchFocusIntent {
  requestId: number;
  sourcePath: string;
}

const ShellContext = createContext<ShellContextValue | null>(null);

export const SIDEBAR_WIDTH = 280;
export const SIDEBAR_COLLAPSED_WIDTH = 72;

export function ShellProvider({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const [collapsed, setCollapsedState] = useState(false);
  const [theme, setThemeState] = useState<AppTheme>(readTheme);
  const [compact, setCompact] = useState(false);
  const [meetingSearchQuery, setMeetingSearchQuery] = useState('');
  const [meetingSearchFocusIntent, setMeetingSearchFocusIntent] = useState<MeetingSearchFocusIntent | null>(null);
  const meetingSearchFocusRequestRef = useRef(0);
  // Remembers the user's own collapse choice so leaving compact mode restores it.
  const userCollapsedRef = useRef(false);

  const controller = useRecordingController();
  const recordingState = useRecordingState();
  const {
    navigate: navigateTo,
    openMeeting: openMeetingRoute,
    isNavigating,
    navigationError,
  } = useMeetingNavigation();
  const { openImportDialog } = useImportDialog();
  const { betaFeatures } = useConfig();
  const { searchTranscripts, searchStatus } = useSidebar();
  const retryNavigationRef = useRef<(() => Promise<void>) | null>(null);

  useLayoutEffect(() => {
    applyTheme(theme);
  }, [theme]);

  useEffect(() => {
    const storedCollapsed = localStorage.getItem(COLLAPSED_KEY) === 'true';
    userCollapsedRef.current = storedCollapsed;

    const applyViewport = () => {
      const isCompact = window.innerWidth < COMPACT_BREAKPOINT;
      setCompact(isCompact);
      setCollapsedState(isCompact ? true : userCollapsedRef.current);
    };
    applyViewport();
    window.addEventListener('resize', applyViewport);
    return () => window.removeEventListener('resize', applyViewport);
  }, []);

  useEffect(() => {
    const timer = setTimeout(() => void searchTranscripts(meetingSearchQuery), 250);
    return () => clearTimeout(timer);
  }, [meetingSearchQuery, searchTranscripts]);

  useEffect(() => {
    setMeetingSearchFocusIntent((current) => {
      if (!current || pathname === '/' || pathname === current.sourcePath) return current;
      return null;
    });
  }, [pathname]);

  const setCollapsed = useCallback((value: boolean) => {
    userCollapsedRef.current = value;
    setCollapsedState(value);
    localStorage.setItem(COLLAPSED_KEY, String(value));
  }, []);

  const toggleCollapsed = useCallback(() => {
    setCollapsedState((current) => {
      const next = !current;
      userCollapsedRef.current = next;
      localStorage.setItem(COLLAPSED_KEY, String(next));
      return next;
    });
  }, []);

  const setTheme = useCallback((next: AppTheme) => {
    setThemeState(next);
    void persistAndBroadcastTheme(next);
  }, []);

  const toggleTheme = useCallback(() => {
    setThemeState((current) => {
      const next = current === 'dark' ? 'light' : 'dark';
      void persistAndBroadcastTheme(next);
      return next;
    });
  }, []);

  const finalizing = controller.command === 'stop'
    || controller.command === 'finalize'
    || recordingState.isProcessing
    || recordingState.isSaving;
  const recordingActionDisabled = controller.isCommandPending || recordingState.isProcessing || recordingState.isSaving;
  const recordingActionLabel = controller.command === 'start'
    ? 'Starting meeting'
    : finalizing
      ? 'Finishing meeting'
      : recordingState.isRecording
        ? controller.activeMeetingId ? 'Return to recording' : 'Stop recording'
        : 'New meeting';
  const runRecordingAction = useCallback(async () => {
    if (recordingActionDisabled) return;
    if (recordingState.isRecording) {
      try {
        if (controller.activeMeetingId) await controller.returnToRecording();
        else await controller.stopRecording();
      } catch {
        // RecordingControllerFeedback is the single owner for command failures.
      }
      return;
    }
    await controller.startRecording({ source: 'app_shell' }).catch(() => {});
  }, [controller, recordingActionDisabled, recordingState.isRecording]);
  const importEnabled = betaFeatures.importAndRetranscribe;
  const navigate = useCallback(async (href: string) => {
    retryNavigationRef.current = () => navigateTo(href);
    try {
      await navigateTo(href);
    } catch (error) {
      throw new ShellNavigationError(error);
    }
  }, [navigateTo]);
  const openMeeting = useCallback(async (meeting: CurrentMeeting) => {
    retryNavigationRef.current = () => openMeetingRoute(meeting);
    try {
      await openMeetingRoute(meeting);
    } catch (error) {
      throw new ShellNavigationError(error);
    }
  }, [openMeetingRoute]);
  const retryNavigation = useCallback(async () => {
    await retryNavigationRef.current?.();
  }, []);
  const focusMeetingsSearch = useCallback(async () => {
    const requestId = ++meetingSearchFocusRequestRef.current;
    setMeetingSearchFocusIntent({ requestId, sourcePath: pathname });
    if (pathname === '/') return;

    try {
      await navigate('/');
    } catch (error) {
      setMeetingSearchFocusIntent((current) => current?.requestId === requestId ? null : current);
      throw error;
    }
  }, [navigate, pathname]);
  const consumeMeetingSearchFocus = useCallback((requestId: number) => {
    setMeetingSearchFocusIntent((current) => current?.requestId === requestId ? null : current);
  }, []);
  const runImportAction = useCallback(async () => {
    if (importEnabled) {
      openImportDialog();
      return;
    }
    await navigate(settingsHref('beta'));
  }, [importEnabled, navigate, openImportDialog]);

  const value = useMemo<ShellContextValue>(() => ({
    collapsed,
    setCollapsed,
    toggleCollapsed,
    theme,
    setTheme,
    toggleTheme,
    compact,
    recordingActionLabel,
    recordingActionDisabled,
    runRecordingAction,
    importActionLabel: importEnabled ? 'Import recording' : 'Enable audio import',
    runImportAction,
    navigate,
    openMeeting,
    isNavigating,
    navigationError,
    retryNavigation,
    meetingSearchQuery,
    setMeetingSearchQuery,
    isMeetingSearchPending: searchStatus === 'searching',
    focusMeetingsSearch,
    meetingSearchFocusRequest: meetingSearchFocusIntent?.requestId ?? null,
    consumeMeetingSearchFocus,
  }), [
    collapsed,
    compact,
    importEnabled,
    isNavigating,
    navigationError,
    meetingSearchQuery,
    meetingSearchFocusIntent,
    searchStatus,
    consumeMeetingSearchFocus,
    focusMeetingsSearch,
    navigate,
    openMeeting,
    recordingActionLabel,
    recordingActionDisabled,
    runImportAction,
    runRecordingAction,
    retryNavigation,
    setCollapsed,
    setTheme,
    theme,
    toggleCollapsed,
    toggleTheme,
  ]);

  return (
    <ShellContext.Provider value={value}>
      {children}
    </ShellContext.Provider>
  );
}

export function useShell() {
  const context = useContext(ShellContext);
  if (!context) {
    throw new Error('useShell must be used within a ShellProvider');
  }
  return context;
}
