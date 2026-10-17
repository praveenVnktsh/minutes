'use client';

import { useEffect, useRef } from 'react';
import { useLiveSource } from '@/lib/liveSource';
import { usePageVisible } from '@/hooks/usePageVisible';

const BAR_COUNT = 24;
// ~12 updates/s: smooth enough for a level meter, and each update is an IPC
// round trip plus a repaint of the recording pill.
const POLL_MS = 83;

/** Maps a mixed-window RMS (speech sits around 0.02-0.2) to a 0-1 bar height. */
function toBarHeight(rms: number): number {
  return Math.max(0.08, Math.min(1, Math.sqrt(Math.max(0, rms)) * 2.2));
}

/**
 * A scrolling bar waveform of what is being recorded, drawn from the level the
 * audio pipeline publishes (`get_recording_level`). Frozen while paused or
 * while the window is hidden.
 */
export function RecordingWaveform({ active, className = '' }: { active: boolean; className?: string }) {
  const source = useLiveSource();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const levels = useRef<number[]>(new Array(BAR_COUNT).fill(0.08));
  const visible = usePageVisible();

  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext('2d');
    if (!canvas || !ctx) return;

    const draw = () => {
      const dpr = window.devicePixelRatio || 1;
      const width = canvas.clientWidth * dpr;
      const height = canvas.clientHeight * dpr;
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }
      ctx.clearRect(0, 0, width, height);
      ctx.fillStyle = getComputedStyle(canvas).color;
      const slot = width / BAR_COUNT;
      levels.current.forEach((level, i) => {
        const barHeight = Math.max(2 * dpr, level * height);
        ctx.beginPath();
        ctx.roundRect(i * slot + slot * 0.25, (height - barHeight) / 2, slot * 0.5, barHeight, slot * 0.25);
        ctx.fill();
      });
    };

    draw();
    if (!active || !visible) return;

    let cancelled = false;
    const timer = window.setInterval(async () => {
      let rms = 0;
      try {
        rms = await source.invoke<number>('get_recording_level');
      } catch {
        // No level available (e.g. between sessions); draw silence.
      }
      if (cancelled) return;
      const next = toBarHeight(rms);
      // Flat silence scrolls into identical bars: skip the repaint.
      if (levels.current.every((level) => level === next)) return;
      levels.current = [...levels.current.slice(1), next];
      draw();
    }, POLL_MS);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [active, visible, source]);

  return <canvas ref={canvasRef} aria-hidden="true" className={`text-recording ${className}`} />;
}
