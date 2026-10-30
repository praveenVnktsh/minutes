'use client';

import { useMemo, useRef, useState } from 'react';
import { usePathname } from 'next/navigation';
import {
  AudioLines,
  Bug,
  Clapperboard,
  MessageSquare,
  Mic,
  PanelLeftClose,
  PanelLeftOpen,
  Settings,
  Video,
} from 'lucide-react';
import { useSidebar, type CurrentMeeting } from '@/components/Sidebar/SidebarProvider';
import { useDebugMode } from '@/hooks/useDebugMode';
import { useReplaysEnabled } from '@/lib/developerSettings';
import { setDebugMode } from '@/lib/debugMode';
import { handleShellActionError, useShell } from '@/contexts/ShellContext';
import { SendFeedbackDialog } from '@/components/SendFeedbackDialog';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import { StatusFeedback } from '@/components/ui/status-feedback';

/** How many meetings the rail lists; the library at / holds the rest. */
export const SIDEBAR_RECENT_LIMIT = 5;

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

function createdAtMillis(meeting: CurrentMeeting): number {
  const time = meeting.created_at ? Date.parse(meeting.created_at) : Number.NaN;
  return Number.isNaN(time) ? 0 : time;
}

/** The newest unarchived meetings, newest first. */
export function selectRecentMeetings(meetings: readonly CurrentMeeting[], limit = SIDEBAR_RECENT_LIMIT): CurrentMeeting[] {
  return meetings
    .filter((meeting) => !meeting.archived)
    .map((meeting, index) => ({ meeting, index }))
    // Stable on equal dates, so the catalog's own order breaks ties.
    .sort((left, right) => createdAtMillis(right.meeting) - createdAtMillis(left.meeting) || left.index - right.index)
    .slice(0, limit)
    .map(({ meeting }) => meeting);
}

/**
 * The app's navigation rail: New meeting, the Meetings library, a few recent
 * meetings and Settings. Search, import, pinning and archiving live in the
 * library (and the ⌘K palette), not here.
 */
export default function SimpleSidebar() {
  const pathname = usePathname();
  const {
    collapsed,
    toggleCollapsed,
    recordingActionLabel,
    runRecordingAction,
    navigate,
    openMeeting,
    recordingActionDisabled,
  } = useShell();
  const { currentMeeting, selectMeetings } = useSidebar();
  const { activeMeetingId, getMeetingActivities, getSummaryActivity } = useMeetingActivity();
  const debugMode = useDebugMode();
  const replaysEnabled = useReplaysEnabled();
  const [showFeedback, setShowFeedback] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const feedbackTriggerRef = useRef<HTMLButtonElement>(null);

  const runShellAction = async (action: () => Promise<void>) => {
    setActionError(null);
    try {
      await action();
    } catch (error) {
      handleShellActionError(error, setActionError);
    }
  };

  const catchNavigation = (error: unknown) => handleShellActionError(error, setActionError);

  // Debug recordings are hidden unless debug mode is on.
  const recentMeetings = useMemo(
    () => selectRecentMeetings(selectMeetings({ debugMode })),
    [debugMode, selectMeetings],
  );

  // A meeting that is recording or has background work shows one small dot:
  // the meeting's own page owns the words for it.
  const meetingStatus = (meeting: CurrentMeeting): 'live' | 'busy' | null => {
    if (activeMeetingId === meeting.id) return 'live';
    const busy = getMeetingActivities(meeting.id).some((activity) => ['starting', 'queued', 'transcribing', 'saving'].includes(activity.status));
    const summary = getSummaryActivity?.(meeting.id);
    if (busy || summary?.status === 'queued' || summary?.status === 'processing') return 'busy';
    return null;
  };

  const renderMeetingRow = (meeting: CurrentMeeting) => {
    const active = Boolean(pathname?.includes('/meeting-details')) && currentMeeting?.id === meeting.id;
    const status = meetingStatus(meeting);
    return (
      <button
        key={meeting.id}
        type="button"
        onClick={() => void openMeeting(meeting).catch(catchNavigation)}
        title={meeting.title}
        aria-current={active ? 'page' : undefined}
        className={`flex w-full min-w-0 items-start gap-2.5 rounded-xl px-3 py-2 text-left transition ${active ? 'bg-surface-2' : 'hover:bg-surface-2'}`}
      >
        <span className="min-w-0 flex-1">
          <span className={`flex items-center gap-1.5 text-[13px] leading-5 ${active ? 'font-medium text-ink' : 'text-ink-muted'}`}>
            <span className="truncate">{meeting.title || 'Untitled meeting'}</span>
            {status && (
              <span
                className={`h-1.5 w-1.5 shrink-0 rounded-full ${status === 'live' ? 'animate-pulse bg-recording' : 'bg-info'}`}
                aria-label={status === 'live' ? 'Recording' : 'Working'}
                role="img"
              />
            )}
            {meeting.debug && (
              <span className="shrink-0 rounded bg-warning-subtle px-1 py-px text-[9px] font-semibold uppercase tracking-wide text-warning">
                debug
              </span>
            )}
          </span>
          <span className="block truncate text-[11px] text-ink-subtle">
            {formatMeetingDate(meeting.created_at)}
          </span>
        </span>
      </button>
    );
  };

  const navItems = [{
    label: 'Meetings',
    icon: Video,
    active: pathname === '/' || Boolean(pathname?.includes('/meeting-details')),
    onClick: () => void navigate('/').catch(catchNavigation),
  }, ...(replaysEnabled ? [{
    label: 'Replays',
    icon: Clapperboard,
    active: Boolean(pathname?.startsWith('/replays')),
    onClick: () => void navigate('/replays').catch(catchNavigation),
  }] : [])];

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
            onClick={() => void navigate('/').catch(catchNavigation)}
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
          <button type="button" onClick={() => void navigate('/').catch(catchNavigation)} className="no-drag flex items-center gap-2 text-left" title="minutes">
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

      {/* New meeting. On the live meeting's own workspace it is only a status label. */}
      {recordingActionLabel === 'Recording' ? (
        <div
          role="status"
          title="Recording"
          className={`mt-5 flex h-10 items-center gap-2 rounded-xl bg-recording-subtle text-sm font-semibold text-recording ${collapsed ? 'w-full justify-center px-0' : 'w-full px-3'}`}
        >
          <span className="h-2 w-2 animate-pulse rounded-full bg-recording" />
          {!collapsed && <span>Recording</span>}
        </div>
      ) : (
        <button
          type="button"
          onClick={() => void runShellAction(runRecordingAction)}
          disabled={recordingActionDisabled}
          title={recordingActionLabel}
          aria-disabled={recordingActionDisabled}
          className={`mt-5 flex h-10 items-center gap-2 rounded-xl text-sm font-semibold transition ${collapsed ? 'w-full justify-center px-0' : 'w-full px-3'
            } ${recordingActionLabel !== 'New meeting'
              ? 'bg-recording-subtle text-recording hover:opacity-90'
              : 'bg-brand text-brand-foreground hover:opacity-90'}`}
        >
          {recordingActionLabel !== 'New meeting' ? (
            <span className="h-2 w-2 animate-pulse rounded-full bg-recording" />
          ) : (
            <Mic className="h-4 w-4" />
          )}
          {!collapsed && <span>{recordingActionLabel}</span>}
        </button>
      )}

      {/* Primary nav */}
      <nav className="mt-4 space-y-1">
        {navItems.map(({ label, icon: Icon, active, onClick }) => (
          <button
            key={label}
            type="button"
            onClick={onClick}
            title={label}
            aria-current={active ? 'page' : undefined}
            className={`flex h-10 w-full items-center gap-3 rounded-xl text-sm transition ${collapsed ? 'justify-center px-0' : 'px-3'
              } ${active ? 'bg-surface-2 font-medium text-ink' : 'text-ink-muted hover:bg-surface-2 hover:text-ink'}`}
          >
            <Icon className="h-4 w-4 shrink-0" />
            {!collapsed && <span>{label}</span>}
          </button>
        ))}
      </nav>

      {/* Recent meetings - only shown when expanded */}
      {!collapsed && (
        <>
          <div className="mb-1 mt-6 px-2">
            <span className="text-[11px] font-semibold uppercase tracking-[0.12em] text-ink-subtle">Recent</span>
          </div>
          <nav aria-label="Recent meetings" className="custom-scrollbar min-h-0 flex-1 space-y-0.5 overflow-y-auto">
            {recentMeetings.map(renderMeetingRow)}
            {actionError && <StatusFeedback tone="error" className="mx-2 text-xs">{actionError}</StatusFeedback>}
            {recentMeetings.length === 0 && (
              <p className="px-3 py-6 text-center text-xs leading-5 text-ink-subtle">Your meetings will appear here.</p>
            )}
          </nav>
        </>
      )}

      {collapsed && <div className="flex-1" />}

      {/* Footer */}
      <div className={collapsed ? 'flex flex-col items-center gap-2 border-t border-hairline pt-3' : 'flex flex-col gap-1 border-t border-hairline px-1 pt-3'}>
        <div className={collapsed ? 'flex flex-col items-center gap-2' : 'flex items-center gap-2'}>
          {debugMode && (
            <button
              type="button"
              onClick={() => void setDebugMode(false)}
              title="Debug mode is on — click to turn it off"
              className={`rounded-lg p-2 text-warning hover:bg-warning-subtle ${collapsed ? '' : 'flex items-center gap-1.5'}`}
            >
              <Bug className="h-4 w-4" />
              {!collapsed && <span className="text-xs font-medium">Debug on</span>}
            </button>
          )}
          <button
            type="button"
            onClick={() => void navigate('/settings').catch(catchNavigation)}
            className={`rounded-lg p-2 hover:bg-surface-2 hover:text-ink ${collapsed ? '' : 'flex items-center gap-1.5'} ${pathname?.startsWith('/settings') ? 'text-ink' : 'text-ink-subtle'}`}
            title="Settings"
            aria-label="Settings"
            aria-current={pathname?.startsWith('/settings') ? 'page' : undefined}
          >
            <Settings className="h-4 w-4" />
            {!collapsed && <span className="text-xs font-medium">Settings</span>}
          </button>
          {collapsed && (
            <button
              ref={feedbackTriggerRef}
              type="button"
              onClick={() => setShowFeedback(true)}
              title="Send feedback"
              className="rounded-lg p-2 text-ink-subtle hover:bg-surface-2 hover:text-ink"
            >
              <MessageSquare className="h-4 w-4" />
            </button>
          )}
        </div>
        {!collapsed && (
          <button
            ref={feedbackTriggerRef}
            type="button"
            onClick={() => setShowFeedback(true)}
            className="flex h-9 w-full items-center gap-2 rounded-xl px-3 text-xs text-ink-subtle transition hover:bg-surface-2 hover:text-ink"
          >
            <MessageSquare className="h-3.5 w-3.5" /> Send feedback
          </button>
        )}
      </div>

      <SendFeedbackDialog open={showFeedback} onOpenChange={setShowFeedback} returnFocusRef={feedbackTriggerRef} />
    </aside>
  );
}
