import { afterEach, beforeEach, describe, expect, mock, test, type Mock } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import {
  listenAll,
  useModelDownload,
  type CancelOutcome,
  type ModelDownload,
  type ModelDownloadEvent,
  type ModelDownloadSource,
  type UseModelDownloadOptions,
} from './useModelDownload';

type Emit = (event: ModelDownloadEvent) => void;

interface FakeSource extends ModelDownloadSource {
  emit: Emit;
  unlisten: Mock<() => void>;
  subscribe: Mock<(emit: Emit) => Promise<() => void>>;
  start: Mock<(model: string) => Promise<void>>;
  cancel: Mock<(model: string) => Promise<CancelOutcome | void>>;
}

function fakeSource(): FakeSource {
  const unlisten = mock(() => {});
  const source: FakeSource = {
    emit: () => {
      throw new Error('not subscribed');
    },
    unlisten,
    subscribe: mock(async (emit: Emit) => {
      source.emit = emit;
      return unlisten;
    }),
    start: mock(async (_model: string) => {}),
    cancel: mock(async (_model: string): Promise<CancelOutcome | void> => 'cancelled'),
  };
  return source;
}

let hook: ModelDownload;
let renderer: ReactTestRenderer | undefined;

function Probe(props: UseModelDownloadOptions) {
  hook = useModelDownload(props);
  return null;
}

async function render(props: UseModelDownloadOptions) {
  await act(async () => {
    renderer = create(<Probe {...props} />);
  });
}

async function emit(source: FakeSource, event: ModelDownloadEvent) {
  await act(async () => {
    source.emit(event);
  });
}

const realNow = Date.now;
let now = 1_000;
const realLocalStorage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');

function stubLocalStorage(initial: Record<string, string> = {}) {
  const store = new Map(Object.entries(initial));
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => void store.set(key, value),
    },
  });
  return store;
}

beforeEach(() => {
  now = 1_000;
  Date.now = () => now;
});

afterEach(async () => {
  Date.now = realNow;
  if (realLocalStorage) Object.defineProperty(globalThis, 'localStorage', realLocalStorage);
  else Reflect.deleteProperty(globalThis, 'localStorage');
  if (renderer) {
    const current = renderer;
    renderer = undefined;
    await act(async () => current.unmount());
  }
});

describe('useModelDownload', () => {
  test('download marks the model downloading, calls onStart, then source.start', async () => {
    const source = fakeSource();
    const calls: string[] = [];
    source.start.mockImplementation(async (model: string) => {
      calls.push(`start:${model}:${hook.downloading.has(model) || hook.isBusy(model)}`);
    });
    await render({ source, onStart: model => calls.push(`onStart:${model}`) });
    expect(hook.listenersReady).toBe(true);

    let started: boolean | undefined;
    await act(async () => {
      started = await hook.download('small');
    });
    expect(started).toBe(true);
    expect(calls).toEqual(['onStart:small', 'start:small:true']);
    expect(hook.downloading.has('small')).toBe(true);
    expect(hook.isBusy('small')).toBe(true);
  });

  test('busy guard returns false for downloading or cancelling models', async () => {
    const source = fakeSource();
    const onStart = mock(() => {});
    await render({ source, onStart });

    await act(async () => {
      await hook.download('small');
    });
    let second: boolean | undefined;
    await act(async () => {
      second = await hook.download('small');
    });
    expect(second).toBe(false);

    await act(async () => hook.markCancelling('medium'));
    let third: boolean | undefined;
    await act(async () => {
      third = await hook.download('medium');
    });
    expect(third).toBe(false);
    expect(onStart).toHaveBeenCalledTimes(1);
    expect(source.start).toHaveBeenCalledTimes(1);
  });

  test('start rejection settles the model and then calls onStartFailed', async () => {
    const source = fakeSource();
    const failure = new Error('disk full');
    source.start.mockImplementation(async () => {
      throw failure;
    });
    const seen: Array<{ busy: boolean; error: unknown }> = [];
    await render({
      source,
      onStartFailed: (model, error) => seen.push({ busy: hook.isBusy(model), error }),
    });

    let started: boolean | undefined;
    await act(async () => {
      started = await hook.download('small');
    });
    expect(started).toBe(true);
    expect(seen).toEqual([{ busy: false, error: failure }]);
    expect(hook.downloading.has('small')).toBe(false);
  });

  test('throttles progress per model, passing >=5% jumps, stale updates, 100% and completed', async () => {
    const source = fakeSource();
    const onProgress = mock((_model: string, _progress: number) => {});
    await render({ source, onProgress });
    const detail = { downloadedMb: 10, totalMb: 100, speedMbps: 2 };

    await emit(source, { kind: 'progress', model: 'small', progress: 10, detail });
    expect(hook.progress.small).toBe(10);
    expect(hook.detail.small).toEqual(detail);

    now += 100;
    await emit(source, { kind: 'progress', model: 'small', progress: 12 });
    expect(hook.progress.small).toBe(10);

    now += 100;
    await emit(source, { kind: 'progress', model: 'small', progress: 15 });
    expect(hook.progress.small).toBe(15);
    expect(hook.detail.small).toEqual(detail);

    now += 301;
    await emit(source, { kind: 'progress', model: 'small', progress: 16 });
    expect(hook.progress.small).toBe(16);

    now += 1;
    await emit(source, { kind: 'progress', model: 'small', progress: 17, completed: true });
    expect(hook.progress.small).toBe(17);

    now += 1;
    await emit(source, { kind: 'progress', model: 'small', progress: 100 });
    expect(hook.progress.small).toBe(100);

    // A different model has its own throttle entry.
    await emit(source, { kind: 'progress', model: 'medium', progress: 1 });
    expect(hook.progress.medium).toBe(1);

    expect(onProgress.mock.calls.map(call => [call[0], call[1]])).toEqual([
      ['small', 10],
      ['small', 15],
      ['small', 16],
      ['small', 17],
      ['small', 100],
      ['medium', 1],
    ]);
  });

  test('a progress event for an unknown model restores it as downloading', async () => {
    const source = fakeSource();
    await render({ source });
    expect(hook.isBusy('small')).toBe(false);
    await emit(source, { kind: 'progress', model: 'small', progress: 40 });
    expect(hook.downloading.has('small')).toBe(true);
    expect(hook.isBusy('small')).toBe(true);
  });

  test('complete, error and cancelled settle first, then call the latest handler', async () => {
    const source = fakeSource();
    const stale = mock(() => {});
    await render({ source, onComplete: stale, onError: stale, onCancelled: stale });

    const seen: string[] = [];
    const record = (label: string) => (model: string, extra?: string) =>
      seen.push(`${label}:${model}:${extra ?? ''}:${hook.isBusy(model)}`);
    await act(async () => {
      renderer!.update(
        <Probe
          source={source}
          onComplete={record('complete')}
          onError={record('error')}
          onCancelled={record('cancelled')}
        />
      );
    });

    for (const model of ['a', 'b', 'c']) {
      await emit(source, { kind: 'progress', model, progress: 50, detail: { downloadedMb: 1, totalMb: 2, speedMbps: 1 } });
    }
    await act(async () => hook.markCancelling('c'));

    await emit(source, { kind: 'complete', model: 'a' });
    await emit(source, { kind: 'error', model: 'b', error: 'boom' });
    await emit(source, { kind: 'cancelled', model: 'c' });

    expect(stale).not.toHaveBeenCalled();
    expect(seen).toEqual(['complete:a::false', 'error:b:boom:false', 'cancelled:c::false']);
    expect(hook.downloading.size).toBe(0);
    expect(hook.cancelling.size).toBe(0);
    expect(hook.progress).toEqual({});
    expect(hook.detail).toEqual({});

    // Settling dropped the throttle entry, so the next update passes immediately.
    await emit(source, { kind: 'progress', model: 'a', progress: 51 });
    expect(hook.progress.a).toBe(51);
  });

  test('settle is idempotent', async () => {
    const source = fakeSource();
    await render({ source });
    await act(async () => {
      hook.settle('never-started');
      hook.settle('never-started');
    });
    expect(hook.downloading.size).toBe(0);
  });

  test('cancel marks the model cancelling and returns the outcome', async () => {
    const source = fakeSource();
    let resolveCancel: (outcome: CancelOutcome) => void = () => {};
    source.cancel.mockImplementation(() => new Promise(resolve => {
      resolveCancel = resolve;
    }));
    await render({ source });
    await act(async () => {
      await hook.download('small');
    });

    let pending: Promise<CancelOutcome | void> | undefined;
    await act(async () => {
      pending = hook.cancel('small');
    });
    expect(hook.cancelling.has('small')).toBe(true);
    expect(source.cancel).toHaveBeenCalledWith('small');

    let outcome: CancelOutcome | void | undefined;
    await act(async () => {
      resolveCancel('pending');
      outcome = await pending;
    });
    expect(outcome).toBe('pending');
    // The hook leaves the model cancelling until the manager or an engine event settles it.
    expect(hook.cancelling.has('small')).toBe(true);
    expect(hook.isBusy('small')).toBe(true);
  });

  test('cancel clears cancelling and rethrows when the source throws', async () => {
    const source = fakeSource();
    const failure = new Error('no owner');
    source.cancel.mockImplementation(async () => {
      throw failure;
    });
    await render({ source });

    let caught: unknown;
    await act(async () => {
      try {
        await hook.cancel('small');
      } catch (err) {
        caught = err;
      }
    });
    expect(caught).toBe(failure);
    expect(hook.cancelling.has('small')).toBe(false);
    expect(hook.isBusy('small')).toBe(false);
  });

  test('persistKey loads the in-flight set and writes every change back', async () => {
    const store = stubLocalStorage({ 'downloading-models': JSON.stringify(['small']) });
    const source = fakeSource();
    await render({ source, persistKey: 'downloading-models' });
    expect([...hook.downloading]).toEqual(['small']);
    expect(hook.isBusy('small')).toBe(true);

    await act(async () => {
      await hook.download('medium');
    });
    expect(JSON.parse(store.get('downloading-models')!)).toEqual(['small', 'medium']);

    await emit(source, { kind: 'complete', model: 'small' });
    expect(JSON.parse(store.get('downloading-models')!)).toEqual(['medium']);
  });

  test('persistKey survives bad JSON and missing localStorage', async () => {
    const store = stubLocalStorage({ 'downloading-models': '{not json' });
    const source = fakeSource();
    await render({ source, persistKey: 'downloading-models' });
    expect(hook.downloading.size).toBe(0);
    await act(async () => {
      await hook.download('small');
    });
    expect(store.get('downloading-models')).toBe('["small"]');
    await act(async () => renderer!.unmount());
    renderer = undefined;

    Reflect.deleteProperty(globalThis, 'localStorage');
    const other = fakeSource();
    await render({ source: other, persistKey: 'downloading-models' });
    expect(hook.downloading.size).toBe(0);
    await act(async () => {
      await hook.download('small');
    });
    expect(hook.downloading.has('small')).toBe(true);
  });

  test('without persistKey localStorage is never touched', async () => {
    const store = stubLocalStorage({ 'downloading-models': '["small"]' });
    const source = fakeSource();
    await render({ source });
    expect(hook.downloading.size).toBe(0);
    await act(async () => {
      await hook.download('medium');
    });
    expect(store.get('downloading-models')).toBe('["small"]');
  });

  test('a rejected subscribe sets listenError', async () => {
    for (const [reason, message] of [
      [new Error('ipc down'), 'ipc down'],
      ['plain string', 'plain string'],
      [{ code: 1 }, 'Failed to listen for model download updates'],
    ] as const) {
      const source = fakeSource();
      source.subscribe.mockImplementation(async () => {
        throw reason;
      });
      await render({ source });
      expect(hook.listenError).toBe(message);
      expect(hook.listenersReady).toBe(false);
      await act(async () => renderer!.unmount());
      renderer = undefined;
    }
  });

  test('unmount unlistens', async () => {
    const source = fakeSource();
    await render({ source });
    expect(source.subscribe).toHaveBeenCalledTimes(1);
    await act(async () => renderer!.unmount());
    renderer = undefined;
    expect(source.unlisten).toHaveBeenCalledTimes(1);
  });

  test('unmount before subscribe resolves unlistens as soon as it does', async () => {
    const source = fakeSource();
    let resolveSubscribe: (fn: () => void) => void = () => {};
    source.subscribe.mockImplementation(() => new Promise(resolve => {
      resolveSubscribe = resolve;
    }));
    await render({ source });
    expect(hook.listenersReady).toBe(false);
    await act(async () => renderer!.unmount());
    renderer = undefined;
    expect(source.unlisten).not.toHaveBeenCalled();

    await act(async () => {
      resolveSubscribe(source.unlisten);
    });
    expect(source.unlisten).toHaveBeenCalledTimes(1);
  });

  test('a rejected subscribe after unmount does not set listenError', async () => {
    const source = fakeSource();
    let rejectSubscribe: (reason: unknown) => void = () => {};
    source.subscribe.mockImplementation(() => new Promise((_resolve, reject) => {
      rejectSubscribe = reject;
    }));
    await render({ source });
    await act(async () => renderer!.unmount());
    renderer = undefined;
    await act(async () => {
      rejectSubscribe(new Error('late'));
    });
    expect(hook.listenError).toBeNull();
  });

  test('download, cancel and settle are referentially stable across renders', async () => {
    const source = fakeSource();
    await render({ source });
    const first = hook;
    await act(async () => {
      await hook.download('small');
    });
    expect(hook.download).toBe(first.download);
    expect(hook.cancel).toBe(first.cancel);
    expect(hook.settle).toBe(first.settle);
    expect(hook.markCancelling).toBe(first.markCancelling);
    expect(hook.isBusy).toBe(first.isBusy);
  });
});

describe('listenAll', () => {
  test('returns one unlisten that releases every registration', async () => {
    const a = mock(() => {});
    const b = mock(() => {});
    const unlisten = await listenAll([Promise.resolve(a), Promise.resolve(b)]);
    expect(a).not.toHaveBeenCalled();
    unlisten();
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);
  });

  test('unlistens the fulfilled registrations and rethrows when one rejects', async () => {
    const a = mock(() => {});
    const c = mock(() => {});
    const failure = new Error('listen failed');
    let caught: unknown;
    try {
      await listenAll([Promise.resolve(a), Promise.reject(failure), Promise.resolve(c)]);
    } catch (err) {
      caught = err;
    }
    expect(caught).toBe(failure);
    expect(a).toHaveBeenCalledTimes(1);
    expect(c).toHaveBeenCalledTimes(1);
  });
});
