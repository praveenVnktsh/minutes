'use client';

import { useEffect, useMemo, useRef, useState, type MouseEvent } from 'react';
import { usePathname, useRouter } from 'next/navigation';
import {
  Archive,
  ArchiveRestore,
  AudioLines,
  Bug,
  Home,
  Mic,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Pin,
  PinOff,
  Search,
  Settings,
  Sun,
  Upload,
  Video,
  X,
} from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useSidebar, type CurrentMeeting } from '@/components/Sidebar/SidebarProvider';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useDebugMode } from '@/hooks/useDebugMode';
import { setDebugMode } from '@/lib/debugMode';
import { useShell } from '@/contexts/ShellContext';
import { useImportDialog } from '@/contexts/ImportDialogContext';
import { useConfig } from '@/contexts/ConfigContext';

function formatMeetingDate(iso?: string): string {
  if (!iso) return '';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '';
  const now = new Date();
  if (date.toDateString() === now.toDateString()) {
    return `Today, ${date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}`;
  }
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) return 'Yesterday';
  return date.toLocaleDateString([], { month: 'short', day: 'numeric' });
}

export default function SimpleSidebar() {
  const router = useRouter();
  const pathname = usePathname();
  const { collapsed, toggleCollapsed, theme, toggleTheme } = useShell();
  const {
    meetings,
    currentMeeting,
    setCurrentMeeting,
    handleRecordingToggle,
    searchTranscripts,
    searchResults,
    isSearching,
    refetchMeetings,
  } = useSidebar();
  const { isRecording } = useRecordingState();
  const debugMode = useDebugMode();
  const { openImportDialog } = useImportDialog();
  const { betaFeatures } = useConfig();
  const [query, setQuery] = useState('');
  const [showArchived, setShowArchived] = useState(false);
  const searchInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const timer = setTimeout(() => void searchTranscripts(query), 250);
    return () => clearTimeout(timer);
  }, [query, searchTranscripts]);

  // Focus the search box when the command palette (or another surface) requests it.
  useEffect(() => {
    const onFocusSearch = () => {
      if (collapsed) toggleCollapsed();
      setTimeout(() => searchInputRef.current?.focus(), 50);
    };
    window.addEventListener('focus-sidebar-search', onFocusSearch);
    return () => window.removeEventListener('focus-sidebar-search', onFocusSearch);
  }, [collapsed, toggleCollapsed]);

  // Debug recordings are hidden unless debug mode is on.
  const scopedMeetings = useMemo(
    () => (debugMode ? meetings : meetings.filter((meeting) => !meeting.debug)),
    [meetings, debugMode],
  );

  const visibleMeetings = useMemo(() => {
    if (!query.trim()) return scopedMeetings;
    const byId = new Map(scopedMeetings.map((meeting) => [meeting.id, meeting]));
    return searchResults
      .map((result) => byId.get(result.id) ?? { id: result.id, title: result.title, created_at: undefined })
      .filter((meeting) => debugMode || !meeting.debug);
  }, [scopedMeetings, debugMode, query, searchResults]);

  const openMeeting = (meeting: { id: string; title: string; created_at?: string }) => {
    setCurrentMeeting(meeting);
    router.push(`/meeting-details?id=${meeting.id}`);
  };

  const togglePinned = async (event: MouseEvent, meeting: CurrentMeeting) => {
    event.stopPropagation();
    try {
      await invoke('api_set_meeting_pinned', { meetingId: meeting.id, pinned: !meeting.pinned });
      await refetchMeetings();
    } catch (error) {
      console.error('Failed to update pin:', error);
    }
  };

  const toggleArchived = async (event: MouseEvent, meeting: CurrentMeeting) => {
    event.stopPropagation();
    try {
      await invoke('api_set_meeting_archived', { meetingId: meeting.id, archived: !meeting.archived });
      await refetchMeetings();
    } catch (error) {
      console.error('Failed to update archive:', error);
    }
  };

  const searching = Boolean(query.trim());
  const pinnedMeetings = searching ? [] : visibleMeetings.filter((meeting) => meeting.pinned && !meeting.archived);
  const regularMeetings = searching ? visibleMeetings : visibleMeetings.filter((meeting) => !meeting.pinned && !meeting.archived);
  const archivedMeetings = searching ? [] : visibleMeetings.filter((meeting) => meeting.archived);
  const archivedCount = scopedMeetings.filter((meeting) => meeting.archived).length;

  const renderMeetingRow = (meeting: CurrentMeeting) => {
    const active = Boolean(pathname?.includes('/meeting-details')) && currentMeeting?.id === meeting.id;
    return (
      <div
        key={meeting.id}
        className={`group flex items-center gap-1 rounded-xl pr-1 transition ${active ? 'bg-surface-2' : 'hover:bg-surface-2'}`}
      >
        <button
          type="button"
          onClick={() => openMeeting(meeting)}
          title={meeting.title}
          className="flex min-w-0 flex-1 items-start gap-2.5 px-3 py-2.5 text-left"
        >
          <span className={`relative mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center ${active ? 'text-[#6ea8fe]' : 'text-ink-subtle'}`}>
            {active && <span className="absolute -left-3 top-1/2 h-5 w-0.5 -translate-y-1/2 rounded-full bg-[#6ea8fe]" />}
            <Video className="h-4 w-4" />
          </span>
          <span className="min-w-0 flex-1">
            <span className={`flex items-center gap-1.5 text-[13px] leading-5 ${active ? 'font-medium text-ink' : 'text-ink-muted'}`}>
              <span className="truncate">{meeting.title}</span>
              {meeting.debug && (
                <span className="shrink-0 rounded bg-amber-400/20 px-1 py-px text-[9px] font-semibold uppercase tracking-wide text-amber-600">
                  debug
                </span>
              )}
            </span>
            <span className="mt-0.5 block truncate text-[11px] text-ink-subtle">
              {formatMeetingDate(meeting.created_at)}
            </span>
          </span>
        </button>
        <div className="flex shrink-0 items-center opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
          <button
            type="button"
            onClick={(event) => togglePinned(event, meeting)}
            title={meeting.pinned ? 'Unpin' : 'Pin'}
            className="rounded p-1 text-ink-subtle hover:bg-surface-1 hover:text-ink"
          >
            {meeting.pinned ? <PinOff className="h-3.5 w-3.5" /> : <Pin className="h-3.5 w-3.5" />}
          </button>
          <button
            type="button"
            onClick={(event) => toggleArchived(event, meeting)}
            title={meeting.archived ? 'Unarchive' : 'Archive'}
            className="rounded p-1 text-ink-subtle hover:bg-surface-1 hover:text-ink"
          >
            {meeting.archived ? <ArchiveRestore className="h-3.5 w-3.5" /> : <Archive className="h-3.5 w-3.5" />}
          </button>
        </div>
      </div>
    );
  };

  const navItems = [
    { label: 'Home', icon: Home, active: pathname === '/', onClick: () => router.push('/') },
    {
      label: 'Meetings',
      icon: Video,
      active: Boolean(pathname?.includes('/meeting-details')),
      onClick: () => router.push('/'),
    },
  ];

  return (
    <aside
      className={`fixed inset-y-0 left-0 z-40 flex flex-col border-r border-hairline bg-surface-1 pb-4 text-ink transition-[width] duration-200 ease-out ${collapsed ? 'w-[72px] px-2' : 'w-[280px] px-3'
        }`}
    >
      <div className="titlebar absolute inset-x-0 top-0 h-7" />

      {/* Brand + collapse */}
      {collapsed ? (
        <div className="mt-8 flex flex-col items-center gap-3">
          <button
            type="button"
            onClick={() => router.push('/')}
            className="no-drag flex h-7 w-7 items-center justify-center rounded-[10px] bg-brand text-brand-foreground"
            title="minutes"
          >
            <AudioLines className="h-4 w-4" />
          </button>
          <button
            type="button"
            onClick={toggleCollapsed}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-ink-subtle hover:bg-surface-2 hover:text-ink"
            title="Expand sidebar"
          >
            <PanelLeftOpen className="h-4 w-4" />
          </button>
        </div>
      ) : (
        <div className="mt-8 flex items-center justify-between px-1">
          <button type="button" onClick={() => router.push('/')} className="no-drag flex items-center gap-2 text-left" title="minutes">
            <span className="flex h-7 w-7 items-center justify-center rounded-[10px] bg-brand text-brand-foreground">
              <AudioLines className="h-4 w-4" />
            </span>
            <span className="text-[15px] font-semibold tracking-[-0.02em]">minutes</span>
          </button>
          <button
            type="button"
            onClick={toggleCollapsed}
            className="rounded-lg p-1.5 text-ink-subtle hover:bg-surface-2 hover:text-ink"
            title="Collapse sidebar"
          >
            <PanelLeftClose className="h-4 w-4" />
          </button>
        </div>
      )}

      {/* New meeting */}
      <button
        type="button"
        onClick={handleRecordingToggle}
        title={isRecording ? 'Return to recording' : 'New meeting'}
        className={`mt-5 flex h-10 items-center gap-2 rounded-xl text-sm font-semibold transition ${collapsed ? 'w-full justify-center px-0' : 'w-full px-3'
          } ${isRecording
            ? 'bg-[#3a2320] text-[#e6938a] hover:opacity-90'
            : 'bg-brand text-brand-foreground hover:opacity-90'}`}
      >
        {isRecording ? (
          <span className="h-2 w-2 animate-pulse rounded-full bg-[#d74d3f]" />
        ) : (
          <Mic className="h-4 w-4" />
        )}
        {!collapsed && <span>{isRecording ? 'Return to recording' : 'New meeting'}</span>}
      </button>

      {/* Import recording */}
      {betaFeatures.importAndRetranscribe && (
        collapsed ? (
          <button
            type="button"
            onClick={() => openImportDialog()}
            title="Import recording"
            className="mt-2 flex h-10 w-full items-center justify-center rounded-xl text-ink-subtle hover:bg-surface-2 hover:text-ink"
          >
            <Upload className="h-4 w-4" />
          </button>
        ) : (
          <button
            type="button"
            onClick={() => openImportDialog()}
            className="mt-2 flex h-10 w-full items-center gap-3 rounded-xl px-3 text-sm text-ink-muted hover:bg-surface-2 hover:text-ink"
          >
            <Upload className="h-4 w-4" /> Import recording
          </button>
        )
      )}

      {/* Search */}
      {collapsed ? (
        <button
          type="button"
          onClick={() => {
            toggleCollapsed();
            setTimeout(() => searchInputRef.current?.focus(), 0);
          }}
          className="mt-3 flex h-10 w-full items-center justify-center rounded-xl text-ink-subtle hover:bg-surface-2 hover:text-ink"
          title="Search"
        >
          <Search className="h-4 w-4" />
        </button>
      ) : (
        <div className="relative mt-3">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-ink-subtle" />
          <input
            ref={searchInputRef}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search"
            className="h-10 w-full rounded-xl border border-hairline bg-surface-0 pl-9 pr-9 text-sm text-ink outline-none placeholder:text-ink-subtle focus:border-ink-subtle"
          />
          {query && (
            <button
              type="button"
              onClick={() => setQuery('')}
              className="absolute right-2.5 top-1/2 -translate-y-1/2 rounded p-1 text-ink-subtle hover:bg-surface-2"
              aria-label="Clear search"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
      )}

      {/* Primary nav */}
      <nav className="mt-4 space-y-1">
        {navItems.map(({ label, icon: Icon, active, onClick }) => (
          <button
            key={label}
            type="button"
            onClick={onClick}
            title={label}
            className={`flex h-10 w-full items-center gap-3 rounded-xl text-sm transition ${collapsed ? 'justify-center px-0' : 'px-3'
              } ${active ? 'bg-surface-2 font-medium text-ink' : 'text-ink-muted hover:bg-surface-2 hover:text-ink'}`}
          >
            <Icon className="h-4 w-4 shrink-0" />
            {!collapsed && <span>{label}</span>}
          </button>
        ))}
      </nav>

      {/* Recent - only meaningful when expanded */}
      {!collapsed && (
        <>
          <div className="mb-2 mt-6 flex items-center justify-between px-2">
            <span className="text-[11px] font-semibold uppercase tracking-[0.12em] text-ink-subtle">Recent</span>
            {isSearching && <span className="text-[11px] text-ink-subtle">Searching…</span>}
          </div>
          <nav className="custom-scrollbar min-h-0 flex-1 space-y-0.5 overflow-y-auto">
            {pinnedMeetings.length > 0 && (
              <>
                <p className="px-3 pb-1 pt-1 text-[10px] font-semibold uppercase tracking-[0.12em] text-ink-subtle">Pinned</p>
                {pinnedMeetings.map(renderMeetingRow)}
              </>
            )}
            {regularMeetings.map(renderMeetingRow)}
            {showArchived && archivedMeetings.length > 0 && (
              <>
                <p className="px-3 pb-1 pt-3 text-[10px] font-semibold uppercase tracking-[0.12em] text-ink-subtle">Archived</p>
                {archivedMeetings.map(renderMeetingRow)}
              </>
            )}
            {!isSearching && regularMeetings.length === 0 && pinnedMeetings.length === 0 && !(showArchived && archivedMeetings.length > 0) && (
              <p className="px-3 py-6 text-center text-xs leading-5 text-ink-subtle">Your meetings will appear here.</p>
            )}
          </nav>
          {(archivedCount > 0 || showArchived) && (
            <button
              type="button"
              onClick={() => setShowArchived((previous) => !previous)}
              className="mt-1 flex h-9 w-full items-center gap-2 rounded-xl px-3 text-xs text-ink-subtle transition hover:bg-surface-2 hover:text-ink"
            >
              <Archive className="h-3.5 w-3.5" />
              {showArchived ? 'Hide archived' : `Show archived (${archivedCount})`}
            </button>
          )}
        </>
      )}

      {collapsed && <div className="flex-1" />}

      {/* Footer */}
      <div className={collapsed ? 'flex flex-col items-center gap-2 border-t border-hairline pt-3' : 'flex items-center gap-2 border-t border-hairline px-1 pt-3'}>
        {debugMode && (
          <button
            type="button"
            onClick={() => void setDebugMode(false)}
            title="Debug mode is on — click to turn it off"
            className={`rounded-lg p-2 text-amber-500 hover:bg-amber-500/10 ${collapsed ? '' : 'flex items-center gap-1.5'}`}
          >
            <Bug className="h-4 w-4" />
            {!collapsed && <span className="text-xs font-medium">Debug on</span>}
          </button>
        )}
        <button
          type="button"
          onClick={() => router.push('/settings')}
          className={`rounded-lg p-2 hover:bg-surface-2 hover:text-ink ${pathname === '/settings' ? 'text-ink' : 'text-ink-subtle'}`}
          title="Settings"
        >
          <Settings className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={toggleTheme}
          className="rounded-lg p-2 text-ink-subtle hover:bg-surface-2 hover:text-ink"
          title={theme === 'dark' ? 'Light mode' : 'Dark mode'}
        >
          {theme === 'dark' ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
        </button>
      </div>
    </aside>
  );
}
