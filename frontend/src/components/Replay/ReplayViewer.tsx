'use client';

/* eslint-disable @typescript-eslint/no-explicit-any */

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { ArrowLeft, Loader2, PanelRightClose, PanelRightOpen } from 'lucide-react';
import { JUMP_TO_TIME_EVENT } from '@/lib/liveTimeline';
import { panel } from '@/lib/theme-classes';
import {
  buildIndex,
  buildLanes,
  countAtAudio,
  parseInteractive,
  parseJsonl,
  summarize,
  type Lanes,
  type ReplayIndex,
  type RunSummary,
} from '@/lib/replay/reducer';
import { ReplaySource, type ReplayBackend } from '@/lib/replay/replaySource';
import { ReplayPlayer } from '@/lib/replay/player';
import { replayApi, tauriReplayBackend, type ReplayRun } from '@/lib/replay/api';
import { ReplayWorkspace } from './ReplayWorkspace';
import { TransportBar } from './TransportBar';
import { DebugDrawer } from './DebugDrawer';

interface LoadedRun {
  index: ReplayIndex;
  lanes: Lanes;
  info: RunSummary;
  report: any | null;
  asks: any[];
  finals: Array<[number, number]>;
  audioUrl: string | null;
}

/** Everything the viewer derives from a run's files, computed once. */
export function loadRun(eventsText: string, reportText: string, interactiveText: string) {
  const records = parseJsonl(eventsText);
  const index = buildIndex(records);
  const lanes = buildLanes(records);
  const info = summarize(records);
  let report: any = null;
  try {
    report = reportText.trim() ? JSON.parse(reportText) : null;
  } catch {
    report = null;
  }
  // Finalized lines, [startSeconds, record index], for [MM:SS] citations.
  const finals: Array<[number, number]> = [];
  for (const r of records) {
    const p = r.payload || {};
    if (r.type === 'event' && r.name === 'transcript-update' && !p.is_partial && String(p.text || '').trim()) {
      finals.push([Number(p.audio_start_time) || 0, r.i]);
    }
  }
  return { index, lanes, info, report, asks: parseInteractive(interactiveText), finals };
}

const DRAWER_KEY = 'meetily:replay-drawer-open';
const PAUSE_KEY = 'meetily:replay-pause-while-answering';

function storedFlag(key: string, fallback: boolean): boolean {
  try {
    const value = localStorage.getItem(key);
    return value === null ? fallback : value === 'true';
  } catch {
    return fallback;
  }
}

function storeFlag(key: string, value: boolean) {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    // Remembered for this window only.
  }
}

/** The cursor, throttled for the drawer (its tables are long). */
function useThrottledCursor(player: ReplayPlayer, source: ReplaySource, everyMs: number) {
  const [cursor, setCursor] = useState({ n: source.n, ms: player.cursorMs });
  useEffect(() => {
    let last = 0;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const update = () => {
      const now = performance.now();
      const apply = () => {
        last = performance.now();
        timer = null;
        setCursor((c) => (c.n === source.n && c.ms === player.cursorMs ? c : { n: source.n, ms: player.cursorMs }));
      };
      if (now - last >= everyMs) apply();
      else if (!timer) timer = setTimeout(apply, everyMs - (now - last));
    };
    const off = player.onFrame(update);
    return () => {
      off();
      if (timer) clearTimeout(timer);
    };
  }, [player, source, everyMs]);
  return cursor;
}

export function ReplayViewer({ run, onBack }: { run: string; onBack: () => void }) {
  const [loaded, setLoaded] = useState<LoadedRun | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [meta, setMeta] = useState<ReplayRun | null>(null);
  const backend = useMemo(() => tauriReplayBackend(run), [run]);

  useEffect(() => {
    let cancelled = false;
    setLoaded(null);
    setError(null);
    (async () => {
      const [events, report, interactive, audioUrl, runs] = await Promise.all([
        replayApi.readFile(run, 'events'),
        replayApi.readFile(run, 'report').catch(() => ''),
        replayApi.readFile(run, 'interactive').catch(() => ''),
        replayApi.audioUrl(run).catch(() => null),
        replayApi.listRuns().catch(() => [] as ReplayRun[]),
      ]);
      if (cancelled) return;
      // Let the loading state paint before the fold.
      await new Promise((resolve) => setTimeout(resolve, 0));
      const started = performance.now();
      const data = loadRun(events, report, interactive);
      console.info(`[replay] ${run}: ${data.index.records.length} events indexed in ${Math.round(performance.now() - started)} ms`);
      if (cancelled) return;
      setMeta(runs.find((r) => r.name === run) ?? null);
      setLoaded({ ...data, audioUrl });
    })().catch((e) => {
      if (!cancelled) setError(e instanceof Error ? e.message : String(e));
    });
    return () => {
      cancelled = true;
    };
  }, [run]);

  if (error) {
    return (
      <div className="flex h-screen flex-col items-center justify-center gap-3 bg-surface-0 px-8 text-center">
        <p className={`max-w-lg rounded-lg px-4 py-3 text-sm ${panel.error}`} role="alert">Could not open this run: {error}</p>
        <button type="button" onClick={onBack} className="text-sm text-ink-muted underline underline-offset-4 hover:text-ink">Back to runs</button>
      </div>
    );
  }
  if (!loaded) {
    return (
      <div role="status" className="flex h-screen items-center justify-center gap-2 bg-surface-0 text-sm text-ink-muted">
        <Loader2 className="h-4 w-4 animate-spin" /> Loading {run}…
      </div>
    );
  }
  return <LoadedReplay key={run} run={run} meta={meta} loaded={loaded} onBack={onBack} backend={backend} />;
}

function LoadedReplay({
  run,
  meta,
  loaded,
  onBack,
  backend,
}: {
  run: string;
  meta: ReplayRun | null;
  loaded: LoadedRun;
  onBack: () => void;
  backend: ReplayBackend;
}) {
  const source = useMemo(() => new ReplaySource(loaded.index, backend, `replay-${run}`), [loaded.index, backend, run]);
  const player = useMemo(() => new ReplayPlayer(source), [source]);
  const view = useSyncExternalStore(source.subscribe, source.getView, source.getView);
  const playerState = useSyncExternalStore(player.subscribe, player.getState, player.getState);
  const [drawerOpen, setDrawerOpen] = useState(() => storedFlag(DRAWER_KEY, true));
  const [pauseWhileAnswering, setPauseWhileAnswering] = useState(() => storedFlag(PAUSE_KEY, true));
  const [asks, setAsks] = useState<any[]>(loaded.asks);
  const [selected, setSelected] = useState<number | null>(null);
  const pauseRef = useRef(pauseWhileAnswering);
  pauseRef.current = pauseWhileAnswering;
  const cursor = useThrottledCursor(player, source, 250);

  // Playback pauses while a live answer streams (a toggle in the drawer).
  useEffect(() => {
    source.onLiveAsk = () => {
      if (pauseRef.current) player.setPlaying(false);
    };
    return () => {
      source.onLiveAsk = null;
    };
  }, [source, player]);

  // Live Copilot progress from Rust; finished asks join the saved list.
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void replayApi.onCopilotEvent((event) => {
      if (event.run !== run) return;
      source.handleLiveEvent(event.requestId, event.event, event.data);
      if (event.event === 'done' && event.data) setAsks((current) => (current.some((a) => a.id === event.data.id) ? current : [...current, event.data]));
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    }).catch((e) => console.warn('[replay] could not listen for live Copilot events:', e));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [run, source]);

  // The recording, streamed by the webview from the asset protocol.
  const audioRef = useRef<HTMLAudioElement | null>(null);
  useEffect(() => {
    if (!loaded.audioUrl) {
      player.attachAudio(null);
      return undefined;
    }
    const audio = new Audio();
    audio.preload = 'auto';
    audio.src = loaded.audioUrl;
    audioRef.current = audio;
    player.attachAudio(audio);
    return () => {
      audio.pause();
      audioRef.current = null;
    };
  }, [loaded.audioUrl, player]);

  // Leaving stops the audio and any live answer.
  useEffect(() => () => {
    player.dispose();
    source.dispose();
    void replayApi.cancelCopilot().catch(() => {});
  }, [player, source]);

  // Start at the beginning, paused.
  useEffect(() => {
    player.seekMs(0);
  }, [player]);

  // [MM:SS] citations: move the cursor to when that line was transcribed (or
  // keep it, if later), then let the transcript show the line.
  useEffect(() => {
    let own = false;
    const onJump = (event: Event) => {
      if (own) return;
      const seconds = (event as CustomEvent<{ seconds?: number }>).detail?.seconds;
      if (typeof seconds !== 'number') return;
      let best: number | null = null;
      for (const [start, i] of loaded.finals) {
        if (start <= seconds + 0.5) best = i;
        else break;
      }
      player.setPlaying(false);
      if (best !== null && best + 1 > source.n) player.seekCount(best + 1);
      else if (best === null) player.seekCount(countAtAudio(source.records, seconds * 1000));
      requestAnimationFrame(() => {
        own = true;
        window.dispatchEvent(new CustomEvent(JUMP_TO_TIME_EVENT, { detail: { seconds } }));
        own = false;
      });
    };
    window.addEventListener(JUMP_TO_TIME_EVENT, onJump);
    return () => window.removeEventListener(JUMP_TO_TIME_EVENT, onJump);
  }, [loaded.finals, player, source]);

  const togglePlaying = useCallback((playing: boolean) => player.setPlaying(playing), [player]);
  const seekCount = useCallback((n: number) => player.seekCount(n), [player]);
  const seekMeeting = useCallback((ms: number) => {
    player.setPlaying(false);
    player.seekMeetingMs(ms);
  }, [player]);

  // For measuring and scripting (DevTools).
  useEffect(() => {
    (window as any).replayViewer = {
      player,
      source,
      fps: () => player.getState().fps,
      seekMeeting: (ms: number) => player.seekMeetingMs(ms),
      play: (on = true) => player.setPlaying(on),
      setSpeed: (speed: number) => player.setSpeed(speed),
      cursor: () => ({ n: source.n, audioMs: source.current.audioMs, records: source.records.length }),
    };
    return () => {
      delete (window as any).replayViewer;
    };
  }, [player, source]);

  const meetingTitle = meta?.recording || loaded.info.meta?.audio_name || run;
  const createdAt = meta?.started_at || loaded.info.meta?.started_at || new Date().toISOString();

  return (
    <div className="flex h-screen min-w-0 flex-col bg-surface-0">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-hairline bg-surface-1 px-3 text-xs text-ink-muted">
        <button type="button" onClick={onBack} className="flex items-center gap-1 rounded px-1.5 py-1 hover:bg-surface-2 hover:text-ink">
          <ArrowLeft className="h-3.5 w-3.5" /> Runs
        </button>
        <span className="truncate font-mono text-[11px] text-ink-subtle">{run}</span>
        <span className="ml-auto hidden text-[11px] text-ink-subtle md:inline">Replay: nothing here records, transcribes or saves meetings.</span>
        <button
          type="button"
          onClick={() => setDrawerOpen((open) => {
            storeFlag(DRAWER_KEY, !open);
            return !open;
          })}
          aria-pressed={drawerOpen}
          aria-label={drawerOpen ? 'Hide debug drawer' : 'Show debug drawer'}
          title={drawerOpen ? 'Hide debug drawer' : 'Show debug drawer'}
          className="flex h-7 w-7 items-center justify-center rounded hover:bg-surface-2 hover:text-ink"
        >
          {drawerOpen ? <PanelRightClose className="h-4 w-4" /> : <PanelRightOpen className="h-4 w-4" />}
        </button>
      </div>
      <div className="flex min-h-0 flex-1">
        <ReplayWorkspace
          source={source}
          view={view}
          title={meetingTitle}
          createdAt={createdAt}
          playing={playerState.playing}
          onTogglePlaying={togglePlaying}
        />
        {drawerOpen && (
          <DebugDrawer
            runName={run}
            recording={meta?.recording ?? null}
            info={loaded.info}
            lanes={loaded.lanes}
            records={source.records}
            report={loaded.report}
            asks={asks}
            live={view.live}
            n={cursor.n}
            axis={playerState.axis}
            cursorMs={cursor.ms}
            endMs={player.endOf()}
            fps={playerState.playing ? playerState.fps : 0}
            pauseWhileAnswering={pauseWhileAnswering}
            onPauseWhileAnsweringChange={(value) => {
              storeFlag(PAUSE_KEY, value);
              setPauseWhileAnswering(value);
            }}
            onSeekCount={seekCount}
            onSeekMeeting={seekMeeting}
            selected={selected}
          />
        )}
      </div>
      <TransportBar
        player={player}
        lanes={loaded.lanes}
        records={source.records}
        names={loaded.info.names}
        selected={selected}
        onSelect={setSelected}
      />
    </div>
  );
}
