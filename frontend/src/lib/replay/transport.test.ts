import { describe, expect, test } from 'bun:test';
import { AUDIO_MAX_RATE, DEFAULT_SPEED, SPEEDS, audioIsClock, audioPlan, needsResync, nextCursor } from './transport';

const plan = (over: Partial<Parameters<typeof audioPlan>[0]> = {}) =>
  audioPlan({ available: true, axis: 'audio', speed: 1, playing: true, cursorMs: 1000, durationMs: 60_000, ...over });

describe('replay transport', () => {
  test('speeds and the default', () => {
    expect([...SPEEDS]).toEqual([1, 2, 4, 16, 64, 256, 1024]);
    expect(DEFAULT_SPEED).toBe(16);
  });

  test('the recording plays up to 4× on the meeting axis and is silent otherwise', () => {
    expect(plan()).toEqual({ play: true, rate: 1, reason: 'playing' });
    expect(plan({ speed: 4 })).toEqual({ play: true, rate: 4, reason: 'playing' });
    expect(plan({ speed: AUDIO_MAX_RATE * 4 })).toEqual({ play: false, rate: 1, reason: 'fast' });
    expect(plan({ axis: 'wall' }).reason).toBe('wall');
    expect(plan({ available: false }).reason).toBe('unavailable');
    expect(plan({ playing: false })).toEqual({ play: false, rate: 1, reason: 'paused' });
    expect(plan({ cursorMs: 60_000 }).reason).toBe('ended');
  });

  test('drift over 250 ms resyncs', () => {
    expect(needsResync(10, 10_200)).toBe(false);
    expect(needsResync(10, 10_251)).toBe(true);
    expect(needsResync(10.3, 10_000)).toBe(true);
  });

  test('at 1× the recording is the clock, never moving the cursor back', () => {
    expect(audioIsClock(plan(), 1, false)).toBe(true);
    expect(audioIsClock(plan({ speed: 2 }), 2, false)).toBe(false);
    expect(audioIsClock(plan(), 1, true)).toBe(false);
    expect(nextCursor({ cursorMs: 5000, dtMs: 16, speed: 1, endMs: 60_000, audioSeconds: 5.5 })).toEqual({ cursorMs: 5500, ended: false });
    expect(nextCursor({ cursorMs: 5000, dtMs: 16, speed: 1, endMs: 60_000, audioSeconds: 4.9 })).toEqual({ cursorMs: 5000, ended: false });
  });

  test('otherwise the cursor advances by the frame times the speed, capped', () => {
    expect(nextCursor({ cursorMs: 0, dtMs: 16, speed: 16, endMs: 60_000 })).toEqual({ cursorMs: 256, ended: false });
    expect(nextCursor({ cursorMs: 0, dtMs: 5000, speed: 2, endMs: 60_000 })).toEqual({ cursorMs: 200, ended: false });
    expect(nextCursor({ cursorMs: 59_990, dtMs: 16, speed: 1024, endMs: 60_000 })).toEqual({ cursorMs: 60_000, ended: true });
  });
});
