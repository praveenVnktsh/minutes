'use client';

/*
 * The run's multi-lane timeline (a port of site/replay's canvas). The lanes
 * are drawn once into an offscreen layer per view (zoom, size, axis, theme,
 * selection); each frame only copies it, dims what has not happened yet and
 * draws the cursor. Click to seek (an item seeks to it), drag to pan, wheel
 * to zoom, double-click to fit.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { clock, type LaneItem, type Lanes, type ReplayRecord, type SeriesPoint } from '@/lib/replay/reducer';
import type { ReplayPlayer } from '@/lib/replay/player';
import type { Axis } from '@/lib/replay/transport';

const LABEL_W = 116;
const LANE_H = 16;
const LIVE_H = 36;
const AXIS_H = 16;

interface Palette {
  ink: string;
  subtle: string;
  hair: string;
  muted: string;
  info: string;
  success: string;
  warning: string;
  error: string;
  surface0: string;
  surface1: string;
  recording: string;
}

function readPalette(element: Element): Palette {
  const style = getComputedStyle(element);
  const v = (name: string) => style.getPropertyValue(name).trim();
  return {
    ink: v('--ink'), subtle: v('--ink-subtle'), hair: v('--hairline'), muted: v('--ink-muted'),
    info: v('--info'), success: v('--success'), warning: v('--warning'), error: v('--error'),
    surface0: v('--surface-0'), surface1: v('--surface-1'), recording: v('--recording'),
  };
}

function toneColor(p: Palette, tone: string): string {
  return ({ info: p.info, success: p.success, warning: p.warning, error: p.error, neutral: p.muted, subtle: p.subtle } as Record<string, string>)[tone] || p.muted;
}

interface Hit {
  item?: LaneItem;
  series?: boolean;
  x0: number;
  x1: number;
  y0: number;
  y1: number;
}

export function ReplayTimeline({
  player,
  lanes,
  records,
  axis,
  selected,
  onSelect,
}: {
  player: ReplayPlayer;
  lanes: Lanes;
  records: ReplayRecord[];
  axis: Axis;
  selected: number | null;
  onSelect: (i: number) => void;
}) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const layerRef = useRef<HTMLCanvasElement | null>(null);
  const layoutRef = useRef<Hit[]>([]);
  const layerKeyRef = useRef('');
  const paletteRef = useRef<Palette | null>(null);
  const viewRef = useRef({ v0: 0, v1: 1 });
  const dragRef = useRef<{ x: number; v0: number; v1: number; moved: boolean } | null>(null);
  const [tip, setTip] = useState<{ x: number; y: number; text: string } | null>(null);
  const rangeRef = useRef<HTMLSpanElement>(null);

  const height = lanes.order.reduce((h, [id]) => h + (id === 'live' ? LIVE_H : LANE_H), 0) + 4 + AXIS_H;

  const fit = useCallback(() => {
    const end = Math.max(1000, player.endOf(axis));
    viewRef.current = { v0: -end * 0.01, v1: end * 1.01 };
  }, [player, axis]);

  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    const wrap = wrapRef.current;
    if (!canvas || !wrap) return;
    const width = wrap.clientWidth;
    if (!width) return;
    const dpr = window.devicePixelRatio || 1;
    if (canvas.width !== Math.round(width * dpr) || canvas.height !== Math.round(height * dpr)) {
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
    }
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    if (!paletteRef.current) paletteRef.current = readPalette(canvas);
    const p = paletteRef.current;
    const { v0, v1 } = viewRef.current;
    const xOf = (ms: number) => LABEL_W + ((ms - v0) / (v1 - v0)) * (width - LABEL_W);
    const theme = document.documentElement.className;
    const key = [width, height, dpr, v0, v1, axis, theme, selected ?? -1].join('|');
    if (key !== layerKeyRef.current) {
      layerKeyRef.current = key;
      const layer = layerRef.current ?? document.createElement('canvas');
      layerRef.current = layer;
      layer.width = Math.round(width * dpr);
      layer.height = Math.round(height * dpr);
      const c = layer.getContext('2d');
      if (!c) return;
      c.setTransform(dpr, 0, 0, dpr, 0, 0);
      c.clearRect(0, 0, width, height);
      c.font = '10px "Source Sans 3", sans-serif';
      const layout: Hit[] = [];
      const span = v1 - v0;
      const steps = [1000, 5000, 10000, 30000, 60000, 120000, 300000, 600000, 1800000];
      const step = steps.find((s) => span / s < 12) || 3600000;
      c.fillStyle = p.subtle;
      c.strokeStyle = p.hair;
      for (let t = Math.ceil(v0 / step) * step; t < v1; t += step) {
        const x = xOf(t);
        c.beginPath();
        c.moveTo(x, 0);
        c.lineTo(x, height - AXIS_H);
        c.stroke();
        c.fillText(clock(t), x + 2, height - 4);
      }
      let y = 2;
      for (const [id] of lanes.order) {
        const h = id === 'live' ? LIVE_H : LANE_H;
        c.strokeStyle = p.hair;
        c.beginPath();
        c.moveTo(0, y + h);
        c.lineTo(width, y + h);
        c.stroke();
        if (id === 'live') {
          drawSeries(c, lanes.series, axis, y, h, xOf, p, width);
          layout.push({ series: true, x0: LABEL_W, x1: width, y0: y, y1: y + h });
        } else {
          let lastPx = -1;
          let lastTone = '';
          for (const item of lanes.lanes[id] || []) {
            const [a, b] = item[axis];
            if (b < v0 || a > v1) continue;
            const x0 = xOf(a);
            const x1 = Math.max(xOf(b), x0 + (item.bar ? 2 : 1.5));
            const px = Math.round(x0);
            const isSelected = selected === item.i;
            // Items narrower than a pixel that share one are drawn (and hit) once.
            if (!isSelected && !item.bar && !item.first && px === lastPx && item.tone === lastTone) continue;
            lastPx = px;
            lastTone = item.tone;
            c.fillStyle = toneColor(p, item.tone);
            const top = item.big ? y + 1 : y + 3;
            const hh = item.big ? h - 2 : h - 6;
            c.fillRect(x0, top, x1 - x0, hh);
            if (item.first) {
              c.fillStyle = p.ink;
              c.fillRect(xOf(item.first[axis]) - 0.5, y + 2, 1.5, h - 4);
            }
            if (item.differs) {
              c.fillStyle = toneColor(p, 'error');
              c.fillText('≠', x1 + 2, y + h - 4);
            }
            if (isSelected) {
              c.strokeStyle = p.ink;
              c.lineWidth = 1.5;
              c.strokeRect(x0 - 1, top - 1, x1 - x0 + 2, hh + 2);
              c.lineWidth = 1;
            }
            layout.push({ item, x0: x0 - 2, x1: x1 + 2, y0: y, y1: y + h });
          }
        }
        y += h;
      }
      layoutRef.current = layout;
    }
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    if (layerRef.current) ctx.drawImage(layerRef.current, 0, 0);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    // What has not happened yet is dimmed.
    const cx = xOf(player.cursorMs);
    if (cx < width) {
      ctx.globalAlpha = 0.55;
      ctx.fillStyle = p.surface0;
      ctx.fillRect(Math.max(cx, LABEL_W), 0, width - Math.max(cx, LABEL_W), height - AXIS_H);
      ctx.globalAlpha = 1;
    }
    if (cx >= LABEL_W) {
      ctx.strokeStyle = p.recording;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(cx, 0);
      ctx.lineTo(cx, height - AXIS_H);
      ctx.stroke();
      ctx.lineWidth = 1;
    }
    const range = `${axis === 'audio' ? 'Meeting time' : 'Wall time'} ${clock(Math.max(0, v0))}–${clock(v1)}`;
    if (rangeRef.current && rangeRef.current.textContent !== range) rangeRef.current.textContent = range;
    // Lane names over everything.
    ctx.fillStyle = p.surface1;
    ctx.fillRect(0, 0, LABEL_W - 4, height - AXIS_H);
    ctx.font = '10px "Source Sans 3", sans-serif';
    ctx.fillStyle = p.subtle;
    let y = 2;
    for (const [id, name] of lanes.order) {
      const h = id === 'live' ? LIVE_H : LANE_H;
      ctx.fillText(name, 8, y + h / 2 + 3);
      y += h;
    }
  }, [axis, height, lanes, player, selected]);

  useEffect(() => {
    fit();
    layerKeyRef.current = '';
    draw();
  }, [fit, draw]);

  useEffect(() => player.onFrame(draw), [player, draw]);

  useEffect(() => {
    const redraw = () => {
      paletteRef.current = null;
      layerKeyRef.current = '';
      draw();
    };
    window.addEventListener('resize', draw);
    const observer = new MutationObserver(redraw);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class', 'data-theme'] });
    return () => {
      window.removeEventListener('resize', draw);
      observer.disconnect();
    };
  }, [draw]);

  const msOf = (x: number, width: number) => {
    const { v0, v1 } = viewRef.current;
    return v0 + ((x - LABEL_W) / (width - LABEL_W)) * (v1 - v0);
  };

  const hit = (x: number, y: number) => {
    let best: { l: Hit; d: number } | null = null;
    for (const l of layoutRef.current) {
      if (l.series || y < l.y0 || y > l.y1 || x < l.x0 || x > l.x1) continue;
      const d = Math.abs((l.x0 + l.x1) / 2 - x);
      if (!best || d < best.d) best = { l, d };
    }
    return best?.l ?? null;
  };

  const seriesAt = (ms: number): SeriesPoint | null => {
    let best: SeriesPoint | null = null;
    for (const s of lanes.series) {
      if (s[axis] <= ms) best = s;
      else break;
    }
    return best;
  };

  const offset = (event: React.PointerEvent | React.WheelEvent) => {
    const rect = canvasRef.current!.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top, width: rect.width };
  };

  // Wheel zoom needs a non-passive listener to prevent the page scrolling.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return undefined;
    const wheel = (event: WheelEvent) => {
      if (event.shiftKey || Math.abs(event.deltaX) > Math.abs(event.deltaY)) return;
      event.preventDefault();
      const rect = canvas.getBoundingClientRect();
      const at = msOf(event.clientX - rect.left, rect.width);
      const factor = Math.exp(event.deltaY * 0.0015);
      const { v0, v1 } = viewRef.current;
      const n0 = at - (at - v0) * factor;
      const n1 = at + (v1 - at) * factor;
      if (n1 - n0 < 200) return;
      viewRef.current = { v0: n0, v1: n1 };
      draw();
    };
    canvas.addEventListener('wheel', wheel, { passive: false });
    return () => canvas.removeEventListener('wheel', wheel);
    // msOf reads the ref; draw changes with the view.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draw, player]);

  return (
    <div className="min-w-0">
      <div className="flex items-center justify-between px-1 pb-1 text-[10px] text-ink-subtle">
        <span ref={rangeRef} />
        <button
          type="button"
          onClick={() => { fit(); draw(); }}
          className="rounded px-1.5 py-0.5 hover:bg-surface-2 hover:text-ink"
        >
          Fit
        </button>
      </div>
      <div ref={wrapRef} className="relative w-full">
        <canvas
          ref={canvasRef}
          aria-label="Replay timeline"
          className="block w-full cursor-crosshair touch-none text-ink"
          style={{ height }}
          onPointerDown={(event) => {
            const { x } = offset(event);
            dragRef.current = { x, ...viewRef.current, moved: false };
            event.currentTarget.setPointerCapture(event.pointerId);
          }}
          onPointerMove={(event) => {
            const { x, y, width } = offset(event);
            const drag = dragRef.current;
            if (drag) {
              const dx = x - drag.x;
              if (Math.abs(dx) > 3) drag.moved = true;
              if (drag.moved) {
                const per = (drag.v1 - drag.v0) / (width - LABEL_W);
                viewRef.current = { v0: drag.v0 - dx * per, v1: drag.v1 - dx * per };
                draw();
              }
              return;
            }
            const found = hit(x, y);
            let text: string | null = null;
            if (found?.item) {
              const r = records[found.item.i];
              text = `${found.item.label}\n${clock(r.audio_ms)} meeting · ${clock(r.t_ms)} wall`;
            } else if (x > LABEL_W && layoutRef.current.some((l) => l.series && y >= l.y0 && y <= l.y1)) {
              const s = seriesAt(msOf(x, width));
              if (s) text = `backlog ${s.backlog.toFixed(1)} s · RTF ${s.rtf == null ? '–' : s.rtf.toFixed(2)}${s.llm ? ' · model busy' : ''}`;
            }
            setTip(text ? { x: Math.min(x + 12, width - 280), y: y + 14, text } : null);
          }}
          onPointerLeave={() => setTip(null)}
          onPointerUp={(event) => {
            const drag = dragRef.current;
            dragRef.current = null;
            if (!drag || drag.moved || records.length === 0) return;
            const { x, y, width } = offset(event);
            const found = hit(x, y);
            if (found?.item) {
              player.seekCount(found.item.i + 1);
              onSelect(found.item.i);
            } else if (x > LABEL_W) {
              player.seekMs(msOf(x, width));
            }
          }}
          onDoubleClick={() => { fit(); draw(); }}
        />
        {tip && (
          <div
            role="tooltip"
            className="pointer-events-none absolute z-10 max-w-[280px] whitespace-pre-wrap rounded-md border border-hairline bg-surface-raised px-2 py-1 text-[11px] leading-4 text-ink shadow-sm"
            style={{ left: tip.x, top: tip.y }}
          >
            {tip.text}
          </div>
        )}
      </div>
    </div>
  );
}

function drawSeries(
  c: CanvasRenderingContext2D,
  series: SeriesPoint[],
  axis: Axis,
  y: number,
  h: number,
  xOf: (ms: number) => number,
  p: Palette,
  width: number,
) {
  if (!series.length) return;
  let maxBacklog = 2;
  for (const s of series) if (s.backlog > maxBacklog) maxBacklog = s.backlog;
  // Model work in flight.
  c.fillStyle = toneColor(p, 'warning');
  c.globalAlpha = 0.12;
  for (let k = 1; k < series.length; k += 1) {
    if (!series[k].llm) continue;
    const x0 = xOf(series[k - 1][axis]);
    c.fillRect(x0, y + 1, Math.max(1, xOf(series[k][axis]) - x0), h - 2);
  }
  c.globalAlpha = 1;
  const line = (value: (s: SeriesPoint) => number | null, max: number, color: string) => {
    c.strokeStyle = color;
    c.beginPath();
    let started = false;
    for (const s of series) {
      const v = value(s);
      if (v == null) {
        started = false;
        continue;
      }
      const x = xOf(s[axis]);
      const yy = y + h - 3 - Math.min(1, v / max) * (h - 6);
      if (!started) c.moveTo(x, yy);
      else c.lineTo(x, yy);
      started = true;
    }
    c.stroke();
  };
  // RTF limit (0.9 of a 1.5 scale).
  c.strokeStyle = toneColor(p, 'error');
  c.setLineDash([3, 3]);
  const limitY = y + h - 3 - (0.9 / 1.5) * (h - 6);
  c.beginPath();
  c.moveTo(LABEL_W, limitY);
  c.lineTo(width, limitY);
  c.stroke();
  c.setLineDash([]);
  line((s) => s.rtf, 1.5, toneColor(p, 'info'));
  line((s) => s.backlog, maxBacklog, toneColor(p, 'warning'));
}
