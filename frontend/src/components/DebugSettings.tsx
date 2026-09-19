'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { Switch } from './ui/switch';
import { Button } from './ui/button';
import { useDebugMode } from '@/hooks/useDebugMode';
import { setDebugMode } from '@/lib/debugMode';

interface DebugInfo {
  version: string;
  platform: string;
  appDataDir: string;
  recordingsDir: string;
  meetings: number;
  debugMeetings: number;
  transcripts: number;
}

/**
 * Settings section for Debug mode: isolate test recordings and expose diagnostics.
 */
export function DebugSettings() {
  const enabled = useDebugMode();
  const [info, setInfo] = useState<DebugInfo | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setInfo(await invoke<DebugInfo>('get_debug_info'));
    } catch (error) {
      console.warn('Failed to load debug info:', error);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh, enabled]);

  const toggle = async (next: boolean) => {
    try {
      await setDebugMode(next);
      toast.success(next ? 'Debug mode on' : 'Debug mode off');
    } catch (error) {
      toast.error(`Could not update debug mode: ${String(error)}`);
    }
  };

  const copyDiagnostics = async () => {
    if (!info) return;
    try {
      await navigator.clipboard.writeText(JSON.stringify(info, null, 2));
      toast.success('Diagnostics copied');
    } catch {
      toast.error('Could not copy diagnostics');
    }
  };

  const clearDebug = async () => {
    setBusy(true);
    try {
      const deleted = await invoke<number>('delete_debug_meetings');
      toast.success(`Deleted ${deleted} debug meeting${deleted === 1 ? '' : 's'}`);
      await refresh();
    } catch (error) {
      toast.error(`Could not delete debug meetings: ${String(error)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between rounded-lg border border-hairline p-4">
        <div className="flex-1 pr-4">
          <div className="font-medium">Debug mode</div>
          <div className="text-sm text-ink-muted">
            Record test meetings into an isolated debug bucket — hidden from your list and stored
            under a <span className="font-mono">recordings/debug</span> folder — and enable verbose
            logging.
          </div>
        </div>
        <Switch aria-label="Enable debug mode" checked={enabled} onCheckedChange={toggle} />
      </div>

      {enabled && (
        <div className="rounded-lg border border-hairline p-4">
          <div className="mb-3 font-medium">Diagnostics</div>
          {info ? (
            <dl className="space-y-1 text-xs">
              {(
                [
                  ['Version', info.version],
                  ['Platform', info.platform],
                  ['Database', info.appDataDir],
                  ['Recordings', info.recordingsDir],
                  ['Meetings', String(info.meetings)],
                  ['Debug meetings', String(info.debugMeetings)],
                  ['Transcript segments', String(info.transcripts)],
                ] as const
              ).map(([label, value]) => (
                <div key={label} className="flex gap-2">
                  <dt className="w-36 shrink-0 text-ink-muted">{label}</dt>
                  <dd className="break-all font-mono text-ink">{value}</dd>
                </div>
              ))}
            </dl>
          ) : (
            <p className="text-xs text-ink-muted">Loading diagnostics…</p>
          )}

          <div className="mt-3 flex flex-wrap items-center gap-2">
            <Button size="sm" variant="outline" onClick={() => void copyDiagnostics()} disabled={!info}>
              Copy diagnostics
            </Button>
            <Button size="sm" variant="outline" onClick={() => void refresh()}>
              Refresh
            </Button>
            <Button
              size="sm"
              variant="destructive"
              onClick={() => void clearDebug()}
              disabled={busy || !info || info.debugMeetings === 0}
            >
              Delete debug meetings
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
