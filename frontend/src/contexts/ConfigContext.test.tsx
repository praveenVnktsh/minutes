import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { ModelConfig, ProviderApiKeys } from '@/types/modelConfig';

type EventHandler = (event: { payload: unknown }) => void;
type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
};

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

const handlers = new Map<string, Set<EventHandler>>();
let invokeImplementation: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
let listenImplementation: (event: string, handler: EventHandler) => Promise<() => void>;
let emitShouldFail = false;

const invoke = mock((command: string, args?: Record<string, unknown>) => invokeImplementation(command, args));
const listen = mock((event: string, handler: EventHandler) => listenImplementation(event, handler));
const emit = mock(async (event: string, payload?: unknown) => {
  if (emitShouldFail) throw new Error('event unavailable');
  for (const handler of handlers.get(event) ?? []) handler({ payload });
});

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvent = { ...await import('@tauri-apps/api/event') };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
});
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, emit, listen }));

const { ConfigProvider, useConfig } = await import('./ConfigContext');

type ConfigContextValue = ReturnType<typeof useConfig>;
let current!: ConfigContextValue;
let renderer: ReactTestRenderer | undefined;
let storedConfig: ModelConfig;
let storedKeys: ProviderApiKeys;

function Probe() {
  current = useConfig();
  return <output>{current.modelConfig.model}</output>;
}

async function flush() {
  await act(async () => {
    await new Promise(resolve => setTimeout(resolve, 0));
  });
}

async function mountProvider() {
  await act(async () => {
    renderer = create(<ConfigProvider><Probe /></ConfigProvider>);
  });
  await flush();
}

async function dispatch(event: string, payload?: unknown) {
  await act(async () => {
    for (const handler of handlers.get(event) ?? []) handler({ payload });
  });
  await flush();
}

beforeEach(() => {
  handlers.clear();
  invoke.mockClear();
  listen.mockClear();
  emit.mockClear();
  emitShouldFail = false;
  storedConfig = {
    provider: 'openai',
    model: 'committed-a',
    whisperModel: 'large-v3',
    apiKey: 'openai-a',
    ollamaEndpoint: null,
  };
  storedKeys = { claude: null, groq: null, openai: 'openai-a', openrouter: null };
  listenImplementation = async (event, handler) => {
    const registered = handlers.get(event) ?? new Set<EventHandler>();
    registered.add(handler);
    handlers.set(event, registered);
    return () => registered.delete(handler);
  };
  invokeImplementation = async (command, args) => {
    if (command === 'api_get_model_config') return { ...storedConfig };
    if (command === 'api_get_api_key') return storedKeys[args?.provider as keyof ProviderApiKeys] ?? '';
    if (command === 'api_get_transcript_config') return null;
    if (command === 'get_ollama_models') return [];
    if (command === 'get_recording_preferences') return null;
    if (command === 'set_language_preference') return null;
    if (command === 'api_save_model_config') {
      storedConfig = {
        provider: args?.provider as ModelConfig['provider'],
        model: args?.model as string,
        whisperModel: args?.whisperModel as string,
        apiKey: args?.apiKey as string | null,
        ollamaEndpoint: args?.ollamaEndpoint as string | null,
      };
      return { ...storedConfig };
    }
    throw new Error(`Unexpected command: ${command}`);
  };
});

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

describe('ConfigProvider committed model configuration', () => {
  test('retains committed state when a save is rejected', async () => {
    await mountProvider();
    invokeImplementation = async (command) => {
      if (command === 'api_save_model_config') throw new Error('database unavailable');
      if (command === 'api_get_model_config') return { ...storedConfig };
      if (command === 'api_get_api_key') return '';
      return null;
    };

    await act(async () => {
      await current.commitModelConfig({ ...storedConfig, model: 'rejected' }).catch(() => undefined);
    });
    await flush();

    expect(current.modelConfig.model).toBe('committed-a');
    expect(current.modelConfigSaveError?.message).toBe('database unavailable');
  });

  test('recovers initial loading after validation failure while the first read is stale', async () => {
    const initial = deferred<ModelConfig>();
    let configReads = 0;
    invokeImplementation = async (command, args) => {
      if (command === 'api_get_model_config') {
        configReads += 1;
        return configReads === 1 ? initial.promise : { ...storedConfig, model: 'recovered' };
      }
      if (command === 'api_get_api_key') return storedKeys[args?.provider as keyof ProviderApiKeys] ?? '';
      if (command === 'get_ollama_models') return [];
      return null;
    };
    await mountProvider();

    await act(async () => {
      await current.commitModelConfig({ ...storedConfig, model: '' }).catch(() => undefined);
    });
    await flush();
    expect(current.modelConfig.model).toBe('recovered');
    expect(current.isModelConfigLoading).toBe(false);

    initial.resolve({ ...storedConfig, model: 'stale-initial' });
    await flush();
    expect(current.modelConfig.model).toBe('recovered');
  });

  test('reconciles an event received during save and ignores its stale payload', async () => {
    await mountProvider();
    const save = deferred<ModelConfig>();
    invokeImplementation = async (command, args) => {
      if (command === 'api_save_model_config') return save.promise;
      if (command === 'api_get_model_config') return { ...storedConfig };
      if (command === 'api_get_api_key') return storedKeys[args?.provider as keyof ProviderApiKeys] ?? '';
      return command === 'get_ollama_models' ? [] : null;
    };

    let committing!: Promise<ModelConfig>;
    act(() => {
      committing = current.commitModelConfig({ ...storedConfig, model: 'committed-b' });
    });
    await dispatch('model-config-updated', { ...storedConfig, model: 'stale-payload' });
    storedConfig = { ...storedConfig, model: 'committed-b' };
    save.resolve({ ...storedConfig });
    await act(async () => {
      await committing;
    });
    await flush();

    expect(current.modelConfig.model).toBe('committed-b');
  });

  test('a delayed old invalidation cannot regress a later commit', async () => {
    await mountProvider();
    await act(async () => {
      await current.commitModelConfig({ ...storedConfig, model: 'model-a' });
      await current.commitModelConfig({ ...storedConfig, model: 'model-b' });
    });
    await dispatch('model-config-updated', { ...storedConfig, model: 'model-a' });

    expect(current.modelConfig.model).toBe('model-b');
  });

  test('an older key hydration cannot overwrite newer keys or failed providers', async () => {
    const oldReads = new Map<keyof ProviderApiKeys, Deferred<string>>();
    for (const provider of ['claude', 'groq', 'openai', 'openrouter'] as const) {
      oldReads.set(provider, deferred<string>());
    }
    let hydration = 1;
    invokeImplementation = async (command, args) => {
      if (command === 'api_get_model_config') return { ...storedConfig };
      if (command === 'api_get_api_key') {
        const provider = args?.provider as keyof ProviderApiKeys;
        if (hydration === 1) return oldReads.get(provider)!.promise;
        if (provider === 'groq') throw new Error('temporary key read failure');
        return `new-${provider}`;
      }
      if (command === 'get_ollama_models') return [];
      return null;
    };
    await mountProvider();
    hydration = 2;
    act(() => current.updateProviderApiKey('groq', 'local-groq'));
    await dispatch('model-config-invalidated');
    expect(current.providerApiKeys.openai).toBe('new-openai');

    for (const [provider, read] of oldReads) read.resolve(`old-${provider}`);
    await flush();
    expect(current.providerApiKeys).toEqual({
      claude: 'new-claude',
      groq: 'local-groq',
      openai: 'new-openai',
      openrouter: 'new-openrouter',
    });
  });

  test('malformed cache and failed notification do not reject a durable commit', async () => {
    Object.defineProperty(globalThis, 'window', { configurable: true, value: globalThis });
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      value: {
        getItem: (key: string) => key === 'providerModelMap' ? '{broken' : null,
        setItem: () => undefined,
      },
    });
    emitShouldFail = true;
    await mountProvider();

    await act(async () => {
      await current.commitModelConfig({ ...storedConfig, model: 'durable' });
    });
    expect(current.modelConfig.model).toBe('durable');
  });

  test('cleans up listeners that register after unmount and after partial setup failure', async () => {
    const firstRegistration = deferred<() => void>();
    const lateUnlisten = mock(() => undefined);
    const partialUnlisten = mock(() => undefined);
    let registration = 0;
    listenImplementation = async () => {
      registration += 1;
      if (registration === 1) return firstRegistration.promise;
      throw new Error('second registration failed');
    };

    await mountProvider();
    await act(async () => renderer!.unmount());
    renderer = undefined;
    firstRegistration.resolve(lateUnlisten);
    await flush();
    expect(lateUnlisten).toHaveBeenCalledTimes(1);

    registration = 0;
    listenImplementation = async () => {
      registration += 1;
      if (registration === 1) return partialUnlisten;
      throw new Error('second registration failed');
    };
    await mountProvider();
    await flush();
    expect(partialUnlisten).toHaveBeenCalledTimes(1);
  });
});
