'use client';

/* eslint-disable @typescript-eslint/no-explicit-any */

import { memo, useMemo, useState, type ReactNode } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { Switch } from '@/components/ui/switch';
import { CitedMarkdown } from '@/components/MeetingDetails/CitedMarkdown';
import { badge, panel, type Tone } from '@/lib/theme-classes';
import { clock, type Lanes, type ReplayRecord, type RunSummary } from '@/lib/replay/reducer';
import type { LiveAskProgress } from '@/lib/replay/replaySource';

const cell = 'px-1.5 py-1 align-top';
const timeButton = 'rounded px-1 font-mono text-[11px] tabular-nums text-ink underline decoration-hairline underline-offset-2 hover:bg-surface-2';

function Badge({ tone, children }: { tone: Tone; children: ReactNode }) {
  return <span className={`inline-block rounded px-1.5 py-px text-[10px] font-semibold ${badge[tone]}`}>{children}</span>;
}

function Section({ title, count, defaultOpen = false, children }: { title: string; count?: number | string; defaultOpen?: boolean; children: ReactNode }) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <section className="border-b border-hairline">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center gap-1 px-4 py-2.5 text-left text-[11px] font-bold uppercase tracking-[0.08em] text-ink-muted hover:text-ink"
      >
        {open ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        {title}
        {count !== undefined && <span className="font-normal normal-case tracking-normal text-ink-subtle">({count})</span>}
      </button>
      {open && <div className="px-4 pb-3">{children}</div>}
    </section>
  );
}

function statsLine(stats: any, extra?: string): string {
  if (!stats) return extra ?? '';
  const parts = [
    `first words ${stats.firstTokenMs ?? '–'} ms`,
    `${stats.tokensPerSecond != null ? Number(stats.tokensPerSecond).toFixed(1) : '–'} tok/s`,
    `reused ${stats.reusedTokens ?? '–'}/${stats.promptTokens ?? '–'}`,
  ];
  if (extra) parts.push(extra);
  return parts.join(' · ');
}

const DETECTOR_TONE = (step: string): Tone =>
  step === 'confirm_yes' ? 'warning' : step === 'skipped' || step === 'rule_reject' || step === 'confirm_no' ? 'neutral' : step?.startsWith('confirm_') && step !== 'confirming' ? 'error' : 'info';

/** One entry of interactive.jsonl, or one asked in this session. */
function LiveAsk({ entry, progress, onSeekMeeting }: { entry: any; progress?: LiveAskProgress; onSeekMeeting: (ms: number) => void }) {
  const status: string = entry?.status ?? progress?.phase ?? 'sent';
  const tone: Tone = status === 'done' ? 'success' : status === 'error' ? 'error' : status === 'stopped' ? 'neutral' : 'info';
  const audioMs: number = entry?.audio_ms ?? progress?.audioMs ?? 0;
  return (
    <li className="rounded-lg border border-hairline bg-surface-0 p-2.5">
      <div className="flex flex-wrap items-center gap-1.5 text-[11px] text-ink-muted">
        <span>You asked live at</span>
        <button type="button" className={timeButton} onClick={() => onSeekMeeting(audioMs)}>[{clock(audioMs)}]</button>
        <Badge tone={tone}>{status}</Badge>
        {entry?.as_of_ms != null && <span className="text-ink-subtle">as of {clock(entry.as_of_ms)}</span>}
      </div>
      <p className="mt-1.5 text-xs font-medium text-ink">{entry?.prompt ?? '…'}</p>
      {entry?.reply ? (
        <div className="prose prose-sm mt-1 max-w-full text-xs leading-5 text-ink-muted">
          <CitedMarkdown>{entry.reply}</CitedMarkdown>
        </div>
      ) : progress && !entry ? (
        <p className="mt-1 text-xs text-ink-subtle">
          {progress.phase === 'queued' ? 'Waiting for the previous answer…' : progress.phase === 'warming' ? 'Loading the model…' : 'Answering in the Copilot panel…'}
          {progress.context && ` Context: ${progress.context.lines} lines, ${progress.context.timeline} summary points.`}
        </p>
      ) : null}
      {entry?.error && <p className={`mt-1 rounded px-2 py-1 text-[11px] ${panel.error}`}>{entry.error}</p>}
      {entry && (
        <p className="mt-1 font-mono text-[10px] leading-4 text-ink-subtle">
          {statsLine(entry.stats, entry.warm_ms != null ? `warm ${entry.warm_ms} ms` : undefined)}
          {entry.context && ` · ${entry.context.lines} lines, ${entry.context.timeline} points`}
          {entry.provider && ` · ${entry.provider}/${entry.model}`}
        </p>
      )}
    </li>
  );
}

/** Backlog and RTF over the run, with the cursor. */
function Sparkline({ series, axis, cursorMs, endMs }: { series: Lanes['series']; axis: 'audio' | 'wall'; cursorMs: number; endMs: number }) {
  const width = 320;
  const height = 64;
  const { backlog, rtf, maxBacklog } = useMemo(() => {
    const stride = Math.max(1, Math.floor(series.length / 400));
    const points = series.filter((_, k) => k % stride === 0);
    const max = Math.max(2, ...points.map((s) => s.backlog));
    const x = (ms: number) => (endMs > 0 ? (ms / endMs) * width : 0);
    const y = (v: number, scale: number) => height - 2 - Math.min(1, v / scale) * (height - 4);
    return {
      backlog: points.map((s) => `${x(s[axis]).toFixed(1)},${y(s.backlog, max).toFixed(1)}`).join(' '),
      rtf: points.filter((s) => s.rtf != null).map((s) => `${x(s[axis]).toFixed(1)},${y(s.rtf as number, 1.5).toFixed(1)}`).join(' '),
      maxBacklog: max,
    };
  }, [series, axis, endMs]);
  const at = useMemo(() => {
    let best: Lanes['series'][number] | null = null;
    for (const s of series) {
      if (s[axis] <= cursorMs) best = s;
      else break;
    }
    return best;
  }, [series, axis, cursorMs]);
  if (!series.length) return <p className="text-xs text-ink-subtle">No live measurements.</p>;
  const cx = endMs > 0 ? (cursorMs / endMs) * width : 0;
  return (
    <div>
      <svg viewBox={`0 0 ${width} ${height}`} className="h-16 w-full rounded border border-hairline bg-surface-0" preserveAspectRatio="none" role="img" aria-label="Live backlog and real-time factor">
        <line x1={0} x2={width} y1={height - 2 - (0.9 / 1.5) * (height - 4)} y2={height - 2 - (0.9 / 1.5) * (height - 4)} className="stroke-error" strokeDasharray="3 3" strokeWidth={0.75} />
        <polyline points={rtf} fill="none" className="stroke-info" strokeWidth={1} />
        <polyline points={backlog} fill="none" className="stroke-warning" strokeWidth={1} />
        <line x1={cx} x2={cx} y1={0} y2={height} className="stroke-recording" strokeWidth={1.25} />
      </svg>
      <p className="mt-1 font-mono text-[10px] text-ink-subtle">
        <span className="text-warning">backlog</span> {at ? `${at.backlog.toFixed(1)} s` : '–'} (max scale {maxBacklog.toFixed(0)} s) · <span className="text-info">RTF</span> {at?.rtf != null ? at.rtf.toFixed(2) : '–'} · limit 0.9{at?.llm ? ' · model busy' : ''}
      </p>
    </div>
  );
}

export const DebugDrawer = memo(function DebugDrawer({
  runName,
  recording,
  info,
  lanes,
  records,
  report,
  asks,
  live,
  n,
  axis,
  cursorMs,
  endMs,
  fps,
  pauseWhileAnswering,
  onPauseWhileAnsweringChange,
  onSeekCount,
  onSeekMeeting,
  selected,
}: {
  runName: string;
  recording: string | null;
  info: RunSummary;
  lanes: Lanes;
  records: ReplayRecord[];
  report: any | null;
  asks: any[];
  live: LiveAskProgress[];
  n: number;
  axis: 'audio' | 'wall';
  cursorMs: number;
  endMs: number;
  fps: number;
  pauseWhileAnswering: boolean;
  onPauseWhileAnsweringChange: (value: boolean) => void;
  onSeekCount: (n: number) => void;
  onSeekMeeting: (ms: number) => void;
  selected: number | null;
}) {
  const [detectorFilter, setDetectorFilter] = useState('');
  const meta = info.meta ?? {};
  const criteria: any[] = Array.isArray(report?.criteria) ? report.criteria : [];
  const passed = criteria.filter((c) => c.status === 'pass').length;
  const failed = criteria.filter((c) => c.status === 'fail').length;
  const steps = useMemo(() => [...new Set(info.detector.map((d) => d.step))].filter(Boolean).sort(), [info.detector]);
  const detector = useMemo(
    () => info.detector.filter((d) => !detectorFilter || d.step === detectorFilter),
    [info.detector, detectorFilter],
  );
  const warnings = lanes.lanes.log ?? [];
  const past = (i: number) => (i < n ? '' : 'opacity-50');
  const savedIds = new Set(asks.map((a) => a.id));
  const pending = live.filter((p) => !savedIds.has(p.id) && !p.entry);
  const sessionDone = live.filter((p) => p.entry && !savedIds.has(p.id)).map((p) => p.entry);
  const allAsks = [...asks, ...sessionDone];
  const selectedRecord = selected !== null ? records[selected] : null;
  const mode = meta.realtime ? 'real time' : meta.speed === 0 || meta.realtime === false ? 'unpaced (simulated meeting clock)' : `${meta.speed ?? 1}×`;

  const timeCell = (i: number, audioMs: number) => (
    <td className={cell}>
      <button type="button" className={timeButton} onClick={() => onSeekCount(i + 1)}>{clock(audioMs)}</button>
    </td>
  );

  return (
    <aside aria-label="Replay debug" className="flex h-full w-[380px] shrink-0 flex-col overflow-y-auto border-l border-hairline bg-surface-1 text-ink">
      <div className="border-b border-hairline px-4 py-3">
        <p className="text-[11px] font-bold uppercase tracking-[0.08em] text-ink-subtle">Run</p>
        <h2 className="mt-0.5 truncate text-sm font-semibold" title={runName}>{recording || runName}</h2>
        <p className="mt-1 text-[11px] leading-4 text-ink-muted">
          {meta.duration_s ? `${(meta.duration_s / 60).toFixed(1)} min` : ''} · {mode} · live: {meta.live_engine || '?'} · model: {meta.summary_model || '?'} · {records.length} events · replay took {clock(info.endMs)}
        </p>
        <p className="mt-1 font-mono text-[10px] text-ink-subtle">{runName} · {fps ? `${fps.toFixed(0)} fps` : 'paused'}</p>
        {(meta.limitations ?? []).map((text: string) => (
          <p key={text} className={`mt-2 rounded px-2 py-1 text-[11px] leading-4 ${panel.neutral}`}>{text}</p>
        ))}
      </div>

      <Section title="Scorecard" count={report ? `${passed} passed · ${failed} failed` : 'no report'} defaultOpen>
        {criteria.length === 0 ? (
          <p className="text-xs text-ink-subtle">No report.json yet: the run has not finished.</p>
        ) : (
          <table className="w-full text-[11px]">
            <tbody>
              {criteria.map((c) => (
                <tr key={c.id ?? c.label} className="border-t border-hairline">
                  <td className={cell}>
                    {c.label}
                    {(c.detail ?? []).length > 0 && (
                      <details className="mt-0.5 text-ink-subtle">
                        <summary className="cursor-pointer">{c.detail.length} details</summary>
                        <ul className="list-disc pl-4">{c.detail.map((d: string) => <li key={d}>{d}</li>)}</ul>
                      </details>
                    )}
                  </td>
                  <td className={cell}>
                    <Badge tone={c.status === 'pass' ? 'success' : c.status === 'fail' ? 'error' : c.status === 'info' ? 'info' : 'neutral'}>
                      {c.status === 'pass' ? 'Pass' : c.status === 'fail' ? 'Fail' : c.status === 'not_applicable' ? 'n/a' : c.status}
                    </Badge>
                  </td>
                  <td className={`${cell} text-ink-muted`}>{c.value}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>

      <Section title="Live asks" count={allAsks.length + pending.length} defaultOpen>
        <label className="mb-2 flex items-center justify-between gap-2 text-[11px] text-ink-muted">
          Pause playback while Copilot answers
          <Switch checked={pauseWhileAnswering} onCheckedChange={onPauseWhileAnsweringChange} aria-label="Pause playback while Copilot answers" />
        </label>
        {allAsks.length + pending.length === 0 ? (
          <p className="text-xs text-ink-subtle">Ask in the Copilot panel at any moment: the real Copilot answers as of the cursor. Asks are saved to this run&apos;s interactive.jsonl.</p>
        ) : (
          <ol className="space-y-2">
            {allAsks.map((entry) => (
              <LiveAsk key={entry.id ?? entry.asked_at} entry={entry} onSeekMeeting={onSeekMeeting} />
            ))}
            {pending.map((progress) => (
              <LiveAsk key={progress.id} entry={null} progress={progress} onSeekMeeting={onSeekMeeting} />
            ))}
          </ol>
        )}
      </Section>

      <Section title="Copilot requests" count={info.requests.length}>
        {info.requests.length === 0 ? <p className="text-xs text-ink-subtle">No Copilot requests.</p> : (
          <table className="w-full text-[11px]">
            <thead className="text-left text-ink-subtle">
              <tr><th className={cell}>At</th><th className={cell}>Kind</th><th className={cell}>Status</th><th className={cell}>First</th><th className={cell}>tok/s</th><th className={cell}>Reused</th></tr>
            </thead>
            <tbody>
              {info.requests.map((r) => {
                const s = r.stats || {};
                return (
                  <tr key={r.i} className={`border-t border-hairline ${past(r.i)}`}>
                    {timeCell(r.i, r.audioMs)}
                    <td className={cell}>{r.kind}</td>
                    <td className={cell}><Badge tone={r.status === 'done' ? 'success' : r.status === 'error' ? 'error' : 'neutral'}>{r.status}</Badge></td>
                    <td className={`${cell} font-mono`}>{s.firstTokenMs ?? '–'}</td>
                    <td className={`${cell} font-mono`}>{s.tokensPerSecond != null ? Number(s.tokensPerSecond).toFixed(1) : '–'}</td>
                    <td className={`${cell} font-mono`}>{s.reusedTokens ?? '–'}/{s.promptTokens ?? '–'}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </Section>

      <Section title="Detector" count={info.detector.length}>
        <select
          value={detectorFilter}
          onChange={(event) => setDetectorFilter(event.target.value)}
          aria-label="Detector step"
          className="mb-2 h-7 rounded-md border border-hairline bg-surface-0 px-2 text-[11px] text-ink"
        >
          <option value="">All steps</option>
          {steps.map((step) => <option key={step} value={step}>{step}</option>)}
        </select>
        {detector.length === 0 ? <p className="text-xs text-ink-subtle">No detector decisions.</p> : (
          <table className="w-full text-[11px]">
            <tbody>
              {detector.map((d) => (
                <tr key={d.i} className={`border-t border-hairline ${past(d.i)}`}>
                  {timeCell(d.i, d.audioMs)}
                  <td className={cell}><Badge tone={DETECTOR_TONE(d.step)}>{d.step}</Badge></td>
                  <td className={cell}>{d.decision}</td>
                  <td className={`${cell} text-ink-subtle`}>line {clock(d.lineMs)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>

      <Section title="llama-helper" count={info.sidecar.length}>
        {info.sidecar.length === 0 ? <p className="text-xs text-ink-subtle">No llama-helper activity.</p> : (
          <table className="w-full text-[11px]">
            <tbody>
              {info.sidecar.map((s) => (
                <tr key={s.i} className={`border-t border-hairline ${past(s.i)}`}>
                  {timeCell(s.i, s.audioMs)}
                  <td className={cell}><Badge tone={s.what === 'preempt' || s.what === 'timeline-preempted' ? 'warning' : s.what === 'timeout' ? 'error' : 'neutral'}>{s.what}</Badge></td>
                  <td className={`${cell} font-mono`}>{s.id ?? ''}</td>
                  <td className={cell}>{s.priority ?? ''}</td>
                  <td className={`${cell} text-ink-subtle`}>{s.detail}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>

      <Section title="Live backlog / RTF" defaultOpen>
        <Sparkline series={lanes.series} axis={axis} cursorMs={cursorMs} endMs={endMs} />
      </Section>

      <Section title="Warnings" count={warnings.length}>
        {warnings.length === 0 ? <p className="text-xs text-ink-subtle">No warnings.</p> : (
          <ul className="space-y-1 text-[11px]">
            {warnings.map((w) => (
              <li key={w.i} className={`flex gap-1.5 ${past(w.i)}`}>
                <button type="button" className={timeButton} onClick={() => onSeekCount(w.i + 1)}>{clock(records[w.i]?.audio_ms ?? 0)}</button>
                <span className={w.tone === 'error' ? 'text-error' : 'text-warning'}>{w.label}</span>
              </li>
            ))}
          </ul>
        )}
      </Section>

      {selectedRecord && (
        <Section title="Selected event" defaultOpen>
          <p className="text-[11px] text-ink-muted">
            <b className="text-ink">{selectedRecord.name}</b> · {selectedRecord.type} · #{selectedRecord.i} · {clock(selectedRecord.audio_ms)} meeting · {clock(selectedRecord.t_ms)} wall
          </p>
          <pre className="mt-1 max-h-64 overflow-auto rounded bg-surface-0 p-2 font-mono text-[10px] leading-4 text-ink-muted">
            {JSON.stringify(selectedRecord.payload, null, 2)}
          </pre>
        </Section>
      )}
    </aside>
  );
});
