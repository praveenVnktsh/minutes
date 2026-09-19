'use client'

import { useEffect, useMemo, useRef, useState, type MouseEvent } from 'react'
import { Archive, ArchiveRestore, CalendarDays, Mic, Pin, PinOff, Search, Upload, X } from 'lucide-react'
import { useSidebar, type CurrentMeeting } from '@/components/Sidebar/SidebarProvider'
import { useShell } from '@/contexts/ShellContext'
import { useDebugMode } from '@/hooks/useDebugMode'
import { useMeetingActivity } from '@/contexts/MeetingActivityContext'
import { Button } from '@/components/ui/button'
import { StatusFeedback } from '@/components/ui/status-feedback'

type LibraryFilter = 'all' | 'pinned' | 'archived'

function formatDate(value?: string): string {
  if (!value) return 'Date unavailable'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return 'Date unavailable'
  return date.toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' })
}

export function MeetingsLibrary() {
  const {
    catalogStatus,
    catalogError,
    searchStatus,
    searchError,
    searchTranscripts,
    selectMeetings,
    selectSearchResults,
    refetchMeetings,
    setMeetingPinned,
    setMeetingArchived,
    meetingMutations,
  } = useSidebar()
  const {
    recordingActionLabel,
    runRecordingAction,
    importActionLabel,
    runImportAction,
    openMeeting,
    isNavigating,
    navigationError,
  } = useShell()
  const { activeMeetingId, getMeetingActivities } = useMeetingActivity()
  const debugMode = useDebugMode()
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState<LibraryFilter>('all')
  const [actionError, setActionError] = useState<string | null>(null)
  const searchRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    const timer = setTimeout(() => void searchTranscripts(query), 250)
    return () => clearTimeout(timer)
  }, [query, searchTranscripts])

  useEffect(() => {
    const focus = () => searchRef.current?.focus()
    window.addEventListener('focus-meetings-search', focus)
    return () => window.removeEventListener('focus-meetings-search', focus)
  }, [])

  const visibility = useMemo(() => ({
    includeArchived: filter === 'archived',
    debugMode,
    pinned: filter === 'pinned' ? true : undefined,
  }), [debugMode, filter])
  const meetings = query.trim() ? selectSearchResults(visibility) : selectMeetings(visibility)
  const visibleMeetings = filter === 'archived'
    ? meetings.filter((meeting) => meeting.archived)
    : meetings
  const loading = catalogStatus === 'loading'
  const searching = query.trim() && searchStatus === 'searching'

  const runMutation = async (action: () => Promise<void>) => {
    setActionError(null)
    try {
      await action()
    } catch (error) {
      setActionError(error instanceof Error ? error.message : String(error))
    }
  }

  const togglePin = (event: MouseEvent, meeting: CurrentMeeting) => {
    event.stopPropagation()
    void runMutation(() => setMeetingPinned(meeting.id, !meeting.pinned))
  }
  const toggleArchive = (event: MouseEvent, meeting: CurrentMeeting) => {
    event.stopPropagation()
    void runMutation(() => setMeetingArchived(meeting.id, !meeting.archived))
  }

  return (
    <div className="h-screen overflow-y-auto bg-surface-0 px-5 pb-24 pt-12 sm:px-8 lg:px-12">
      <div className="mx-auto max-w-6xl">
        <header className="flex flex-col gap-5 border-b border-hairline pb-7 sm:flex-row sm:items-end sm:justify-between">
          <div>
            <p className="text-xs font-semibold uppercase tracking-[0.16em] text-ink-subtle">Library</p>
            <h1 className="mt-1 font-serif text-4xl font-semibold tracking-[-0.035em] text-ink">Meetings</h1>
            <p className="mt-2 max-w-xl text-sm leading-6 text-ink-muted">Record a conversation, return to active work, or find notes from an earlier meeting.</p>
          </div>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={() => void runMutation(runImportAction)}><Upload />{importActionLabel}</Button>
            <Button variant={recordingActionLabel === 'Stop recording' ? 'recording' : 'default'} onClick={() => void runMutation(runRecordingAction)}>
              <Mic />{recordingActionLabel}
            </Button>
          </div>
        </header>

        <section className="mt-6" aria-label="Meeting filters">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
            <div className="relative flex-1">
              <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-ink-subtle" />
              <input
                ref={searchRef}
                aria-label="Search meetings"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Search titles and transcripts"
                className="h-11 w-full rounded-xl border border-hairline bg-surface-raised pl-10 pr-10 text-sm text-ink outline-none placeholder:text-ink-subtle focus-visible:ring-2 focus-visible:ring-focus"
              />
              {query && <button type="button" aria-label="Clear search" onClick={() => setQuery('')} className="absolute right-3 top-1/2 -translate-y-1/2 rounded p-1 text-ink-subtle hover:bg-surface-2"><X className="h-4 w-4" /></button>}
            </div>
            <div className="flex rounded-xl border border-hairline bg-surface-raised p-1" role="group" aria-label="Filter meetings">
              {(['all', 'pinned', 'archived'] as const).map((value) => (
                <button
                  key={value}
                  type="button"
                  aria-pressed={filter === value}
                  onClick={() => setFilter(value)}
                  className={`rounded-lg px-3 py-2 text-xs font-semibold capitalize ${filter === value ? 'bg-selected text-selected-foreground' : 'text-ink-muted hover:bg-surface-2'}`}
                >
                  {value}
                </button>
              ))}
            </div>
          </div>
        </section>

        <div className="mt-5 space-y-3">
          {catalogStatus === 'refreshing' && <StatusFeedback pending tone="info">Refreshing meetings…</StatusFeedback>}
          {catalogStatus === 'error' && (
            <StatusFeedback tone="error" actionLabel="Retry" onAction={() => void refetchMeetings()}>
              Could not load meetings{catalogError ? `: ${catalogError}` : '.'}
            </StatusFeedback>
          )}
          {searchStatus === 'error' && <StatusFeedback tone="error">Search failed{searchError ? `: ${searchError}` : '.'}</StatusFeedback>}
          {(actionError || navigationError) && <StatusFeedback tone="error">{actionError ?? navigationError?.message}</StatusFeedback>}
        </div>

        {(loading || searching) ? (
          <div className="mt-12 flex items-center justify-center gap-2 text-sm text-ink-muted" role="status">
            <span className="h-4 w-4 animate-spin rounded-full border-2 border-hairline border-t-info" />
            {loading ? 'Loading meetings…' : 'Searching all transcripts…'}
          </div>
        ) : visibleMeetings.length === 0 && catalogStatus !== 'error' && searchStatus !== 'error' ? (
          <div className="mt-12 rounded-2xl border border-dashed border-hairline bg-surface-raised px-6 py-14 text-center">
            <CalendarDays className="mx-auto h-8 w-8 text-ink-subtle" />
            <h2 className="mt-4 text-lg font-semibold text-ink">
              {query.trim() ? 'No meetings match your search' : filter === 'archived' ? 'No archived meetings' : filter === 'pinned' ? 'No pinned meetings' : 'Your first meeting starts here'}
            </h2>
            <p className="mx-auto mt-2 max-w-md text-sm text-ink-muted">
              {query.trim() ? 'Try a different phrase or clear the search.' : 'Start a meeting to create a local recording, transcript, and notes workspace.'}
            </p>
            {!query.trim() && filter === 'all' && <Button className="mt-5" onClick={() => void runMutation(runRecordingAction)}><Mic />{recordingActionLabel}</Button>}
          </div>
        ) : (
          <ul className="mt-6 grid gap-3 md:grid-cols-2 xl:grid-cols-3" aria-busy={isNavigating}>
            {visibleMeetings.map((meeting) => {
              const mutation = meetingMutations[meeting.id]
              const pending = mutation?.pin?.status === 'pending' || mutation?.archive?.status === 'pending'
              const activity = getMeetingActivities(meeting.id).find((item) => !['ready', 'cancelled'].includes(item.status))
              const active = meeting.id === activeMeetingId
              return (
                <li key={meeting.id}>
                  <article className={`group h-full rounded-2xl border bg-surface-raised p-4 transition hover:-translate-y-0.5 hover:shadow-md ${active ? 'border-recording/60' : 'border-hairline'}`}>
                    <button type="button" className="w-full text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus" onClick={() => void openMeeting(meeting)}>
                      <div className="flex items-start justify-between gap-3">
                        <h2 className="line-clamp-2 text-base font-semibold leading-6 text-ink">{meeting.title || 'Untitled meeting'}</h2>
                        {active && <span className="shrink-0 rounded-full bg-recording-subtle px-2 py-1 text-[10px] font-semibold uppercase tracking-wide text-recording">Live</span>}
                      </div>
                      <p className="mt-3 text-xs text-ink-subtle">{formatDate(meeting.created_at)}</p>
                      {activity && <p className="mt-2 truncate text-xs font-medium text-info">{activity.message ?? activity.stage ?? activity.status}</p>}
                    </button>
                    <div className="mt-4 flex items-center justify-end gap-1 border-t border-hairline pt-3">
                      <button type="button" disabled={pending} onClick={(event) => togglePin(event, meeting)} aria-label={meeting.pinned ? `Unpin ${meeting.title}` : `Pin ${meeting.title}`} className="rounded-lg p-2 text-ink-subtle hover:bg-surface-2 hover:text-ink disabled:opacity-50">
                        {meeting.pinned ? <PinOff className="h-4 w-4" /> : <Pin className="h-4 w-4" />}
                      </button>
                      <button type="button" disabled={pending} onClick={(event) => toggleArchive(event, meeting)} aria-label={meeting.archived ? `Unarchive ${meeting.title}` : `Archive ${meeting.title}`} className="rounded-lg p-2 text-ink-subtle hover:bg-surface-2 hover:text-ink disabled:opacity-50">
                        {meeting.archived ? <ArchiveRestore className="h-4 w-4" /> : <Archive className="h-4 w-4" />}
                      </button>
                    </div>
                    {(mutation?.pin?.status === 'error' || mutation?.archive?.status === 'error') && (
                      <StatusFeedback tone="error" className="mt-2 text-xs">{mutation.pin?.error ?? mutation.archive?.error}</StatusFeedback>
                    )}
                  </article>
                </li>
              )
            })}
          </ul>
        )}
      </div>
    </div>
  )
}
