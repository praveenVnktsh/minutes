'use client';

import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Plus, X } from 'lucide-react';
import { Switch } from '@/components/ui/switch';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { StatusFeedback } from '@/components/ui/status-feedback';
import {
  meetingDetectionService,
  normalizeAppName,
  type AudioApp,
  type MeetingDetectionSettings,
} from '@/services/meetingDetectionService';

function describeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function sameApp(a: AudioApp, b: AudioApp): boolean {
  if (a.bundle_id && b.bundle_id && a.bundle_id.toLowerCase() === b.bundle_id.toLowerCase()) return true;
  return normalizeAppName(a.name) === normalizeAppName(b.name);
}

/**
 * Settings for which apps may trigger the "meeting detected" prompt: the
 * apps the user never wants to be asked about, and whether unknown apps count.
 */
export function MeetingDetectionIgnoreList() {
  const [settings, setSettings] = useState<MeetingDetectionSettings | null>(null);
  const [recent, setRecent] = useState<AudioApp[]>([]);
  const [snoozedUntil, setSnoozedUntil] = useState<number | null>(null);
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async (isCancelled: () => boolean = () => false) => {
    try {
      const [loaded, recentApps, snooze] = await Promise.all([
        meetingDetectionService.getSettings(),
        meetingDetectionService.recentApps().catch(() => [] as AudioApp[]),
        meetingDetectionService.snoozedUntil().catch(() => null),
      ]);
      if (isCancelled()) return;
      setSettings(loaded);
      setRecent(recentApps ?? []);
      setSnoozedUntil(snooze);
    } catch (loadError) {
      if (!isCancelled()) setError(`Could not load ignored apps: ${describeError(loadError)}`);
    }
  }, []);

  // Load once, then follow changes made anywhere (the prompt's "Don't ask",
  // a snooze ending) so this view never writes back a stale copy.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void refresh(() => cancelled);
    meetingDetectionService.onChanged(() => { void refresh(() => cancelled); })
      .then((stop) => { if (cancelled) stop(); else unlisten = stop; })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refresh]);

  const run = useCallback(async (action: () => Promise<MeetingDetectionSettings>) => {
    setBusy(true);
    setError(null);
    try {
      setSettings(await action());
      return true;
    } catch (actionError) {
      setError(`Could not save: ${describeError(actionError)}`);
      return false;
    } finally {
      setBusy(false);
    }
  }, []);

  const ignored = useMemo(() => settings?.ignored_apps ?? [], [settings]);
  const suggestions = useMemo(
    () => recent.filter((app) => !ignored.some((entry) => sameApp(entry, app))).slice(0, 6),
    [ignored, recent],
  );

  const addApp = async (app: AudioApp) => {
    if (!app.name.trim()) return;
    if (await run(() => meetingDetectionService.ignoreApp(app))) setDraft('');
  };

  const addDraft = () => {
    const name = draft.trim();
    if (!name) return;
    // Typing the name of a recently seen app adds it with its bundle id.
    const known = recent.find((app) => normalizeAppName(app.name) === normalizeAppName(name));
    void addApp(known ?? { name, bundle_id: null });
  };

  const resume = async () => {
    try {
      await meetingDetectionService.resume();
      setSnoozedUntil(null);
    } catch (resumeError) {
      setError(`Could not resume detection: ${describeError(resumeError)}`);
    }
  };

  if (!settings) {
    return error ? <StatusFeedback tone="error">{error}</StatusFeedback> : null;
  }

  return (
    <div className="space-y-4 rounded-lg border p-4">
      <div className="flex items-center justify-between">
        <div className="flex-1 pr-4">
          <div className="font-medium">Only prompt for known meeting apps</div>
          <div className="text-sm text-ink-muted">
            Ask only for apps like Zoom, Teams, Meet, Slack and FaceTime. When off, any other app using the microphone can also trigger the prompt.
          </div>
        </div>
        <Switch
          aria-label="Only prompt for known meeting apps"
          checked={settings.only_known_apps}
          disabled={busy}
          onCheckedChange={(enabled) => void run(() => meetingDetectionService.setOnlyKnownApps(enabled))}
        />
      </div>

      <div className="border-t border-hairline pt-4">
        <div className="font-medium">Never ask for these apps</div>
        <div className="mb-3 text-sm text-ink-muted">
          Minutes won&apos;t offer to record when only these apps are using the microphone. You can also add an app from the prompt&apos;s &ldquo;…&rdquo; menu.
        </div>

        {ignored.length === 0 ? (
          <p className="mb-3 text-sm text-ink-subtle">No apps yet.</p>
        ) : (
          <ul aria-label="Ignored apps" className="mb-3 divide-y divide-hairline rounded-md border border-hairline">
            {ignored.map((app) => (
              <li key={`${app.bundle_id ?? ''}:${app.name}`} className="flex items-center justify-between gap-2 px-3 py-2">
                <div className="min-w-0">
                  <div className="truncate text-sm text-ink">{app.name}</div>
                  {app.bundle_id && <div className="truncate text-xs text-ink-subtle">{app.bundle_id}</div>}
                </div>
                <button
                  type="button"
                  aria-label={`Remove ${app.name}`}
                  disabled={busy}
                  onClick={() => void run(() => meetingDetectionService.unignoreApp(app))}
                  className="shrink-0 rounded-md p-1.5 text-ink-subtle hover:bg-surface-2 hover:text-ink disabled:opacity-40"
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </li>
            ))}
          </ul>
        )}

        <form
          className="flex items-center gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            addDraft();
          }}
        >
          <Input
            aria-label="App name to ignore"
            placeholder="Add app…"
            value={draft}
            disabled={busy}
            onChange={(event) => setDraft(event.target.value)}
            className="h-9"
          />
          <Button type="submit" variant="outline" size="sm" disabled={busy || !draft.trim()} className="h-9 shrink-0">
            Add
          </Button>
        </form>

        {suggestions.length > 0 && (
          <div className="mt-3">
            <div className="mb-1.5 text-xs text-ink-subtle">Recently used the microphone</div>
            <div className="flex flex-wrap gap-1.5">
              {suggestions.map((app) => (
                <button
                  key={`${app.bundle_id ?? ''}:${app.name}`}
                  type="button"
                  disabled={busy}
                  onClick={() => void addApp(app)}
                  className="inline-flex items-center gap-1 rounded-full border border-hairline bg-surface-2 px-2.5 py-1 text-xs text-ink hover:bg-surface-raised disabled:opacity-40"
                >
                  <Plus className="h-3 w-3" />
                  {app.name}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>

      {snoozedUntil && (
        <div className="flex items-center justify-between border-t border-hairline pt-4 text-sm text-ink-muted">
          <span>
            Detection is snoozed until {new Date(snoozedUntil * 1000).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}.
          </span>
          <Button variant="outline" size="sm" onClick={() => void resume()}>Resume now</Button>
        </div>
      )}

      {error && <StatusFeedback tone="error">{error}</StatusFeedback>}
    </div>
  );
}
