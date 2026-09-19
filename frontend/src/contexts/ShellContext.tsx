'use client';

import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useRecordingController } from '@/contexts/RecordingControllerContext';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useMeetingNavigation } from '@/hooks/useNavigation';
import { useImportDialog } from '@/contexts/ImportDialogContext';
import { useConfig } from '@/contexts/ConfigContext';
import { settingsHref } from '@/components/settings/settingsSections';
import type { CurrentMeeting } from '@/components/Sidebar/SidebarProvider';
import { applyTheme, persistAndBroadcastTheme, readTheme, type AppTheme } from '@/lib/theme';

const COLLAPSED_KEY = 'meetily:sidebar-collapsed';

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
  recordingActionLabel: 'New meeting' | 'Return to recording' | 'Stop recording';
  runRecordingAction: () => Promise<void>;
  importActionLabel: 'Import recording' | 'Enable audio import';
  runImportAction: () => Promise<void>;
  navigate: (href: string) => Promise<void>;
  openMeeting: (meeting: CurrentMeeting) => Promise<void>;
  isNavigating: boolean;
  navigationError: Error | null;
}

const ShellContext = createContext<ShellContextValue | null>(null);

export const SIDEBAR_WIDTH = 280;
export const SIDEBAR_COLLAPSED_WIDTH = 72;

export function ShellProvider({ children }: { children: React.ReactNode }) {
  const [collapsed, setCollapsedState] = useState(false);
  const [theme, setThemeState] = useState<AppTheme>(readTheme);
  const [compact, setCompact] = useState(false);
  // Remembers the user's own collapse choice so leaving compact mode restores it.
  const userCollapsedRef = useRef(false);

  const controller = useRecordingController();
  const recordingState = useRecordingState();
  const meetingNavigation = useMeetingNavigation();
  const { openImportDialog } = useImportDialog();
  const { betaFeatures } = useConfig();

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

  const hasNativeSession = recordingState.isRecording;
  const recordingActionLabel = hasNativeSession
    ? controller.activeMeetingId ? 'Return to recording' : 'Stop recording'
    : 'New meeting';
  const runRecordingAction = useCallback(async () => {
    if (recordingState.isRecording) {
      if (controller.activeMeetingId) await controller.returnToRecording();
      else await controller.stopRecording();
      return;
    }
    await controller.startRecording({ source: 'app_shell' });
  }, [controller, recordingState.isRecording]);
  const importEnabled = betaFeatures.importAndRetranscribe;
  const runImportAction = useCallback(async () => {
    if (importEnabled) {
      openImportDialog();
      return;
    }
    await meetingNavigation.navigate(settingsHref('beta'));
  }, [importEnabled, meetingNavigation, openImportDialog]);

  const value = useMemo<ShellContextValue>(() => ({
    collapsed,
    setCollapsed,
    toggleCollapsed,
    theme,
    setTheme,
    toggleTheme,
    compact,
    recordingActionLabel,
    runRecordingAction,
    importActionLabel: importEnabled ? 'Import recording' : 'Enable audio import',
    runImportAction,
    navigate: meetingNavigation.navigate,
    openMeeting: meetingNavigation.openMeeting,
    isNavigating: meetingNavigation.isNavigating,
    navigationError: meetingNavigation.navigationError,
  }), [
    collapsed,
    compact,
    importEnabled,
    meetingNavigation.isNavigating,
    meetingNavigation.navigate,
    meetingNavigation.navigationError,
    meetingNavigation.openMeeting,
    recordingActionLabel,
    runImportAction,
    runRecordingAction,
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
