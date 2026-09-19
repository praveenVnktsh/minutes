import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestInstance, type ReactTestRenderer } from 'react-test-renderer';
import type { ModelConfig, ProviderApiKeys } from '../../src/types/modelConfig';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
const originalBuiltIn = { ...await import('../../src/components/BuiltInModelManager') };
const originalOllamaDownload = { ...await import('../../src/contexts/OllamaDownloadContext') };
afterAll(() => {
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
  mock.module('../../src/components/BuiltInModelManager', () => originalBuiltIn);
  mock.module('../../src/contexts/OllamaDownloadContext', () => originalOllamaDownload);
  mock.module('@tauri-apps/api/core', () => originalCore);
});

const committed: ModelConfig = {
  provider: 'openai',
  model: 'gpt-4o',
  whisperModel: 'large-v3',
  apiKey: 'openai-key',
  ollamaEndpoint: null,
  customOpenAIEndpoint: 'http://localhost:8000/v1',
  customOpenAIModel: 'local-model',
  customOpenAIApiKey: 'custom-key',
  maxTokens: 4096,
  temperature: 0.7,
  topP: 0.9,
};
const commitModelConfig = mock(async (draft: ModelConfig) => draft);
let invokeImplementation = async (_command: string, _args?: Record<string, unknown>): Promise<unknown> => [];
const invoke = mock((command: string, args?: Record<string, unknown>) => invokeImplementation(command, args));
const downloadState = { downloadingModels: new Set<string>() };
const context = {
  modelConfig: committed,
  commitModelConfig,
  isModelConfigLoading: false,
  isModelConfigSaving: false,
  providerApiKeys: { claude: 'claude-key', groq: 'groq-key', openai: 'openai-key', openrouter: 'router-key' } as ProviderApiKeys,
  modelOptions: {
    ollama: ['llama3'], claude: ['claude-sonnet-4-5-20250929'], groq: ['llama-3.3-70b-versatile'],
    openrouter: [], openai: ['gpt-4o', 'gpt-4o-mini'], 'builtin-ai': [], 'custom-openai': [],
  },
};
mock.module('../../src/contexts/ConfigContext', () => ({ useConfig: () => context }));
mock.module('../../src/components/BuiltInModelManager', () => ({ BuiltInModelManager: () => null }));
mock.module('../../src/contexts/OllamaDownloadContext', () => ({
  useOllamaDownload: () => ({ ...downloadState, isDownloading: (model: string) => downloadState.downloadingModels.has(model), getProgress: () => undefined }),
}));
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));

const { ModelConfigForm } = await import('../../src/components/settings/ModelConfigForm');
const { ModelSettingsModal } = await import('../../src/components/ModelSettingsModal');

let renderer: ReactTestRenderer;
const text = () => JSON.stringify(renderer.toJSON());
const flush = async () => act(async () => { await Promise.resolve(); });

beforeEach(() => {
  commitModelConfig.mockReset();
  commitModelConfig.mockImplementation(async draft => draft);
  invoke.mockClear();
  invokeImplementation = async () => [];
  context.modelConfig = committed;
  context.providerApiKeys = { claude: 'claude-key', groq: 'groq-key', openai: 'openai-key', openrouter: 'router-key' };
  downloadState.downloadingModels = new Set();
});
afterEach(() => renderer?.unmount());

async function renderForm(props: Record<string, unknown> = {}) {
  await act(async () => { renderer = create(<ModelConfigForm showCancel {...props} />); });
  await flush();
}

function modelSelect() {
  return renderer.root.findAllByType('select')[1];
}

function optionValues() {
  return modelSelect().findAllByType('option').map(option => option.props.value);
}

function instanceText(node: ReactTestInstance | string): string {
  return typeof node === 'string' ? node : node.children.map(instanceText).join('');
}

function buttonContaining(label: string) {
  return renderer.root.findAllByType('button').find(button => instanceText(button).includes(label))!;
}

function submit() {
  return renderer.root.findByType('form').props.onSubmit({ preventDefault() {} });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

describe('transactional model configuration form', () => {
  test('Cancel discards its isolated draft without committing', async () => {
    const onCancel = mock(() => {});
    await renderForm({ onCancel });
    act(() => modelSelect().props.onChange({ target: { value: 'gpt-4o-mini' } }));
    expect(modelSelect().props.value).toBe('gpt-4o-mini');

    act(() => renderer.root.findAllByType('button').find(button => button.props.children === 'Cancel')!.props.onClick());

    expect(modelSelect().props.value).toBe('gpt-4o');
    expect(commitModelConfig).not.toHaveBeenCalled();
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  test('keeps a rejected draft open and reports the failure', async () => {
    commitModelConfig.mockImplementationOnce(async () => { throw new Error('database unavailable'); });
    await renderForm();
    act(() => modelSelect().props.onChange({ target: { value: 'gpt-4o-mini' } }));
    await act(async () => { await submit(); });

    expect(modelSelect().props.value).toBe('gpt-4o-mini');
    expect(text()).toContain('Could not save model settings: ');
    expect(text()).toContain('database unavailable');
  });

  test('blocks duplicate submission and notifies only after a successful commit', async () => {
    let resolve!: (value: ModelConfig) => void;
    commitModelConfig.mockImplementationOnce(draft => new Promise(done => { resolve = () => done(draft); }));
    const onCommitted = mock(() => {});
    await renderForm({ onCommitted });
    act(() => modelSelect().props.onChange({ target: { value: 'gpt-4o-mini' } }));

    act(() => { void submit(); void submit(); });
    expect(commitModelConfig).toHaveBeenCalledTimes(1);
    expect(onCommitted).not.toHaveBeenCalled();
    expect(modelSelect().props.disabled).toBe(true);
    act(() => modelSelect().props.onChange({ target: { value: 'gpt-4o' } }));
    expect(modelSelect().props.value).toBe('gpt-4o-mini');
    await act(async () => resolve({ ...committed, model: 'gpt-4o-mini' }));
    expect(onCommitted).toHaveBeenCalledTimes(1);
  });

  test('does not report a committed save as failed when onCommitted throws', async () => {
    commitModelConfig.mockImplementationOnce(async draft => draft);
    await renderForm({ onCommitted: () => { throw new Error('parent closed'); } });
    act(() => modelSelect().props.onChange({ target: { value: 'gpt-4o-mini' } }));
    await act(async () => { await submit(); });
    expect(text()).toContain('Model settings saved');
    expect(text()).not.toContain('Model settings were not saved');
  });

  test('blocks custom field edits while a commit is pending', async () => {
    const pending = deferred<ModelConfig>();
    commitModelConfig.mockImplementationOnce(() => pending.promise);
    context.modelConfig = { ...committed, provider: 'custom-openai', model: 'local-model', apiKey: undefined };
    await renderForm();
    act(() => buttonContaining('Advanced options').props.onClick());
    const endpoint = renderer.root.findAllByType('input').find(input => String(input.props.id).includes('custom-endpoint'))!;
    act(() => endpoint.props.onChange({ target: { value: 'http://pending/v1' } }));
    act(() => { void submit(); });

    const currentInputs = renderer.root.findAllByType('input');
    expect(currentInputs.every(input => input.props.disabled)).toBe(true);
    const maxTokens = currentInputs.find(input => String(input.props.id).includes('max-tokens'))!;
    act(() => maxTokens.props.onChange({ target: { value: '8192' } }));
    expect(maxTokens.props.value).toBe(4096);
    await act(async () => pending.resolve({ ...context.modelConfig, customOpenAIEndpoint: 'http://pending/v1' }));
  });

  test('refreshes a clean form from committed state without overwriting a dirty draft', async () => {
    await renderForm();
    context.modelConfig = { ...committed, model: 'gpt-4o-mini' };
    await act(async () => { renderer.update(<ModelConfigForm showCancel />); });
    expect(modelSelect().props.value).toBe('gpt-4o-mini');

    act(() => modelSelect().props.onChange({ target: { value: 'gpt-4o' } }));
    context.modelConfig = { ...committed, model: 'externally-committed' };
    await act(async () => { renderer.update(<ModelConfigForm showCancel />); });
    expect(modelSelect().props.value).toBe('gpt-4o');
    context.modelConfig = committed;
  });

  test('uses the committed active key and applies late provider keys only while untouched', async () => {
    context.modelConfig = { ...committed, apiKey: 'authoritative-key' };
    context.providerApiKeys = { ...context.providerApiKeys, openai: 'stale-key', claude: null };
    await renderForm();
    const provider = renderer.root.findAllByType('select')[0];
    expect(renderer.root.findAllByType('input')[0].props.value).toBe('authoritative-key');

    act(() => provider.props.onChange({ target: { value: 'claude' } }));
    expect(renderer.root.findAllByType('input')[0].props.value).toBe('');
    context.providerApiKeys = { ...context.providerApiKeys, claude: 'hydrated-key' };
    await act(async () => renderer.update(<ModelConfigForm showCancel />));
    expect(renderer.root.findAllByType('input')[0].props.value).toBe('hydrated-key');

    act(() => renderer.root.findAllByType('input')[0].props.onChange({ target: { value: 'typed-key' } }));
    context.providerApiKeys = { ...context.providerApiKeys, claude: 'later-key' };
    await act(async () => renderer.update(<ModelConfigForm showCancel />));
    expect(renderer.root.findAllByType('input')[0].props.value).toBe('typed-key');
  });

  test('never commits a stale provider cache key over the active committed key', async () => {
    context.modelConfig = { ...committed, apiKey: 'authoritative-key' };
    context.providerApiKeys = { ...context.providerApiKeys, openai: 'stale-key' };
    await renderForm();
    act(() => modelSelect().props.onChange({ target: { value: 'gpt-4o-mini' } }));
    await act(async () => { await submit(); });
    expect(commitModelConfig.mock.calls[0][0].apiKey).toBe('authoritative-key');
  });

  test('ignores stale model responses after switching providers', async () => {
    const oldRequest = deferred<Array<{ name: string }>>();
    invokeImplementation = async command => command === 'get_ollama_models' ? oldRequest.promise : [];
    context.modelConfig = { ...committed, provider: 'ollama', model: 'llama3', apiKey: undefined };
    await renderForm();
    act(() => renderer.root.findAllByType('select')[0].props.onChange({ target: { value: 'openai' } }));
    await act(async () => oldRequest.resolve([{ name: 'stale-ollama-model' }]));

    expect(optionValues()).not.toContain('stale-ollama-model');
    expect(modelSelect().props.value).toBe('gpt-4o');
  });

  test('ignores an older refresh that resolves after the latest request', async () => {
    const first = deferred<Array<{ name: string }>>();
    const second = deferred<Array<{ name: string }>>();
    let requests = 0;
    invokeImplementation = async command => {
      if (command !== 'get_ollama_models') return [];
      return ++requests === 1 ? first.promise : second.promise;
    };
    context.modelConfig = { ...committed, provider: 'ollama', model: 'llama3', apiKey: undefined };
    await renderForm();
    const refresh = buttonContaining('Refresh');
    act(() => refresh.props.onClick());
    await act(async () => second.resolve([{ name: 'new-model' }]));
    await act(async () => first.resolve([{ name: 'old-model' }]));
    expect(optionValues()).toContain('new-model');
    expect(optionValues()).not.toContain('old-model');
  });

  test('survives a malformed provider model cache', async () => {
    const originalWindow = globalThis.window;
    const originalStorage = globalThis.localStorage;
    Object.assign(globalThis, { window: globalThis, localStorage: { getItem: () => '{bad json' } });
    try {
      await renderForm();
      expect(() => act(() => renderer.root.findAllByType('select')[0].props.onChange({ target: { value: 'claude' } }))).not.toThrow();
    } finally {
      Object.assign(globalThis, { window: originalWindow, localStorage: originalStorage });
    }
  });

  test('scopes connection feedback to the tested custom configuration', async () => {
    const first = deferred<{ message: string }>();
    invokeImplementation = async command => command === 'api_test_custom_openai_connection' ? first.promise : [];
    context.modelConfig = { ...committed, provider: 'custom-openai', model: 'local-model', apiKey: undefined };
    await renderForm();
    const testButton = buttonContaining('Test connection');
    act(() => testButton.props.onClick());
    const endpoint = renderer.root.findAllByType('input').find(input => String(input.props.id).includes('custom-endpoint'))!;
    act(() => endpoint.props.onChange({ target: { value: 'http://changed/v1' } }));
    await act(async () => first.resolve({ message: 'Old connection succeeded' }));
    expect(text()).not.toContain('Old connection succeeded');

    invokeImplementation = async command => command === 'api_test_custom_openai_connection' ? { message: 'Connection ready' } : [];
    await act(async () => buttonContaining('Test connection').props.onClick());
    expect(text()).toContain('Connection ready');
  });

  test('deletes Ollama models as immediate library operations', async () => {
    invokeImplementation = async command => command === 'get_ollama_models' ? [{ name: 'llama3' }] : null;
    context.modelConfig = { ...committed, provider: 'ollama', model: 'llama3', apiKey: undefined };
    await renderForm();
    const remove = renderer.root.findByProps({ 'aria-label': 'Delete llama3 from Ollama' });
    await act(async () => { await remove.props.onClick(); });
    expect(invoke.mock.calls.some(call => call[0] === 'delete_ollama_model')).toBe(true);
    expect(text()).toContain('llama3 deleted from Ollama');
  });

  test('refreshes Ollama models when a shared download completes', async () => {
    invokeImplementation = async command => command === 'get_ollama_models' ? [{ name: 'llama3' }] : null;
    downloadState.downloadingModels = new Set(['gemma3:1b']);
    context.modelConfig = { ...committed, provider: 'ollama', model: 'llama3', apiKey: undefined };
    await renderForm();
    const readsBeforeCompletion = invoke.mock.calls.filter(call => call[0] === 'get_ollama_models').length;
    downloadState.downloadingModels = new Set();
    await act(async () => { renderer.update(<ModelConfigForm showCancel />); await Promise.resolve(); });
    expect(invoke.mock.calls.filter(call => call[0] === 'get_ollama_models').length).toBeGreaterThan(readsBeforeCompletion);
  });

  test('preserves every custom provider field in the one owner commit', async () => {
    context.modelConfig = { ...committed, provider: 'custom-openai', model: 'local-model', apiKey: undefined };
    const onSave = mock(() => {});
    await act(async () => { renderer = create(<ModelSettingsModal modelConfig={committed} setModelConfig={() => {}} onSave={onSave} />); });
    await flush();
    const endpoint = renderer.root.findAllByType('input').find(input => String(input.props.id).includes('custom-endpoint'))!;
    act(() => endpoint.props.onChange({ target: { value: 'http://localhost:9000/v1' } }));
    await act(async () => { await submit(); });

    expect(commitModelConfig).toHaveBeenCalledTimes(1);
    expect(commitModelConfig.mock.calls[0][0]).toMatchObject({
      customOpenAIEndpoint: 'http://localhost:9000/v1', customOpenAIModel: 'local-model',
      customOpenAIApiKey: 'custom-key', maxTokens: 4096, temperature: 0.7, topP: 0.9,
    });
    expect(onSave).not.toHaveBeenCalled();
    context.modelConfig = committed;
  });
});
