'use client';

import { useEffect, useState } from 'react';
import { useLiveSource } from '@/lib/liveSource';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { badge, panel } from '@/lib/theme-classes';
import {
  LIVE_TIMELINE_UPDATE_EVENT,
  acceptSnapshot,
  formatTimelineTime,
  kindMarker,
  currentLiveTimeline,
  statusNotice,
  type TimelineEntry,
  type TimelineSnapshot,
} from '@/lib/liveTimeline';

/** The recording's live summary: hydrated from the Rust worker (which keeps
 * running across navigation) and kept current by its update events. */
export function useLiveTimeline(): TimelineSnapshot | null {
  const source = useLiveSource();
  const [snapshot, setSnapshot] = useState<TimelineSnapshot | null>(null);
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void currentLiveTimeline(source)
      .then((current) => {
        if (!disposed && current) setSnapshot((previous) => acceptSnapshot(previous, current));
      })
      .catch((error) => console.warn('Could not load the live summary:', error));
    void source.listen<TimelineSnapshot>(LIVE_TIMELINE_UPDATE_EVENT, ({ payload }) => {
      setSnapshot((previous) => acceptSnapshot(previous, payload));
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch((error) => console.warn('Could not listen for live summary updates:', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [source]);
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
        {entry.text}
        {entry.speaker && <span className="ml-1.5 text-[11px] text-ink-subtle">· {entry.speaker}</span>}
      </span>
    </>
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
            <ol className="space-y-1.5">
              {entries.map((entry) => (
                <li key={entry.id} className="flex gap-2">
                  <TimelineEntryRow entry={entry} />
                </li>
              ))}
            </ol>
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
