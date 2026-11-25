'use client';

import { useCallback, useEffect, useState } from 'react';
import { Clapperboard, Loader2, RotateCw } from 'lucide-react';
import { badge, panel, type Tone } from '@/lib/theme-classes';
import { replayApi, type ReplayRun } from '@/lib/replay/api';

const STATUS_TONE: Record<ReplayRun['status'], Tone> = { done: 'success', running: 'info', incomplete: 'warning' };

function length(seconds: number | null): string {
  if (!seconds) return '–';
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds % 60);
  return minutes >= 60 ? `${Math.floor(minutes / 60)} h ${minutes % 60} min` : `${minutes} min ${String(rest).padStart(2, '0')} s`;
}

function date(iso: string | null, name: string): string {
  const parsed = iso ? new Date(iso) : null;
  if (parsed && !Number.isNaN(parsed.getTime())) {
    return parsed.toLocaleString([], { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
  }
  return name.slice(0, 15);
}

export function RunList({ onOpen }: { onOpen: (run: string) => void }) {
  const [runs, setRuns] = useState<ReplayRun[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setRuns(await replayApi.listRuns());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-surface-0 text-ink">
      <header className="flex shrink-0 items-end justify-between gap-4 px-8 pb-4 pt-10">
        <div>
          <h1 className="font-serif text-[26px] font-semibold tracking-[-0.02em]">Replays</h1>
          <p className="mt-0.5 text-xs text-ink-subtle">
            Pipeline replays on this Mac, played back through the recording screen. Runs never leave this machine.
          </p>
        </div>
        <button
          type="button"
          onClick={() => void refresh()}
          disabled={loading}
          className="flex h-8 items-center gap-1.5 rounded-full border border-hairline px-3 text-xs text-ink-muted hover:bg-surface-2 hover:text-ink disabled:opacity-50"
        >
          {loading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <RotateCw className="h-3.5 w-3.5" />} Refresh
        </button>
      </header>
      <main className="min-h-0 flex-1 overflow-y-auto px-8 pb-10">
        {error && <p role="alert" className={`mb-4 rounded-lg px-4 py-3 text-sm ${panel.error}`}>Could not list runs: {error}</p>}
        {runs === null && !error ? (
          <p role="status" className="flex items-center gap-2 text-sm text-ink-muted"><Loader2 className="h-4 w-4 animate-spin" /> Looking for runs…</p>
        ) : runs && runs.length === 0 ? (
          <div className="mx-auto mt-16 max-w-xl rounded-2xl border border-hairline bg-surface-1 p-8 text-center">
            <Clapperboard className="mx-auto h-6 w-6 text-ink-subtle" />
            <h2 className="mt-3 text-base font-semibold">No replays yet</h2>
            <p className="mt-2 text-sm leading-6 text-ink-muted">
              A replay feeds a recorded meeting through the live pipeline and records every event. Make one from a
              checkout of the repository; it appears here when it starts.
            </p>
            <pre className="mt-4 overflow-x-auto rounded-lg bg-surface-0 p-3 text-left font-mono text-[11px] leading-5 text-ink">
{`cargo build --release -p llama-helper
cargo run --release -p meetily --features dev-replay \\
  --example replay -- --audio "/path/to/meeting/audio.mp4"`}
            </pre>
            <p className="mt-3 text-xs text-ink-subtle">Runs are written to the app data folder&apos;s replays directory. Starting a replay from here is not available yet.</p>
          </div>
        ) : runs ? (
          <table className="w-full text-sm">
            <thead className="text-left text-[11px] uppercase tracking-[0.08em] text-ink-subtle">
              <tr className="border-b border-hairline">
                <th className="px-3 py-2 font-semibold">Recording</th>
                <th className="px-3 py-2 font-semibold">Length</th>
                <th className="px-3 py-2 font-semibold">Status</th>
                <th className="px-3 py-2 font-semibold">Report</th>
                <th className="px-3 py-2 font-semibold">Live asks</th>
                <th className="px-3 py-2 font-semibold">Date</th>
              </tr>
            </thead>
            <tbody>
              {runs.map((run) => (
                <tr
                  key={run.name}
                  className="cursor-pointer border-b border-hairline hover:bg-surface-1"
                  onClick={() => onOpen(run.name)}
                >
                  <td className="px-3 py-2.5">
                    <button type="button" className="text-left font-medium text-ink hover:underline" onClick={(event) => { event.stopPropagation(); onOpen(run.name); }}>
                      {run.recording || run.audio_name || run.name}
                    </button>
                    <p className="font-mono text-[11px] text-ink-subtle">{run.name}</p>
                  </td>
                  <td className="px-3 py-2.5 tabular-nums text-ink-muted">{length(run.duration_s)}</td>
                  <td className="px-3 py-2.5">
                    <span className={`rounded px-1.5 py-0.5 text-[11px] font-semibold ${badge[STATUS_TONE[run.status] ?? 'neutral']}`}>{run.status}</span>
                  </td>
                  <td className="px-3 py-2.5 text-xs">
                    {run.passed == null ? <span className="text-ink-subtle">–</span> : (
                      <span className="flex gap-1.5">
                        <span className={`rounded px-1.5 py-0.5 font-semibold ${badge.success}`}>{run.passed} passed</span>
                        <span className={`rounded px-1.5 py-0.5 font-semibold ${run.failed ? badge.error : badge.neutral}`}>{run.failed ?? 0} failed</span>
                      </span>
                    )}
                  </td>
                  <td className="px-3 py-2.5 tabular-nums text-ink-muted">{run.interactive || '–'}</td>
                  <td className="px-3 py-2.5 text-ink-muted">{date(run.started_at, run.name)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : null}
      </main>
    </div>
  );
}
