'use client';

import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { Pause, Play, SkipBack, SkipForward, Volume2, VolumeX } from 'lucide-react';
import { SegmentedControl } from '@/components/ui/segmented-control';
import { clock, nextOfName, type Lanes, type ReplayRecord } from '@/lib/replay/reducer';
import type { ReplayPlayer } from '@/lib/replay/player';
import { AUDIO_MAX_RATE, SPEEDS, type AudioReason } from '@/lib/replay/transport';
import { ReplayTimeline } from './ReplayTimeline';

const AUDIO_NOTE: Record<AudioReason, string> = {
  unavailable: 'No recording for this run',
  wall: 'Audio follows meeting time',
  fast: `Audio muted above ${AUDIO_MAX_RATE}×`,
  ended: 'Past the end of the recording',
  playing: '',
  paused: '',
};

/** Marker names offered for previous/next, most useful first. */
const PREFERRED_MARKERS = ['copilot-state', 'card', 'detector', 'live-timeline-update', 'transcript-update', 'copilot-request', 'phase', 'log'];

const iconButton = 'flex h-8 w-8 items-center justify-center rounded-full text-ink-muted hover:bg-surface-2 hover:text-ink disabled:opacity-40';

export function TransportBar({
  player,
  lanes,
  records,
  names,
  selected,
  onSelect,
}: {
  player: ReplayPlayer;
  lanes: Lanes;
  records: ReplayRecord[];
  names: string[];
  selected: number | null;
  onSelect: (i: number | null) => void;
}) {
  const state = useSyncExternalStore(player.subscribe, player.getState, player.getState);
  const timeRef = useRef<HTMLSpanElement>(null);
  const markers = PREFERRED_MARKERS.filter((name) => names.includes(name)).concat(names.filter((name) => !PREFERRED_MARKERS.includes(name)));
  const [marker, setMarker] = useState(markers[0] ?? 'transcript-update');

  // The time readout follows every frame without re-rendering React.
  useEffect(() => {
    const update = () => {
      const n = player.source.n;
      const record = n > 0 ? records[n - 1] : null;
      const meeting = state.axis === 'audio' ? player.cursorMs : record?.audio_ms ?? 0;
      const wall = state.axis === 'wall' ? player.cursorMs : record?.t_ms ?? 0;
      const text = `${clock(meeting)} meeting · ${clock(wall)} wall · ${n}/${records.length}`;
      if (timeRef.current && timeRef.current.textContent !== text) timeRef.current.textContent = text;
    };
    update();
    return player.onFrame(update);
  }, [player, records, state.axis]);

  const jump = (direction: number) => {
    const n = nextOfName(records, player.source.n, marker, direction);
    if (n !== null) {
      player.seekCount(n);
      onSelect(records[n - 1].i);
    }
  };

  // Space plays and pauses; arrows step between markers.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target?.closest('input, select, textarea, [contenteditable="true"], button')) return;
      if (event.key === ' ') {
        event.preventDefault();
        player.toggle();
      } else if (event.key === 'ArrowRight') jump(1);
      else if (event.key === 'ArrowLeft') jump(-1);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  const note = AUDIO_NOTE[state.audioReason];

  return (
    <section aria-label="Replay transport" className="shrink-0 border-t border-hairline bg-surface-1 px-4 pb-2 pt-2">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <button
          type="button"
          onClick={() => player.toggle()}
          aria-label={state.playing ? 'Pause' : 'Play'}
          title={state.playing ? 'Pause (Space)' : 'Play (Space)'}
          className="flex h-9 w-9 items-center justify-center rounded-full bg-brand text-brand-foreground hover:opacity-90"
        >
          {state.playing ? <Pause className="h-4 w-4 fill-current" /> : <Play className="h-4 w-4 fill-current" />}
        </button>
        <span ref={timeRef} className="min-w-[230px] font-mono text-xs tabular-nums text-ink" />
        <div className="flex h-8 items-center rounded-full bg-surface-2 p-0.5" role="group" aria-label="Speed">
          {SPEEDS.map((speed) => (
            <button
              key={speed}
              type="button"
              aria-pressed={state.speed === speed}
              onClick={() => player.setSpeed(speed)}
              className={`h-7 rounded-full px-2 text-[11px] tabular-nums ${state.speed === speed ? 'bg-surface-raised font-semibold text-ink shadow-sm' : 'text-ink-muted hover:text-ink'}`}
            >
              {speed}×
            </button>
          ))}
        </div>
        <SegmentedControl
          value={state.axis}
          aria-label="Timeline axis"
          className="h-8 rounded-full"
          options={[
            { value: 'audio', label: 'Meeting time' },
            { value: 'wall', label: 'Wall time' },
          ]}
          onValueChange={(axis) => player.setAxis(axis)}
        />
        <div className="flex items-center gap-1">
          <button type="button" className={iconButton} onClick={() => jump(-1)} aria-label="Previous marker" title="Previous marker (←)">
            <SkipBack className="h-4 w-4" />
          </button>
          <select
            value={marker}
            onChange={(event) => setMarker(event.target.value)}
            aria-label="Marker type"
            className="h-8 max-w-[180px] rounded-md border border-hairline bg-surface-0 px-2 text-xs text-ink"
          >
            {markers.map((name) => <option key={name} value={name}>{name}</option>)}
          </select>
          <button type="button" className={iconButton} onClick={() => jump(1)} aria-label="Next marker" title="Next marker (→)">
            <SkipForward className="h-4 w-4" />
          </button>
        </div>
        <div className="ml-auto flex items-center gap-2">
          {note && <span className="text-[11px] text-ink-subtle" aria-live="polite">{note}</span>}
          <button
            type="button"
            className={iconButton}
            disabled={!state.audioAvailable}
            onClick={() => player.setMuted(!state.muted)}
            aria-label={state.muted ? 'Unmute' : 'Mute'}
            title={state.muted ? 'Unmute' : 'Mute'}
          >
            {state.muted || state.volume === 0 ? <VolumeX className="h-4 w-4" /> : <Volume2 className="h-4 w-4" />}
          </button>
          <input
            type="range"
            min={0}
            max={1}
            step={0.05}
            value={state.muted ? 0 : state.volume}
            disabled={!state.audioAvailable}
            onChange={(event) => player.setVolume(Number(event.target.value))}
            aria-label="Volume"
            className="w-20 accent-[var(--ink)]"
          />
        </div>
      </div>
      <div className="mt-1">
        <ReplayTimeline
          player={player}
          lanes={lanes}
          records={records}
          axis={state.axis}
          selected={selected}
          onSelect={onSelect}
        />
      </div>
    </section>
  );
}
