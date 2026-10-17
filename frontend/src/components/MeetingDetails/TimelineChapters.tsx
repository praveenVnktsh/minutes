'use client';

import { useCallback, useEffect, useState } from 'react';
import { useLiveSource } from '@/lib/liveSource';
import { ChevronDown, ChevronRight } from 'lucide-react';
import {
  LIVE_TIMELINE_UPDATE_EVENT,
  jumpToTime,
  liveTimelineApi,
  type StoredTimelineEntry,
  type TimelineSnapshot,
} from '@/lib/liveTimeline';
import { TimelineEntryRow, TimelineSections } from './LiveSummaryPanel';

/** A meeting's stored live summary entries. Reloads when the recording's
 * worker stores its last entry after the page opened. */
export function useMeetingTimeline(meetingId: string | undefined): StoredTimelineEntry[] {
  const source = useLiveSource();
  const [entries, setEntries] = useState<StoredTimelineEntry[]>([]);
  const load = useCallback(async () => {
    if (!meetingId) return;
    try {
      setEntries(await liveTimelineApi.listForMeeting(meetingId));
    } catch (error) {
      console.warn('Could not load the meeting chapters:', error);
    }
  }, [meetingId]);

  useEffect(() => {
    setEntries([]);
    void load();
    if (!meetingId) return undefined;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void source.listen<TimelineSnapshot>(LIVE_TIMELINE_UPDATE_EVENT, ({ payload }) => {
      if (payload.meeting_id === meetingId) void load();
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [load, meetingId, source]);

  return entries;
}

/** The meeting's chapters above its transcript; clicking one scrolls the
 * transcript to that time. */
export function TimelineChapters({ meetingId }: { meetingId: string | undefined }) {
  const entries = useMeetingTimeline(meetingId);
  const [open, setOpen] = useState(true);
  if (entries.length === 0) return null;

  return (
    <section aria-label="Chapters" className="mt-2">
      <button
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        className="flex items-center gap-1 text-[11px] font-bold uppercase tracking-[0.08em] text-ink-muted hover:text-ink"
      >
        {open ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        Chapters
        <span className="font-normal normal-case tracking-normal text-ink-subtle">({entries.length})</span>
      </button>
      {open && (
        <div className="mt-1.5 max-h-48 overflow-y-auto">
          <TimelineSections
            entries={entries}
            listClassName="space-y-0.5"
            renderEntry={(entry) => (
              <button
                type="button"
                onClick={() => jumpToTime(entry.start_ms)}
                title="Jump to this point in the transcript"
                className="flex w-full gap-2 rounded px-1.5 py-1 text-left hover:bg-surface-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus"
              >
                <TimelineEntryRow entry={entry} />
              </button>
            )}
          />
        </div>
      )}
    </section>
  );
}
