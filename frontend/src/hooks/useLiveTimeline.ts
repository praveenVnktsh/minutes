'use client';

import { useEffect, useState } from 'react';
import { useLiveSource } from '@/lib/liveSource';
import {
  LIVE_TIMELINE_UPDATE_EVENT,
  acceptSnapshot,
  currentLiveTimeline,
  type TimelineSnapshot,
} from '@/lib/liveTimeline';

/** The recording's live notes: hydrated from the Rust worker (which keeps
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
      .catch((error) => console.warn('Could not load the live notes:', error));
    void source.listen<TimelineSnapshot>(LIVE_TIMELINE_UPDATE_EVENT, ({ payload }) => {
      setSnapshot((previous) => acceptSnapshot(previous, payload));
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch((error) => console.warn('Could not listen for live notes updates:', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [source]);
  return snapshot;
}
