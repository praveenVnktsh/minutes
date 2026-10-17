import { describe, expect, test } from 'bun:test';
import { createThrottledFlush, type ThrottledFlushTimers } from '../../src/lib/throttledFlush';
import { isPageVisible } from '../../src/hooks/usePageVisible';

function fakeTimers() {
  let now = 0;
  let nextId = 1;
  const queue = new Map<number, { at: number; callback: () => void }>();
  const timers: ThrottledFlushTimers = {
    now: () => now,
    setTimeout: (callback, ms) => {
      const id = nextId++;
      queue.set(id, { at: now + ms, callback });
      return id;
    },
    clearTimeout: (handle) => {
      queue.delete(handle as number);
    },
  };
  const advance = (ms: number) => {
    const target = now + ms;
    for (;;) {
      const due = [...queue.entries()]
        .filter(([, timer]) => timer.at <= target)
        .sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      queue.delete(due[0]);
      now = due[1].at;
      due[1].callback();
    }
    now = target;
  };
  return { timers, advance, pending: () => queue.size };
}

describe('createThrottledFlush', () => {
  test('merges a burst into one flush', () => {
    const { timers, advance } = fakeTimers();
    let flushes = 0;
    const flush = createThrottledFlush(() => { flushes += 1; }, 100, timers);
    for (let i = 0; i < 20; i += 1) flush.schedule();
    advance(0);
    expect(flushes).toBe(1);
  });

  test('flushes at most once per interval under a steady stream', () => {
    const { timers, advance } = fakeTimers();
    let flushes = 0;
    const flush = createThrottledFlush(() => { flushes += 1; }, 100, timers);
    // 60 events a second for one second.
    for (let i = 0; i < 60; i += 1) {
      flush.schedule();
      advance(1000 / 60);
    }
    advance(100);
    expect(flushes).toBeGreaterThanOrEqual(10);
    expect(flushes).toBeLessThanOrEqual(11);
  });

  test('a flush always follows the last event', () => {
    const { timers, advance } = fakeTimers();
    const seen: number[] = [];
    let latest = 0;
    const flush = createThrottledFlush(() => seen.push(latest), 100, timers);
    latest = 1;
    flush.schedule();
    advance(10);
    latest = 2;
    flush.schedule();
    advance(200);
    expect(seen.at(-1)).toBe(2);
  });

  test('cancel drops the pending flush', () => {
    const { timers, advance, pending } = fakeTimers();
    let flushes = 0;
    const flush = createThrottledFlush(() => { flushes += 1; }, 100, timers);
    flush.schedule();
    advance(0);
    flush.schedule();
    flush.cancel();
    expect(pending()).toBe(0);
    advance(500);
    expect(flushes).toBe(1);
  });
});

describe('isPageVisible', () => {
  test('is false only for a hidden document', () => {
    expect(isPageVisible({ visibilityState: 'hidden' })).toBe(false);
    expect(isPageVisible({ visibilityState: 'visible' })).toBe(true);
    expect(isPageVisible(undefined)).toBe(true);
  });
});
