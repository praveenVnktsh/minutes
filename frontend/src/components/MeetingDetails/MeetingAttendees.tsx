'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { Plus, UserPlus, X } from 'lucide-react';
import { toast } from 'sonner';
import { peopleApi, type Person } from '@/lib/people';
import { badge, selection } from '@/lib/theme-classes';

const MAX_SUGGESTIONS = 8;

/** Role, team and projects, for a chip's hover text. */
export function personDetails(person: Person): string {
  const lines = [person.name];
  const work = [person.role, person.team].filter(Boolean).join(', ');
  if (work) lines.push(work);
  if (person.projects.length > 0) {
    lines.push(`Projects: ${person.projects.slice(0, 5).map((p) => p.title).join(', ')}`);
  }
  if (person.email) lines.push(person.email);
  return lines.join('\n');
}

function matches(person: Person, query: string): boolean {
  if (!query) return true;
  const needle = query.toLowerCase();
  return [person.name, person.email ?? '', ...person.aliases].some((value) =>
    value.toLowerCase().includes(needle),
  );
}

type Option = { kind: 'person'; person: Person } | { kind: 'create'; name: string };

/**
 * Who attended a meeting, as chips under its title, with a picker to add
 * someone from the people directory (or create them) and × to remove.
 * Calendar invites fill this in when a recording starts during an event.
 */
export function MeetingAttendees({ meetingId }: { meetingId: string }) {
  const [attendees, setAttendees] = useState<Person[]>([]);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [people, setPeople] = useState<Person[]>([]);
  const [busy, setBusy] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  const load = useCallback(() => {
    peopleApi
      .attendees(meetingId)
      .then(setAttendees)
      .catch((error) => console.warn('Could not load the meeting attendees:', error));
  }, [meetingId]);

  useEffect(() => {
    setAttendees([]);
    load();
  }, [load]);

  // Calendar linking finishes in the background right after a recording
  // starts; pick its attendees up when it does.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    listen<{ meeting_id: string }>('meeting-attendees-updated', ({ payload }) => {
      if (payload?.meeting_id === meetingId) load();
    })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch(() => { /* not running in Tauri */ });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [load, meetingId]);

  const close = useCallback(() => {
    setOpen(false);
    setQuery('');
  }, []);

  useEffect(() => {
    if (!open || typeof document === 'undefined' || !document.addEventListener) return;
    const onPointerDown = (event: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) close();
    };
    document.addEventListener('mousedown', onPointerDown);
    return () => document.removeEventListener('mousedown', onPointerDown);
  }, [open, close]);

  const openPicker = () => {
    setOpen(true);
    peopleApi
      .list()
      .then(setPeople)
      .catch((error) => toast.error('Could not load people', { description: String(error) }));
  };

  const options = useMemo<Option[]>(() => {
    const trimmed = query.trim();
    const attending = new Set(attendees.map((person) => person.id));
    const found: Option[] = people
      .filter((person) => !attending.has(person.id) && matches(person, trimmed))
      .slice(0, MAX_SUGGESTIONS)
      .map((person) => ({ kind: 'person', person }));
    const exact = people.some((person) => person.name.toLowerCase() === trimmed.toLowerCase());
    if (trimmed && !exact) found.push({ kind: 'create', name: trimmed });
    return found;
  }, [attendees, people, query]);

  const choose = async (option: Option) => {
    setBusy(true);
    try {
      const personId =
        option.kind === 'person'
          ? option.person.id
          : (await peopleApi.create({ name: option.name })).id;
      setAttendees(await peopleApi.addAttendee(meetingId, personId));
      close();
    } catch (error) {
      toast.error('Could not add the attendee', { description: String(error) });
    } finally {
      setBusy(false);
    }
  };

  const remove = async (person: Person) => {
    try {
      setAttendees(await peopleApi.removeAttendee(meetingId, person.id));
    } catch (error) {
      toast.error(`Could not remove ${person.name}`, { description: String(error) });
    }
  };

  return (
    <div ref={rootRef} className="relative flex flex-wrap items-center gap-1" aria-label="Attendees">
      {attendees.map((person) => (
        <span
          key={person.id}
          title={personDetails(person)}
          className={`group flex items-center gap-1 rounded-full py-0.5 pl-2 pr-1 text-[11px] font-medium ${badge.neutral}`}
        >
          {person.name}
          <button
            type="button"
            onClick={() => void remove(person)}
            aria-label={`Remove ${person.name}`}
            className="flex h-3.5 w-3.5 items-center justify-center rounded-full opacity-0 hover:text-ink focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-focus group-hover:opacity-100"
          >
            <X className="h-3 w-3" />
          </button>
        </span>
      ))}
      <button
        type="button"
        onClick={() => (open ? close() : openPicker())}
        aria-label={attendees.length > 0 ? 'Add attendee' : 'Add attendees'}
        aria-expanded={open}
        className="flex h-5 items-center gap-1 rounded-full px-1.5 text-[11px] text-ink-subtle hover:bg-surface-2 hover:text-ink focus-visible:ring-2 focus-visible:ring-focus"
      >
        {attendees.length > 0 ? (
          <Plus className="h-3 w-3" />
        ) : (
          <>
            <UserPlus className="h-3 w-3" /> Add attendees
          </>
        )}
      </button>
      {open && (
        <div className="absolute left-0 top-full z-50 mt-1 w-64 rounded-md border border-hairline bg-surface-raised p-1 text-ink shadow-md">
          <input
            autoFocus
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Escape') {
                event.preventDefault();
                close();
              } else if (event.key === 'Enter' && options[0] && !busy) {
                event.preventDefault();
                void choose(options[0]);
              }
            }}
            placeholder="Search or add a person"
            aria-label="Search people"
            className="w-full rounded bg-transparent px-2 py-1.5 text-xs outline-none placeholder:text-ink-subtle"
          />
          <ul role="listbox" aria-label="People" className="max-h-64 overflow-y-auto border-t border-hairline pt-1">
            {options.length === 0 && (
              <li className="px-2 py-1.5 text-xs text-ink-subtle">Type a name to add someone.</li>
            )}
            {options.map((option) => (
              <li key={option.kind === 'person' ? option.person.id : 'create'}>
                <button
                  type="button"
                  role="option"
                  aria-selected={false}
                  disabled={busy}
                  onClick={() => void choose(option)}
                  className={`flex w-full flex-col items-start rounded px-2 py-1.5 text-left text-xs disabled:opacity-50 ${selection.row.idle}`}
                >
                  {option.kind === 'person' ? (
                    <>
                      <span className="font-medium">{option.person.name}</span>
                      {(option.person.role || option.person.team || option.person.email) && (
                        <span className="text-[11px] text-ink-subtle">
                          {[option.person.role, option.person.team].filter(Boolean).join(', ') || option.person.email}
                        </span>
                      )}
                    </>
                  ) : (
                    <span>{`Create '${option.name}'`}</span>
                  )}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
