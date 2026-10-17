'use client';

import { useEffect, useState, type ReactNode } from 'react';
import { listen } from '@tauri-apps/api/event';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { badge, panel } from '@/lib/theme-classes';
import {
  LIVE_TIMELINE_UPDATE_EVENT,
  acceptSnapshot,
  entryDisplayText,
  formatTimelineTime,
  groupByTopic,
  kindMarker,
  liveTimelineApi,
  statusNotice,
  type TimelineEntry,
  type TimelineSnapshot,
} from '@/lib/liveTimeline';

/** The recording's live summary: hydrated from the Rust worker (which keeps
 * running across navigation) and kept current by its update events. */
export function useLiveTimeline(): TimelineSnapshot | null {
  const [snapshot, setSnapshot] = useState<TimelineSnapshot | null>(null);
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void liveTimelineApi.current()
      .then((current) => {
        if (!disposed && current) setSnapshot((previous) => acceptSnapshot(previous, current));
      })
      .catch((error) => console.warn('Could not load the live summary:', error));
    void listen<TimelineSnapshot>(LIVE_TIMELINE_UPDATE_EVENT, ({ payload }) => {
      setSnapshot((previous) => acceptSnapshot(previous, payload));
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch((error) => console.warn('Could not listen for live summary updates:', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
  return snapshot;
}

export function TimelineEntryRow({ entry }: { entry: TimelineEntry }) {
  const marker = kindMarker(entry.kind);
  return (
    <>
      <span className="w-11 shrink-0 pt-px font-mono text-[11px] tabular-nums text-ink-subtle">
        {formatTimelineTime(entry.start_ms)}
      </span>
      <span className="min-w-0 flex-1 text-sm leading-snug text-ink">
        {marker && (
          <span className={`mr-1.5 inline-block rounded px-1.5 py-px text-[10px] font-semibold uppercase tracking-[0.04em] ${badge[marker.tone]}`}>
            {marker.label}
          </span>
        )}
        {entryDisplayText(entry)}
      </span>
    </>
  );
}

/** A topic's section header: a thin rule and the title. */
export function TopicHeader({ topic }: { topic: string }) {
  return (
    <div className="flex items-center gap-2 pb-1 pt-1 text-[11px] font-semibold text-ink-muted">
      <span aria-hidden className="h-px w-3 shrink-0 bg-hairline" />
      <span className="min-w-0 truncate" title={topic}>{topic}</span>
      <span aria-hidden className="h-px min-w-4 flex-1 bg-hairline" />
    </div>
  );
}

/** Entries grouped under their topics. `renderEntry` draws one entry's row
 * (the chapters wrap it in a button). */
export function TimelineSections<T extends TimelineEntry>({
  entries,
  renderEntry,
  itemClassName,
  listClassName = 'space-y-1.5',
}: {
  entries: T[];
  renderEntry: (entry: T) => ReactNode;
  itemClassName?: string;
  listClassName?: string;
}) {
  return (
    <ol className="space-y-1">
      {groupByTopic(entries).map((section) => (
        <li key={`${section.topic ?? ''}-${section.entries[0].id}`} aria-label={section.topic ?? undefined}>
          {section.topic && <TopicHeader topic={section.topic} />}
          <ol className={listClassName}>
            {section.entries.map((entry) => (
              <li key={entry.id} className={itemClassName}>
                {renderEntry(entry)}
              </li>
            ))}
          </ol>
        </li>
      ))}
    </ol>
  );
}

/** The live summary section above the live transcript. */
export function LiveSummaryPanel({ active }: { active: boolean }) {
  const snapshot = useLiveTimeline();
  const [open, setOpen] = useState(true);
  const entries = snapshot?.entries ?? [];
  const notice = statusNotice(snapshot);
  const listening = active && snapshot?.status !== 'disabled' && snapshot?.status !== 'unavailable';

  return (
    <section aria-label="Live summary" className="mx-auto w-full max-w-[900px] px-8 pb-2">
      <button
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        className="flex items-center gap-1 text-[11px] font-bold uppercase tracking-[0.08em] text-ink-muted hover:text-ink"
      >
        {open ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        Live summary
        {entries.length > 0 && <span className="font-normal normal-case tracking-normal text-ink-subtle">({entries.length})</span>}
      </button>
      {open && (
        <div className="mt-2 max-h-[40vh] overflow-y-auto rounded-lg border border-hairline bg-surface-1 px-3 py-2">
          {notice && (
            <p role="status" className={`mb-2 rounded-md px-2.5 py-1.5 text-xs ${panel[notice.tone]}`}>
              {notice.text}
            </p>
          )}
          {entries.length > 0 && (
            <TimelineSections
              entries={entries}
              itemClassName="flex gap-2"
              renderEntry={(entry) => <TimelineEntryRow entry={entry} />}
            />
          )}
          {listening && (
            <p aria-live="polite" className="mt-1.5 flex items-center gap-2 text-xs italic text-ink-subtle">
              <span className={`h-1 w-1 rounded-full bg-ink-subtle ${snapshot?.status === 'generating' ? 'animate-pulse' : ''}`} />
              {snapshot?.status === 'generating'
                ? 'Summarizing the last minute…'
                : entries.length === 0
                  ? 'Listening… key points will appear here every minute or so.'
                  : 'Listening…'}
            </p>
          )}
        </div>
      )}
    </section>
  );
}
