import { afterAll, afterEach, describe, expect, mock, test } from 'bun:test';
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

const registration = deferred<() => void>();
const listen = mock(() => registration.promise);
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen }));
mock.module('sonner', () => ({ toast: { success: mock(() => {}) } }));

const { useModalState } = await import('../../src/hooks/useModalState');
let renderer: ReactTestRenderer | undefined;

function Probe() {
  useModalState({ provider: 'localWhisper', model: 'base', apiKey: null });
  return null;
}

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
});
