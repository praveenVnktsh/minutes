'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { CalendarDays, RefreshCw, Search } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { SaveFeedback, type SaveFeedbackState } from '@/components/ui/status-feedback';

interface CalendarConfig {
  enabled: boolean;
  url: string;
  refresh_minutes: number;
  lookahead_days: number;
  remind: boolean;
}

interface CalendarEvent {
  uid: string;
  title: string;
  start: string;
  end: string | null;
  location: string | null;
  all_day: boolean;
}

const DEFAULT_CONFIG: CalendarConfig = {
  enabled: false,
  url: '',
  refresh_minutes: 15,
  lookahead_days: 7,
  remind: true,
};

function formatEventTime(event: CalendarEvent): string {
  const start = new Date(event.start);
  if (Number.isNaN(start.getTime())) return event.start;
  if (event.all_day) {
    return start.toLocaleDateString(undefined, {
      weekday: 'short',
      month: 'short',
      day: 'numeric',
    });
  }
  return start.toLocaleString(undefined, {
    weekday: 'short',
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  });
}

export function CalendarSettings() {
  const [config, setConfig] = useState<CalendarConfig>(DEFAULT_CONFIG);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [preview, setPreview] = useState<CalendarEvent[] | null>(null);
  const [savedConfig, setSavedConfig] = useState<CalendarConfig>(DEFAULT_CONFIG);
  const [saveState, setSaveState] = useState<SaveFeedbackState>('saved');

  useEffect(() => {
    invoke<CalendarConfig>('get_calendar_config')
      .then(value => { setConfig(value); setSavedConfig(value); })
      .catch((error) => toast.error('Could not load calendar settings', {
        description: String(error),
      }))
      .finally(() => setLoading(false));

    invoke<{ events: CalendarEvent[] } | null>('get_calendar_events')
      .then((snapshot) => setPreview(snapshot?.events ?? null))
      .catch(() => { /* no cached snapshot yet */ });
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<{ events: CalendarEvent[] }>('calendar-events-updated', (event) => {
      setPreview(event.payload.events);
    }).then((dispose) => { unlisten = dispose; }).catch(() => { /* not running in Tauri */ });
    return () => { unlisten?.(); };
  }, []);

  const save = useCallback(async (showToast = true) => {
    setSaving(true);
    setSaveState('saving');
    try {
      await invoke('set_calendar_config', { config });
      setSavedConfig(config);
      setSaveState('saved');
      if (showToast) toast.success('Calendar settings saved');
      return true;
    } catch (error) {
      setSaveState('error');
      toast.error('Could not save calendar settings', { description: String(error) });
      return false;
    } finally {
      setSaving(false);
    }
  }, [config]);

  const testFeed = async () => {
    setTesting(true);
    try {
      const events = await invoke<CalendarEvent[]>('test_calendar_feed', { url: config.url });
      setPreview(events);
      toast.success(events.length > 0
        ? `Found ${events.length} upcoming event${events.length === 1 ? '' : 's'}`
        : 'Connected, but no upcoming events were found');
    } catch (error) {
      toast.error('Could not read the calendar feed', { description: String(error) });
    } finally {
      setTesting(false);
    }
  };

  const refreshNow = async () => {
    setRefreshing(true);
    try {
      if (!await save(false)) return;
      const snapshot = await invoke<{ events: CalendarEvent[] }>('refresh_calendar_now');
      setPreview(snapshot.events);
      toast.success('Calendar synced');
    } catch (error) {
      toast.error('Calendar sync failed', { description: String(error) });
    } finally {
      setRefreshing(false);
    }
  };

  if (loading) {
    return <div className="mt-6 h-40 animate-pulse rounded-lg bg-surface-2" />;
  }

  return (
    <div className="mt-6 max-w-3xl space-y-6">
      <div>
        <h2 className="text-xl font-semibold">Calendar subscription</h2>
        <p className="mt-1 text-sm text-ink-muted">
          Paste your calendar&apos;s secret iCal address to see upcoming meetings and get reminders.
          This is read-only and works with Google, Outlook, Fastmail, and Apple Calendar — no
          account sign-in required.
        </p>
      </div>

      <div className="flex items-center justify-between rounded-lg border bg-surface-raised p-4">
        <div className="pr-4">
          <div className="font-medium">Enable calendar sync</div>
          <div className="text-sm text-ink-muted">
            The feed is polled in the background and never uploaded anywhere.
          </div>
        </div>
        <Switch
          aria-label="Enable calendar sync"
          checked={config.enabled}
          onCheckedChange={enabled => setConfig(current => ({ ...current, enabled }))}
        />
      </div>

      <div className="space-y-2">
        <label htmlFor="calendar-url" className="text-sm font-medium">Secret iCal address</label>
        <input
          id="calendar-url"
          type="password"
          value={config.url}
          onChange={event => setConfig(current => ({ ...current, url: event.target.value }))}
          placeholder="https://calendar.google.com/calendar/ical/.../basic.ics"
          className="w-full rounded-md border border-hairline bg-surface-raised px-3 py-2 text-sm outline-none focus:border-focus focus:ring-2 focus:ring-focus/30"
        />
        <p className="text-xs text-ink-muted">
          Google Calendar: Settings → your calendar → <em>Integrate calendar</em> → “Secret address
          in iCal format”. Treat this link like a password.
        </p>
      </div>

      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <div className="space-y-2">
          <label htmlFor="calendar-refresh" className="text-sm font-medium">Refresh interval (minutes)</label>
          <input
            id="calendar-refresh"
            type="number"
            min={1}
            max={1440}
            value={config.refresh_minutes}
            onChange={event => setConfig(current => ({
              ...current,
              refresh_minutes: Number(event.target.value) || 0,
            }))}
            className="w-full rounded-md border border-hairline bg-surface-raised px-3 py-2 text-sm outline-none focus:border-focus focus:ring-2 focus:ring-focus/30"
          />
        </div>
        <div className="space-y-2">
          <label htmlFor="calendar-lookahead" className="text-sm font-medium">Look ahead (days)</label>
          <input
            id="calendar-lookahead"
            type="number"
            min={1}
            max={60}
            value={config.lookahead_days}
            onChange={event => setConfig(current => ({
              ...current,
              lookahead_days: Number(event.target.value) || 0,
            }))}
            className="w-full rounded-md border border-hairline bg-surface-raised px-3 py-2 text-sm outline-none focus:border-focus focus:ring-2 focus:ring-focus/30"
          />
        </div>
      </div>

      <div className="flex items-center justify-between rounded-lg border bg-surface-raised p-4">
        <div className="pr-4">
          <div className="font-medium">Meeting reminders</div>
          <div className="text-sm text-ink-muted">
            Uses your notification reminder times. All-day events are never interrupted.
          </div>
        </div>
        <Switch
          aria-label="Enable calendar meeting reminders"
          checked={config.remind}
          onCheckedChange={remind => setConfig(current => ({ ...current, remind }))}
        />
      </div>

      <div className="flex flex-wrap gap-3">
        <Button onClick={() => save()} disabled={saving || testing || refreshing}>
          {saving ? 'Saving…' : 'Save settings'}
        </Button>
        <Button variant="outline" onClick={testFeed} disabled={testing || saving || refreshing || !config.url}>
          <Search className="mr-2 h-4 w-4" />
          {testing ? 'Checking…' : 'Test connection'}
        </Button>
        <Button variant="outline" onClick={refreshNow} disabled={refreshing || saving || testing || !config.enabled}>
          <RefreshCw className="mr-2 h-4 w-4" />
          {refreshing ? 'Syncing…' : 'Sync now'}
        </Button>
      </div>
      <SaveFeedback
        state={saving ? 'saving' : saveState === 'error' ? 'error' : JSON.stringify(config) === JSON.stringify(savedConfig) ? 'saved' : 'unsaved'}
        labels={{ saving: 'Saving calendar settings', saved: 'Calendar settings saved', error: 'Could not save calendar settings', unsaved: 'Calendar changes not saved' }}
      />

      {preview && (
        <div className="rounded-lg border bg-surface-raised p-4">
          <div className="mb-3 flex items-center gap-2 font-medium">
            <CalendarDays className="h-4 w-4" />
            Upcoming events
          </div>
          {preview.length === 0 ? (
            <p className="text-sm text-ink-muted">No upcoming events in the look-ahead window.</p>
          ) : (
            <ul className="space-y-2">
              {preview.map(event => (
                <li
                  key={`${event.uid}-${event.start}`}
                  className="flex items-baseline justify-between gap-4 text-sm"
                >
                  <span className="min-w-0 truncate font-medium">{event.title}</span>
                  <span className="shrink-0 text-ink-muted">{formatEventTime(event)}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
