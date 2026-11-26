'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { Pencil, Plus, Trash2 } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import {
  PEOPLE_SYNC_EVENT,
  peopleApi,
  peopleSyncApi,
  type PeopleSourceStatus,
  type Person,
  type PersonEdit,
} from '@/lib/people';
import { badge } from '@/lib/theme-classes';
import { cn } from '@/lib/utils';
import { PeopleSourceCard } from './PeopleSourceCard';
import { PersonDialog } from './PersonDialog';

const SOURCE_LABELS: Record<string, string> = {
  linear: 'Linear',
  notion: 'Notion',
  calendar: 'Calendar',
  manual: 'Added by you',
};

const DISCONNECTED: PeopleSourceStatus[] = (['linear', 'notion'] as const).map(source => ({
  source,
  connected: false,
  workspace: null,
  last_synced_at: null,
  last_attempt_at: null,
  last_error: null,
  last_count: null,
  notion: null,
}));

/** Whether a person matches a search across name, email, role, team, aliases and projects. */
export function matchesSearch(person: Person, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  return [
    person.name,
    person.email ?? '',
    person.role ?? '',
    person.team ?? '',
    ...person.aliases,
    ...person.projects.map(project => project.title),
  ].some(value => value.toLowerCase().includes(needle));
}

function sourcesOf(person: Person): string[] {
  return Array.from(new Set(person.sources.map(ref => ref.source)));
}

export function PeopleSettings() {
  const [statuses, setStatuses] = useState<PeopleSourceStatus[]>(DISCONNECTED);
  const [people, setPeople] = useState<Person[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState('');
  const [editing, setEditing] = useState<Person | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  const loadPeople = useCallback(async () => {
    try {
      setPeople(await peopleApi.list());
    } catch (error) {
      toast.error('Could not load people', { description: String(error) });
    }
  }, []);

  useEffect(() => {
    Promise.all([
      peopleSyncApi.status().then(setStatuses).catch(error => toast.error('Could not load people sync settings', { description: String(error) })),
      loadPeople(),
    ]).finally(() => setLoading(false));
  }, [loadPeople]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<PeopleSourceStatus[]>(PEOPLE_SYNC_EVENT, event => {
      setStatuses(event.payload);
      void loadPeople();
    }).then(dispose => { unlisten = dispose; }).catch(() => { /* not running in Tauri */ });
    return () => { unlisten?.(); };
  }, [loadPeople]);

  const visible = useMemo(() => people.filter(person => matchesSearch(person, query)), [people, query]);

  const openEditor = (person: Person | null) => {
    setEditing(person);
    setDialogOpen(true);
  };

  const save = async (edit: PersonEdit): Promise<boolean> => {
    try {
      if (editing) {
        if (Object.keys(edit).length > 0) await peopleApi.update(editing.id, edit);
        toast.success('Saved');
      } else {
        await peopleApi.create(edit);
        toast.success('Person added');
      }
      await loadPeople();
      return true;
    } catch (error) {
      toast.error('Could not save', { description: String(error) });
      return false;
    }
  };

  const remove = async (person: Person) => {
    try {
      await peopleApi.remove(person.id);
      setConfirmDelete(null);
      setPeople(current => current.filter(p => p.id !== person.id));
      toast.success(`Deleted ${person.name}`);
    } catch (error) {
      toast.error('Could not delete', { description: String(error) });
    }
  };

  if (loading) {
    return <div className="h-40 animate-pulse rounded-lg bg-surface-2" />;
  }

  return (
    <div className="max-w-4xl space-y-8">
      <div>
        <h2 className="text-xl font-semibold">People</h2>
        <p className="mt-1 text-sm text-ink-muted">
          Who you meet with, so transcripts spell names right and notes know who owns what. Linear and Notion
          are read with your own key and token, once a day or when you press Sync now. The directory stays on
          this Mac, and no meeting content is ever sent to Linear or Notion.
        </p>
      </div>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        {statuses.map(status => (
          <PeopleSourceCard key={status.source} status={status} onStatuses={setStatuses} onPeopleChanged={() => void loadPeople()} />
        ))}
      </div>

      <section aria-label="People directory" className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <h3 className="mr-auto font-semibold">Directory <span className="font-normal text-ink-muted">({people.length})</span></h3>
          <Input
            aria-label="Search people"
            placeholder="Search people"
            value={query}
            onChange={event => setQuery(event.target.value)}
            className="w-56"
          />
          <Button size="sm" onClick={() => openEditor(null)}>
            <Plus className="h-4 w-4" />
            Add person
          </Button>
        </div>

        {visible.length === 0 ? (
          <p className="rounded-lg border border-hairline bg-surface-raised p-4 text-sm text-ink-muted">
            {people.length === 0 ? 'No people yet. Connect Linear or Notion, or add someone by hand.' : 'Nobody matches that search.'}
          </p>
        ) : (
          <ul className="divide-y divide-hairline rounded-lg border border-hairline bg-surface-raised">
            {visible.map(person => (
              <li key={person.id} className="flex items-start gap-3 p-3">
                <div className="min-w-0 flex-1 space-y-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-medium">{person.name}</span>
                    {person.is_me && <span className={cn('rounded-full px-2 py-0.5 text-xs', badge.info)}>You</span>}
                    {sourcesOf(person).map(source => (
                      <span key={source} className={cn('rounded-full px-2 py-0.5 text-xs', badge.neutral)}>{SOURCE_LABELS[source] ?? source}</span>
                    ))}
                  </div>
                  {(person.role || person.team) && (
                    <div className="text-sm text-ink-muted">{[person.role, person.team].filter(Boolean).join(' · ')}</div>
                  )}
                  {person.projects.length > 0 && (
                    <div className="truncate text-xs text-ink-muted">
                      {person.projects.slice(0, 4).map(project => project.title).join(', ')}
                      {person.projects.length > 4 && ` +${person.projects.length - 4} more`}
                    </div>
                  )}
                </div>
                {confirmDelete === person.id ? (
                  <div className="flex shrink-0 items-center gap-2">
                    <span className="text-sm">Delete?</span>
                    <Button size="sm" variant="error" aria-label={`Confirm delete ${person.name}`} onClick={() => void remove(person)}>Delete</Button>
                    <Button size="sm" variant="outline" onClick={() => setConfirmDelete(null)}>Cancel</Button>
                  </div>
                ) : (
                  <div className="flex shrink-0 gap-1">
                    <Button size="icon" variant="ghost" aria-label={`Edit ${person.name}`} onClick={() => openEditor(person)}>
                      <Pencil className="h-4 w-4" />
                    </Button>
                    <Button size="icon" variant="ghost" aria-label={`Delete ${person.name}`} onClick={() => setConfirmDelete(person.id)}>
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>

      <PersonDialog open={dialogOpen} person={editing} onOpenChange={setDialogOpen} onSave={save} />
    </div>
  );
}
