/**
 * Coalesces bursts of work into at most one flush per `intervalMs`.
 *
 * Callers record what changed somewhere (a ref, a pending value) and call
 * `schedule()`; the flush runs once, trailing, no sooner than `intervalMs`
 * after the previous one. Used to keep high-rate native events (streaming
 * partials, levels) from driving a React render per event.
 */
export interface ThrottledFlush {
  /** Ask for a flush; repeated calls before it runs are merged. */
  schedule(): void;
  /** Drop a pending flush (e.g. on unmount). */
  cancel(): void;
}

export interface ThrottledFlushTimers {
  now: () => number;
  setTimeout: (callback: () => void, ms: number) => unknown;
  clearTimeout: (handle: unknown) => void;
}

const defaultTimers: ThrottledFlushTimers = {
  now: () => Date.now(),
  setTimeout: (callback, ms) => globalThis.setTimeout(callback, ms),
  clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export function createThrottledFlush(
  flush: () => void,
  intervalMs: number,
  timers: ThrottledFlushTimers = defaultTimers,
): ThrottledFlush {
  let pending: unknown = null;
  let lastFlushAt = Number.NEGATIVE_INFINITY;

  const run = () => {
    pending = null;
    lastFlushAt = timers.now();
    flush();
  };

  return {
    schedule() {
      if (pending !== null) return;
      const wait = Math.max(0, intervalMs - (timers.now() - lastFlushAt));
      pending = timers.setTimeout(run, wait);
    },
    cancel() {
      if (pending !== null) timers.clearTimeout(pending);
      pending = null;
    },
  };
}
