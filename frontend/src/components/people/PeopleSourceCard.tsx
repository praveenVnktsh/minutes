'use client';

import { useState } from 'react';
import { Link2, Plug, RefreshCw, Search, Unplug } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import {
  peopleSyncApi,
  type NotionDatabase,
  type NotionField,
  type NotionOverrides,
  type PeopleConnectionTest,
  type PeopleSourceStatus,
  type PeopleSyncSource,
} from '@/lib/people';
import { cn } from '@/lib/utils';
import { focusRing, panel } from '@/lib/theme-classes';

const COPY: Record<PeopleSyncSource, { title: string; blurb: string; keyLabel: string; placeholder: string; help: string }> = {
  linear: {
    title: 'Linear',
    blurb: 'Teammates, their teams, active projects and in-progress issues.',
    keyLabel: 'Linear API key',
    placeholder: 'lin_api_…',
    help: 'Linear → Settings → Security & access → Personal API keys. Read-only access is enough.',
  },
  notion: {
    title: 'Notion',
    blurb: 'People from a team-directory database: role, team and projects.',
    keyLabel: 'Notion integration token',
    placeholder: 'ntn_… or secret_…',
    help: 'Create an internal integration at notion.so/profile/integrations, copy its token, then share your team directory with it (••• → Connections).',
  },
};

const FIELD_LABELS: Record<NotionField, string> = {
  name: 'Name',
  email: 'Email',
  role: 'Role',
  team: 'Team',
  projects: 'Projects',
};

const AUTO = '__auto__';

export function formatSyncedAt(value: string | null): string {
  if (!value) return 'Not synced yet';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return `Last synced ${date.toLocaleString(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })}`;
}

function describeTest(test: PeopleConnectionTest, source: PeopleSyncSource): string {
  const where = test.workspace ? `Connected to ${test.workspace}` : 'Connected';
  if (test.user_count === null) return where;
  const noun = source === 'linear' ? 'active user' : 'row';
  return `${where} · ${test.user_count} ${noun}${test.user_count === 1 ? '' : 's'}`;
}

const selectClass = cn('h-9 w-full rounded-md border border-input bg-surface-raised px-2 text-sm text-ink', focusRing);

interface PeopleSourceCardProps {
  status: PeopleSourceStatus;
  onStatuses: (statuses: PeopleSourceStatus[]) => void;
  /** After a sync or disconnect changes the people list. */
  onPeopleChanged: () => void;
}

export function PeopleSourceCard({ status, onStatuses, onPeopleChanged }: PeopleSourceCardProps) {
  const source = status.source;
  const copy = COPY[source];
  const [token, setToken] = useState('');
  const [busy, setBusy] = useState<null | 'connect' | 'test' | 'sync' | 'disconnect' | 'databases' | 'mapping'>(null);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [databases, setDatabases] = useState<NotionDatabase[] | null>(null);
  const [databaseId, setDatabaseId] = useState<string>(status.notion?.database_id ?? '');
  const [overrides, setOverrides] = useState<NotionOverrides>(status.notion?.mapping ?? {});

  const refreshStatus = async () => onStatuses(await peopleSyncApi.status());

  const run = async (kind: NonNullable<typeof busy>, action: () => Promise<void>, failure: string) => {
    setBusy(kind);
    try {
      await action();
    } catch (error) {
      toast.error(failure, { description: String(error) });
    } finally {
      setBusy(null);
    }
  };

  const connect = () => run('connect', async () => {
    const result = await peopleSyncApi.connect(source, token);
    setToken('');
    setTestResult(describeTest(result.test, source));
    onStatuses(result.statuses);
    toast.success(`${copy.title} connected`, { description: describeTest(result.test, source) });
    // Notion needs a database before it can sync: offer the list straight away.
    if (source === 'notion') {
      try {
        const list = await peopleSyncApi.notionDatabases();
        setDatabases(list);
        if (!databaseId && list.length === 1) setDatabaseId(list[0].id);
      } catch {
        /* the Choose database button retries */
      }
    }
  }, `Could not connect ${copy.title}`);

  const test = () => run('test', async () => {
    const result = await peopleSyncApi.test(source);
    setTestResult(describeTest(result, source));
  }, `${copy.title} connection failed`);

  const sync = () => run('sync', async () => {
    const [result] = await peopleSyncApi.syncNow(source);
    await refreshStatus();
    onPeopleChanged();
    if (result?.error) {
      toast.error(`${copy.title} sync failed`, { description: result.error });
    } else if (result) {
      const message = `Synced ${result.synced} ${result.synced === 1 ? 'person' : 'people'} from ${copy.title}`;
      if (result.warnings.length > 0) {
        toast.warning(message, { description: result.warnings.join(' ') });
      } else {
        toast.success(message);
      }
    }
  }, `${copy.title} sync failed`);

  const disconnect = () => run('disconnect', async () => {
    await peopleSyncApi.disconnect(source);
    setTestResult(null);
    setDatabases(null);
    await refreshStatus();
    onPeopleChanged();
    toast.success(`${copy.title} disconnected`);
  }, `Could not disconnect ${copy.title}`);

  const loadDatabases = () => run('databases', async () => {
    const list = await peopleSyncApi.notionDatabases();
    setDatabases(list);
    if (!databaseId && list.length === 1) setDatabaseId(list[0].id);
  }, 'Could not list Notion databases');

  const saveMapping = () => run('mapping', async () => {
    const database = databases?.find(db => db.id === databaseId);
    onStatuses(await peopleSyncApi.saveNotionMapping(databaseId, database?.title ?? status.notion?.database_title ?? null, overrides));
    toast.success('Notion mapping saved');
  }, 'Could not save the Notion mapping');

  const chosen = databases?.find(db => db.id === databaseId) ?? null;

  return (
    <section aria-label={`${copy.title} people sync`} className="space-y-4 rounded-lg border border-hairline bg-surface-raised p-4">
      <div>
        <h3 className="font-semibold">{copy.title}</h3>
        <p className="text-sm text-ink-muted">{copy.blurb}</p>
      </div>

      {!status.connected ? (
        <form className="space-y-2" onSubmit={event => { event.preventDefault(); void connect(); }}>
          <label htmlFor={`${source}-token`} className="text-sm font-medium">{copy.keyLabel}</label>
          <Input
            id={`${source}-token`}
            aria-label={copy.keyLabel}
            type="password"
            autoComplete="off"
            value={token}
            placeholder={copy.placeholder}
            onChange={event => setToken(event.target.value)}
          />
          <p className="text-xs text-ink-muted">{copy.help}</p>
          <Button type="submit" size="sm" disabled={!token.trim() || busy !== null}>
            <Plug className="h-4 w-4" />
            {busy === 'connect' ? 'Connecting…' : 'Connect'}
          </Button>
        </form>
      ) : (
        <div className="space-y-3">
          <div className="text-sm">
            <div className="font-medium">{status.workspace ?? 'Connected'}</div>
            <div className="text-ink-muted">
              {formatSyncedAt(status.last_synced_at)}
              {status.last_synced_at && status.last_count !== null && ` · ${status.last_count} ${status.last_count === 1 ? 'person' : 'people'}`}
            </div>
            {testResult && <div className="text-ink-muted">{testResult}</div>}
          </div>
          {status.last_error && (
            <div role="alert" className={cn('rounded-md p-3 text-sm', panel.error)}>{status.last_error}</div>
          )}
          {!status.last_error && status.last_warning && (
            <div role="status" className={cn('rounded-md p-3 text-sm', panel.warning)}>{status.last_warning}</div>
          )}
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="outline" onClick={test} disabled={busy !== null}>
              <Search className="h-4 w-4" />
              {busy === 'test' ? 'Testing…' : 'Test'}
            </Button>
            <Button size="sm" variant="outline" onClick={sync} disabled={busy !== null || (source === 'notion' && !status.notion?.database_id)}>
              <RefreshCw className="h-4 w-4" />
              {busy === 'sync' ? 'Syncing…' : 'Sync now'}
            </Button>
            <Button size="sm" variant="outline" onClick={disconnect} disabled={busy !== null}>
              <Unplug className="h-4 w-4" />
              {busy === 'disconnect' ? 'Disconnecting…' : 'Disconnect'}
            </Button>
          </div>

          {source === 'notion' && (
            <div className="space-y-3 border-t border-hairline pt-3">
              <div className="flex items-center justify-between gap-2 text-sm">
                <span>
                  Database: <span className="font-medium">{status.notion?.database_title ?? (status.notion?.database_id ? 'Chosen' : 'None chosen')}</span>
                </span>
                <Button size="sm" variant="ghost" onClick={loadDatabases} disabled={busy !== null}>
                  <Link2 className="h-4 w-4" />
                  {busy === 'databases' ? 'Loading…' : status.notion?.database_id ? 'Change' : 'Choose database'}
                </Button>
              </div>
              {databases && (
                <div className="space-y-3">
                  {databases.length === 0 ? (
                    <p className="text-sm text-ink-muted">The integration can&apos;t see any databases yet. Share your team directory with it in Notion.</p>
                  ) : (
                    <select
                      aria-label="Notion database"
                      className={selectClass}
                      value={databaseId}
                      onChange={event => { setDatabaseId(event.target.value); setOverrides({}); }}
                    >
                      <option value="">Choose a database…</option>
                      {databases.map(db => <option key={db.id} value={db.id}>{db.title}</option>)}
                    </select>
                  )}
                  {chosen && (
                    <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                      {(Object.keys(FIELD_LABELS) as NotionField[]).map(field => {
                        const value = overrides[field];
                        return (
                          <label key={field} className="space-y-1 text-sm">
                            <span className="font-medium">{FIELD_LABELS[field]}</span>
                            <select
                              aria-label={`${FIELD_LABELS[field]} property`}
                              className={selectClass}
                              value={value === undefined || value === null ? AUTO : value}
                              onChange={event => {
                                const next = event.target.value;
                                setOverrides(current => ({ ...current, [field]: next === AUTO ? null : next }));
                              }}
                            >
                              <option value={AUTO}>Auto ({chosen.detected[field] ?? 'none found'})</option>
                              <option value="">Don&apos;t use</option>
                              {chosen.properties.map(property => (
                                <option key={property.name} value={property.name}>{property.name} ({property.type})</option>
                              ))}
                            </select>
                          </label>
                        );
                      })}
                    </div>
                  )}
                  <Button size="sm" onClick={saveMapping} disabled={!databaseId || busy !== null}>
                    {busy === 'mapping' ? 'Saving…' : 'Save database and mapping'}
                  </Button>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </section>
  );
}
