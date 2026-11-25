/*
 * The replay's clock: the cursor on either axis, play/pause, speed, and the
 * meeting audio kept in step with it. Drives a ReplaySource (which drives the
 * components). React reads it through `subscribe` (coarse changes) and
 * `onFrame` (every animation frame, for the canvas and the time readout).
 */

import { countAtAudio, countAtTime, type ReplayRecord } from './reducer';
import type { ReplaySource } from './replaySource';
import {
  DEFAULT_SPEED,
  audioIsClock,
  audioPlan,
  audioRateFor,
  needsResync,
  nextCursor,
  type AudioReason,
  type Axis,
} from './transport';

/** While playing above 4×, the components are updated at most this often. */
const PUBLISH_EVERY_MS = 100;

export interface PlayerState {
  playing: boolean;
  speed: number;
  axis: Axis;
  audioReason: AudioReason;
  audioAvailable: boolean;
  volume: number;
  muted: boolean;
  fps: number;
}

const VOLUME_KEY = 'meetily:replay-volume';
const MUTED_KEY = 'meetily:replay-muted';

function stored(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Remembered for this window only.
  }
}

export class ReplayPlayer {
  cursorMs = 0;
  private state: PlayerState;
  private readonly subscribers = new Set<() => void>();
  private readonly frameListeners = new Set<() => void>();
  private raf = 0;
  private lastFrame = 0;
  private lastPublish = 0;
  private fps = { frames: 0, since: 0 };
  private audio: HTMLAudioElement | null = null;
  private audioDurationMs = 0;
  private disposed = false;

  constructor(readonly source: ReplaySource) {
    const volume = Number(stored(VOLUME_KEY));
    this.state = {
      playing: false,
      speed: DEFAULT_SPEED,
      axis: 'audio',
      audioReason: 'unavailable',
      audioAvailable: false,
      volume: Number.isFinite(volume) && stored(VOLUME_KEY) !== null ? Math.min(1, Math.max(0, volume)) : 1,
      muted: stored(MUTED_KEY) === '1',
      fps: 0,
    };
  }

  get records(): ReplayRecord[] {
    return this.source.records;
  }

  // -- React ------------------------------------------------------------------

  subscribe = (fn: () => void) => {
    this.subscribers.add(fn);
    return () => {
      this.subscribers.delete(fn);
    };
  };

  getState = (): PlayerState => this.state;

  onFrame(fn: () => void): () => void {
    this.frameListeners.add(fn);
    return () => {
      this.frameListeners.delete(fn);
    };
  }

  private set(patch: Partial<PlayerState>) {
    const next = { ...this.state, ...patch };
    const changed = (Object.keys(patch) as Array<keyof PlayerState>).some((k) => next[k] !== this.state[k]);
    if (!changed) return;
    this.state = next;
    for (const fn of [...this.subscribers]) fn();
  }

  private frame() {
    for (const fn of [...this.frameListeners]) fn();
  }

  // -- Axis and cursor ----------------------------------------------------------

  axisOf(record: ReplayRecord, axis: Axis = this.state.axis): number {
    return axis === 'audio' ? record.audio_ms : record.t_ms;
  }

  endOf(axis: Axis = this.state.axis): number {
    const last = this.records[this.records.length - 1];
    return last ? this.axisOf(last, axis) : 0;
  }

  private countAt(ms: number): number {
    return this.state.axis === 'audio' ? countAtAudio(this.records, ms) : countAtTime(this.records, ms);
  }

  /** Moves the cursor to `ms` on the current axis. */
  seekMs(ms: number): void {
    this.cursorMs = Math.max(0, Math.min(ms, this.endOf()));
    this.source.seek(this.countAt(this.cursorMs));
    this.syncAudio(true);
    this.frame();
  }

  /** Moves the cursor to just after record `n - 1`. */
  seekCount(n: number): void {
    const clamped = Math.max(0, Math.min(n, this.records.length));
    this.source.seek(clamped);
    this.cursorMs = clamped > 0 ? this.axisOf(this.records[clamped - 1]) : 0;
    this.syncAudio(true);
    this.frame();
  }

  /** Moves the cursor to meeting time `ms` (a [MM:SS] citation). */
  seekMeetingMs(ms: number): void {
    const n = countAtAudio(this.records, ms);
    if (this.state.axis === 'audio') this.seekMs(ms);
    else this.seekCount(n);
  }

  setAxis(axis: Axis): void {
    if (axis === this.state.axis) return;
    this.set({ axis });
    const n = this.source.n;
    this.cursorMs = n > 0 ? this.axisOf(this.records[n - 1]) : 0;
    this.syncAudio(true);
    this.frame();
  }

  setSpeed(speed: number): void {
    this.set({ speed });
    this.syncAudio(true);
  }

  // -- Playing ------------------------------------------------------------------

  setPlaying(playing: boolean): void {
    const on = playing && this.records.length > 0;
    if (on && this.cursorMs >= this.endOf()) this.seekMs(0);
    this.set({ playing: on });
    cancelAnimationFrame(this.raf);
    if (on) {
      this.lastFrame = 0;
      this.fps = { frames: 0, since: performance.now() };
      this.raf = requestAnimationFrame(this.tick);
    } else {
      // Settle the components on the paused moment.
      this.source.advance(this.countAt(this.cursorMs));
    }
    this.syncAudio(true);
  }

  toggle(): void {
    this.setPlaying(!this.state.playing);
  }

  private tick = (now: number) => {
    if (!this.state.playing || this.disposed) return;
    const dt = this.lastFrame ? now - this.lastFrame : 0;
    this.lastFrame = now;
    this.fps.frames += 1;
    if (now - this.fps.since >= 1000) {
      this.set({ fps: (this.fps.frames * 1000) / (now - this.fps.since || 1) });
      this.fps = { frames: 0, since: now };
    }
    const plan = this.plan();
    const audio = this.audio;
    const clocked = audio && audioIsClock(plan, audio);
    const step = nextCursor({
      cursorMs: this.cursorMs,
      dtMs: dt,
      speed: this.state.speed,
      endMs: this.endOf(),
      audioSeconds: clocked ? audio.currentTime : null,
    });
    this.cursorMs = step.cursorMs;
    const n = this.countAt(this.cursorMs);
    const throttled = this.state.speed > 4 && now - this.lastPublish < PUBLISH_EVERY_MS;
    if (n !== this.source.n && (!throttled || step.ended)) {
      this.lastPublish = now;
      this.source.advance(n);
    }
    this.syncAudio(false);
    this.frame();
    if (step.ended) {
      this.setPlaying(false);
      return;
    }
    this.raf = requestAnimationFrame(this.tick);
  };

  // -- Audio --------------------------------------------------------------------

  /** Attaches the run's recording (null when the run has none). */
  attachAudio(audio: HTMLAudioElement | null): void {
    this.audio = audio;
    this.audioDurationMs = 0;
    if (audio) {
      audio.preservesPitch = true;
      audio.volume = this.state.volume;
      audio.muted = this.state.muted;
      const ready = () => {
        this.audioDurationMs = Number.isFinite(audio.duration) ? audio.duration * 1000 : 0;
        this.set({ audioAvailable: true });
        this.syncAudio(true);
      };
      if (audio.readyState >= 1) ready();
      else audio.addEventListener('loadedmetadata', ready, { once: true });
      audio.addEventListener('error', () => this.set({ audioAvailable: false }), { once: true });
    } else {
      this.set({ audioAvailable: false });
    }
    this.syncAudio(true);
  }

  setVolume(volume: number): void {
    const value = Math.min(1, Math.max(0, volume));
    store(VOLUME_KEY, String(value));
    this.set({ volume: value, muted: value === 0 ? this.state.muted : false });
    if (this.audio) {
      this.audio.volume = value;
      if (value > 0) this.audio.muted = false;
    }
    store(MUTED_KEY, this.state.muted ? '1' : '0');
  }

  setMuted(muted: boolean): void {
    store(MUTED_KEY, muted ? '1' : '0');
    this.set({ muted });
    if (this.audio) this.audio.muted = muted;
  }

  private meetingCursorMs(): number {
    if (this.state.axis === 'audio') return this.cursorMs;
    const n = this.source.n;
    return n > 0 ? this.records[n - 1].audio_ms : 0;
  }

  private plan() {
    return audioPlan({
      available: Boolean(this.audio) && this.state.audioAvailable,
      axis: this.state.axis,
      speed: this.state.speed,
      playing: this.state.playing,
      cursorMs: this.meetingCursorMs(),
      durationMs: this.audioDurationMs,
    });
  }

  /** Keeps the recording in step with the cursor. `force` also resyncs a
   * paused recording (after a seek). */
  private syncAudio(force: boolean): void {
    const audio = this.audio;
    const plan = this.plan();
    if (plan.reason !== this.state.audioReason) this.set({ audioReason: plan.reason });
    if (!audio || !this.state.audioAvailable) return;
    const cursor = this.meetingCursorMs();
    if (!plan.play) {
      if (!audio.paused) audio.pause();
      if (force && plan.reason === 'paused' && needsResync(audio.currentTime, cursor)) audio.currentTime = cursor / 1000;
      return;
    }
    const rate = audioRateFor(plan);
    if (audio.playbackRate !== rate) audio.playbackRate = rate;
    // While it plays the recording is the clock; otherwise follow the cursor.
    const clocked = audioIsClock(plan, audio);
    if ((force || !clocked) && needsResync(audio.currentTime, cursor)) audio.currentTime = cursor / 1000;
    if (audio.paused) void audio.play().catch(() => this.set({ audioReason: 'unavailable' }));
  }

  dispose(): void {
    this.disposed = true;
    cancelAnimationFrame(this.raf);
    if (this.audio) {
      this.audio.pause();
      this.audio.removeAttribute('src');
      this.audio.load();
    }
    this.audio = null;
    this.subscribers.clear();
    this.frameListeners.clear();
  }
}
