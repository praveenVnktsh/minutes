/*
 * The replay transport's rules, pure so they are tested without a browser:
 * playback speeds, how the cursor advances each frame, and what the meeting
 * audio does at a given speed and axis (the same rules as site/replay).
 */

export const SPEEDS = [1, 2, 4, 16, 64, 256, 1024] as const;
export type Speed = (typeof SPEEDS)[number];
export const DEFAULT_SPEED: Speed = 16;

/** `audio` is meeting time (the recording's clock); `wall` is how long the
 * replay itself took. */
export type Axis = 'audio' | 'wall';

/** The recording plays at most this fast (pitch kept); faster is silent. */
export const AUDIO_MAX_RATE = 4;
/** Drift between the recording and the cursor that triggers a seek. */
export const AUDIO_RESYNC_MS = 250;
/** The longest frame step, so a stalled tab does not leap ahead. */
export const MAX_FRAME_MS = 100;

export type AudioReason = 'unavailable' | 'wall' | 'fast' | 'ended' | 'playing' | 'paused';

export interface AudioPlan {
  play: boolean;
  rate: number;
  reason: AudioReason;
}

/** What the recording should do (site/replay/parser.js `audioPlan`). */
export function audioPlan({
  available,
  axis,
  speed,
  playing,
  cursorMs,
  durationMs,
}: {
  available: boolean;
  axis: Axis;
  speed: number;
  playing: boolean;
  cursorMs: number;
  durationMs: number;
}): AudioPlan {
  if (!available) return { play: false, rate: 1, reason: 'unavailable' };
  if (axis !== 'audio') return { play: false, rate: 1, reason: 'wall' };
  if (speed > AUDIO_MAX_RATE) return { play: false, rate: 1, reason: 'fast' };
  if (durationMs > 0 && cursorMs >= durationMs) return { play: false, rate: speed, reason: 'ended' };
  return { play: playing, rate: speed, reason: playing ? 'playing' : 'paused' };
}

/** Whether the recording at `audioSeconds` is too far from the cursor. */
export function needsResync(audioSeconds: number, cursorMs: number, limitMs = AUDIO_RESYNC_MS): boolean {
  return Math.abs(audioSeconds * 1000 - cursorMs) > limitMs;
}

/** Whether the recording is the clock: playing at 1×, where following it
 * avoids any drift between what is heard and what is shown. */
export function audioIsClock(plan: AudioPlan, speed: number, audioPaused: boolean): boolean {
  return plan.play && speed === 1 && !audioPaused;
}

/**
 * The cursor after one frame. `dtMs` is the frame's wall time (capped), and at
 * 1× with the recording playing its position (`audioSeconds`) is the clock,
 * never moving the cursor backwards.
 */
export function nextCursor({
  cursorMs,
  dtMs,
  speed,
  endMs,
  audioSeconds,
}: {
  cursorMs: number;
  dtMs: number;
  speed: number;
  endMs: number;
  audioSeconds?: number | null;
}): { cursorMs: number; ended: boolean } {
  const step = Math.max(0, Math.min(dtMs, MAX_FRAME_MS));
  const next = audioSeconds != null ? Math.max(cursorMs, audioSeconds * 1000) : cursorMs + step * speed;
  if (next >= endMs) return { cursorMs: endMs, ended: true };
  return { cursorMs: next, ended: false };
}

/** The audio element's settings for a plan (volume and mute are the user's). */
export function audioRateFor(plan: AudioPlan): number {
  return Math.min(Math.max(plan.rate, 0.25), AUDIO_MAX_RATE);
}
