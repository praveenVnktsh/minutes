import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const originalEvent = { ...await import('@tauri-apps/api/event') };
const originalToast = { ...await import('sonner') };

afterAll(() => {
  mock.module('@tauri-apps/api/event', () => originalEvent);
  mock.module('sonner', () => originalToast);
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

let registration = deferred<() => void>();
let downloadCallback: ((event: { payload: { modelName: string } }) => void) | null = null;
const listen = mock((_event: string, callback: typeof downloadCallback) => {
  downloadCallback = callback;
  return registration.promise;
});
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen }));
mock.module('sonner', () => ({ toast: { success: mock(() => {}) } }));

const { useModalState } = await import('../../src/hooks/useModalState');
let renderer: ReactTestRenderer | undefined;

function Probe() {
  useModalState({ provider: 'localWhisper', model: 'base', apiKey: null });
  return null;
}

beforeEach(() => {
  registration = deferred();
  downloadCallback = null;
  listen.mockClear();
});

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

describe('useModalState listener lifecycle', () => {
  test('unlistens when registration resolves after unmount', async () => {
    const unlisten = mock(() => {});
    await act(async () => { renderer = create(<Probe />); });
    await act(async () => renderer!.unmount());
    renderer = undefined;
    await act(async () => { registration.resolve(unlisten); await registration.promise; });

    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  test('clears a pending model-close timer on unmount', async () => {
    const unlisten = mock(() => {});
    await act(async () => { renderer = create(<Probe />); });
    await act(async () => { registration.resolve(unlisten); await registration.promise; });
    const originalSetTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    const clearTimeoutMock = mock(() => {});
    globalThis.setTimeout = mock(() => 41 as unknown as ReturnType<typeof setTimeout>) as unknown as typeof setTimeout;
    globalThis.clearTimeout = clearTimeoutMock as typeof clearTimeout;
    try {
      await act(async () => { downloadCallback?.({ payload: { modelName: 'base' } }); });
      await act(async () => renderer!.unmount());
      renderer = undefined;
      expect(clearTimeoutMock).toHaveBeenCalledWith(41);
    } finally {
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
    }
  });
});
