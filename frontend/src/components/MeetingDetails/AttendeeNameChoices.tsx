'use client';

import { createContext, useContext, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { peopleApi, type Person } from '@/lib/people';

/** The attendees' names, trimmed and deduplicated (case-insensitively), in order. */
export function attendeeNames(attendees: Pick<Person, 'name'>[]): string[] {
  const seen = new Set<string>();
  const names: string[] = [];
  for (const { name } of attendees) {
    const trimmed = name.trim();
    const key = trimmed.toLowerCase();
    if (!trimmed || seen.has(key)) continue;
    seen.add(key);
    names.push(trimmed);
  }
  return names;
}

/** Emitted by the Rust core whenever a meeting's attendees change. */
export const MEETING_ATTENDEES_UPDATED_EVENT = 'meeting-attendees-updated';

/**
 * The meeting's attendees' names, reloaded whenever `refreshKey` changes or
 * the meeting's attendees change. Empty when there are none or they cannot
 * be read.
 */
export function useMeetingAttendeeNames(meetingId: string | undefined, refreshKey?: unknown): string[] {
  const [names, setNames] = useState<string[]>([]);
  const [changes, setChanges] = useState(0);

  useEffect(() => {
    if (!meetingId) {
      setNames([]);
      return;
    }
    let cancelled = false;
    peopleApi
      .attendees(meetingId)
      .then((people) => {
        if (!cancelled) setNames(attendeeNames(people));
      })
      .catch((error) => {
        console.warn('Could not load meeting attendees:', error);
        if (!cancelled) setNames([]);
      });
    return () => {
      cancelled = true;
    };
  }, [meetingId, refreshKey, changes]);

  useEffect(() => {
    if (!meetingId) return;
    let unlisten: (() => void) | undefined;
    let disposed = false;
    listen<{ meeting_id: string }>(MEETING_ATTENDEES_UPDATED_EVENT, ({ payload }) => {
      if (payload?.meeting_id === meetingId) setChanges((count) => count + 1);
    })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch(() => {
        /* not running in Tauri */
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [meetingId]);

  return names;
}

/** The meeting's attendees' names, for the speaker picker deep in the transcript view. */
export const AttendeeNamesContext = createContext<string[]>([]);

export function useAttendeeNames(): string[] {
  return useContext(AttendeeNamesContext);
}

/**
 * The meeting's attendees as one-click names for a speaker. Renders nothing
 * when the meeting has no attendees.
 */
export function AttendeeNameChoices({
  names,
  current,
  onPick,
  disabled = false,
}: {
  names: string[];
  /** The speaker's current name; its chip is disabled. */
  current?: string;
  onPick: (name: string) => void;
  disabled?: boolean;
}) {
  if (names.length === 0) return null;
  return (
    <div>
      <div className="mb-1 text-[11px] font-medium text-ink-muted">Attendees</div>
      <div className="flex flex-wrap gap-1">
        {names.map((name) => (
          <button
            key={name}
            type="button"
            onClick={() => onPick(name)}
            disabled={disabled || name === current?.trim()}
            className="rounded-full bg-surface-2 px-2 py-0.5 text-[11px] text-ink-muted hover:text-ink disabled:opacity-50"
          >
            {name}
          </button>
        ))}
      </div>
    </div>
  );
}
