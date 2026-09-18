"use client";

import { Pause, Play } from 'lucide-react';
import { useRef, useState, type PointerEvent } from 'react';

export function formatClock(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '00:00';
  const total = Math.floor(seconds);
  return `${Math.floor(total / 60).toString().padStart(2, '0')}:${(total % 60).toString().padStart(2, '0')}`;
}

export function timeFromClientX(
  clientX: number,
  rect: { left: number; width: number },
  duration: number,
): number {
  if (rect.width <= 0 || duration <= 0) return 0;
  const ratio = (clientX - rect.left) / rect.width;
  return Math.min(duration, Math.max(0, ratio * duration));
}

export interface AudioScrubberProps {
  currentTime: number;
  duration: number;
  isPlaying: boolean;
  disabled?: boolean;
  onTogglePlayback: () => void;
  onScrubPreview: (time: number) => void;
  onScrubCommit: (time: number) => void;
}

export function AudioScrubber({
  currentTime,
  duration,
  isPlaying,
  disabled = false,
  onTogglePlayback,
  onScrubPreview,
  onScrubCommit,
}: AudioScrubberProps) {
  const barRef = useRef<HTMLDivElement>(null);
  const draggingRef = useRef(false);
  const [dragging, setDragging] = useState(false);

  const percent = duration > 0 ? (Math.min(currentTime, duration) / duration) * 100 : 0;
  const canSeek = !disabled && duration > 0;

  const timeFromEvent = (event: PointerEvent<HTMLDivElement>) => {
    const rect = barRef.current?.getBoundingClientRect?.()
      ?? event.currentTarget.getBoundingClientRect?.()
      ?? { left: 0, width: 0 };
    return timeFromClientX(event.clientX, rect, duration);
  };

  const startDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (!canSeek) return;
    event.preventDefault();
    draggingRef.current = true;
    setDragging(true);
    event.currentTarget.setPointerCapture(event.pointerId);
    onScrubPreview(timeFromEvent(event));
  };

  const moveDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    onScrubPreview(timeFromEvent(event));
  };

  const endDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    draggingRef.current = false;
    setDragging(false);
    onScrubCommit(timeFromEvent(event));
  };

  const nudge = (delta: number) => {
    if (!canSeek) return;
    onScrubCommit(Math.min(duration, Math.max(0, currentTime + delta)));
  };

  return (
    <div className="mt-3 flex items-center gap-3">
      <button
        type="button"
        onClick={onTogglePlayback}
        disabled={disabled}
        className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-[var(--surface-2)] text-[var(--ink-muted)] hover:text-ink disabled:opacity-40"
        title={isPlaying ? 'Pause' : 'Play recording'}
        aria-label={isPlaying ? 'Pause' : 'Play recording'}
      >
        {isPlaying ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
      </button>
      <span className="shrink-0 text-xs tabular-nums text-[var(--ink-subtle)]">
        {formatClock(currentTime)}
      </span>
      <div
        ref={barRef}
        role="slider"
        aria-label="Seek recording"
        aria-valuemin={0}
        aria-valuemax={Math.max(0, Math.floor(duration))}
        aria-valuenow={Math.max(0, Math.floor(currentTime))}
        aria-valuetext={`${formatClock(currentTime)} of ${formatClock(duration)}`}
        aria-disabled={!canSeek}
        tabIndex={canSeek ? 0 : -1}
        className={`relative h-6 min-w-0 flex-1 touch-none ${canSeek ? 'cursor-pointer' : 'cursor-default opacity-50'}`}
        onPointerDown={startDrag}
        onPointerMove={moveDrag}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onKeyDown={(event) => {
          if (!canSeek) return;
          const step = event.shiftKey ? 5 : 1;
          if (event.key === 'ArrowRight' || event.key === 'ArrowUp') {
            event.preventDefault();
            nudge(step);
          } else if (event.key === 'ArrowLeft' || event.key === 'ArrowDown') {
            event.preventDefault();
            nudge(-step);
          } else if (event.key === 'Home') {
            event.preventDefault();
            onScrubCommit(0);
          } else if (event.key === 'End') {
            event.preventDefault();
            onScrubCommit(duration);
          }
        }}
      >
        <div className="absolute inset-y-[11px] left-0 right-0 rounded-full bg-[var(--surface-2)]">
          <div
            className="h-full rounded-full bg-[var(--ink-muted)]"
            style={{ width: `${percent}%` }}
          />
        </div>
        <div
          className={`absolute top-1/2 h-3 w-3 -translate-x-1/2 -translate-y-1/2 rounded-full bg-ink ${dragging ? 'scale-110' : ''}`}
          style={{ left: `${percent}%` }}
        />
      </div>
      <span className="shrink-0 text-right text-xs tabular-nums text-[var(--ink-subtle)]">
        {formatClock(duration)}
      </span>
    </div>
  );
}
