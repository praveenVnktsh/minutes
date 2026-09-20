import { afterAll, afterEach, describe, expect, mock, test } from 'bun:test';
import { createElement } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { clampSeekTime, useAudioPlayer } from '../../src/hooks/useAudioPlayer';

describe('clampSeekTime', () => {
  test('passes a request through untouched while duration is not known yet', () => {
    // duration 0 means "haven't decoded the file yet", not "the file is empty" -
    // this is the exact case that used to collapse an early click to 0.
    expect(clampSeekTime(42, 0)).toBe(42);
  });

  test('clamps to duration once it is known', () => {
    expect(clampSeekTime(200, 120)).toBe(120);
  });

  test('allows landing exactly on the duration', () => {
    expect(clampSeekTime(120, 120)).toBe(120);
  });

  test('rejects negative and non-finite requests regardless of duration', () => {
    expect(clampSeekTime(-5, 120)).toBe(0);
    expect(clampSeekTime(-5, 0)).toBe(0);
    expect(clampSeekTime(Number.NaN, 120)).toBe(0);
    expect(clampSeekTime(Number.POSITIVE_INFINITY, 120)).toBe(0);
  });

  test('treats a non-finite duration the same as an unknown one', () => {
    expect(clampSeekTime(10, Number.NaN)).toBe(10);
    expect(clampSeekTime(10, Number.POSITIVE_INFINITY)).toBe(10);
  });
});

// --- useAudioPlayer: a seek before decode survives it -----------------------
//
// bun's test runner has no DOM, so the Web Audio pieces the hook reaches for
// (window.AudioContext, decodeAudioData, rAF) need stubs, and the Tauri
// `invoke` call needs mocking. Kept to the minimum this one race needs.

// Controlled by hand rather than auto-resolving: react-test-renderer's async
// `act` drains the microtask queue before returning, so an invoke() that
// resolves on its own would finish decoding inside the very first `act`
// call, and there would be no "before ready" window left to test.
let pendingRead: ((bytes: number[]) => void) | undefined;
const invoke = mock(() => new Promise<number[]>(resolve => {
  pendingRead = resolve;
}));
mock.module('@tauri-apps/api/core', () => ({ invoke }));

class FakeAudioContext {
  state = 'running';
  currentTime = 0;
  destination = {};
  async resume() {}
  close() {}
  createBufferSource() {
    return {
      buffer: null as unknown,
      onended: null as (() => void) | null,
      connect() {},
      start() {},
      stop() {},
      disconnect() {},
    };
  }
  decodeAudioData(_data: ArrayBuffer, success: (buffer: { duration: number }) => void) {
    // Real decoding is asynchronous; resolving on a later microtask is enough
    // to reproduce "the person clicked before decoding finished" without a
    // real codec.
    Promise.resolve().then(() => success({ duration: 120 }));
  }
}

const originalWindow = (globalThis as any).window;
const originalRaf = (globalThis as any).requestAnimationFrame;
const originalCaf = (globalThis as any).cancelAnimationFrame;

(globalThis as any).window = { AudioContext: FakeAudioContext };
(globalThis as any).requestAnimationFrame = () => 0;
(globalThis as any).cancelAnimationFrame = () => {};

afterAll(() => {
  (globalThis as any).window = originalWindow;
  (globalThis as any).requestAnimationFrame = originalRaf;
  (globalThis as any).cancelAnimationFrame = originalCaf;
});

let state: ReturnType<typeof useAudioPlayer>;
function View({ audioPath }: { audioPath: string | null }) {
  state = useAudioPlayer(audioPath);
  return createElement('output', null, JSON.stringify({
    currentTime: state.currentTime,
    duration: state.duration,
    isReady: state.isReady,
  }));
}

describe('useAudioPlayer seek-before-ready', () => {
  let renderer: ReactTestRenderer | undefined;

  afterEach(async () => {
    if (renderer) await act(async () => renderer!.unmount());
    renderer = undefined;
  });

  test('a seek issued before decoding finishes lands at the requested time, not 0', async () => {
    await act(async () => {
      renderer = create(createElement(View, { audioPath: 'meeting.wav' }));
    });
    expect(state.isReady).toBe(false);

    // Click a transcript line the instant the meeting opens, long before
    // decodeAudioData resolves.
    await act(async () => {
      await state.seek(42);
    });
    expect(state.currentTime).toBe(42);
    expect(state.isReady).toBe(false);

    // Now let the file read - and, in turn, decodeAudioData's queued
    // microtask - resolve.
    await act(async () => {
      pendingRead?.([1, 2, 3, 4]);
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(state.isReady).toBe(true);
    expect(state.duration).toBe(120);
    expect(state.currentTime).toBe(42);
  });
});
