import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { ModelConfig } from '../../src/types/modelConfig';

const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
const originalBuiltIn = { ...await import('../../src/components/BuiltInModelManager') };
const originalOllamaDownload = { ...await import('../../src/contexts/OllamaDownloadContext') };
afterAll(() => {
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
  mock.module('../../src/components/BuiltInModelManager', () => originalBuiltIn);
  mock.module('../../src/contexts/OllamaDownloadContext', () => originalOllamaDownload);
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
const context = {
  modelConfig: committed,
  commitModelConfig,
  isModelConfigLoading: false,
  isModelConfigSaving: false,
  providerApiKeys: { claude: 'claude-key', groq: 'groq-key', openai: 'openai-key', openrouter: 'router-key' },
  modelOptions: {
    ollama: ['llama3'], claude: ['claude-sonnet-4-5-20250929'], groq: ['llama-3.3-70b-versatile'],
    openrouter: [], openai: ['gpt-4o', 'gpt-4o-mini'], 'builtin-ai': [], 'custom-openai': [],
  },
};
mock.module('../../src/contexts/ConfigContext', () => ({ useConfig: () => context }));
mock.module('../../src/components/BuiltInModelManager', () => ({ BuiltInModelManager: () => null }));
mock.module('../../src/contexts/OllamaDownloadContext', () => ({
  useOllamaDownload: () => ({ isDownloading: () => false, getProgress: () => undefined }),
}));

const { ModelConfigForm } = await import('../../src/components/settings/ModelConfigForm');
const { ModelSettingsModal } = await import('../../src/components/ModelSettingsModal');

let renderer: ReactTestRenderer;
const text = () => JSON.stringify(renderer.toJSON());
const flush = async () => act(async () => { await Promise.resolve(); });

beforeEach(() => commitModelConfig.mockReset());
afterEach(() => renderer?.unmount());

async function renderForm(props: Record<string, unknown> = {}) {
  await act(async () => { renderer = create(<ModelConfigForm showCancel {...props} />); });
  await flush();
}

function modelSelect() {
  return renderer.root.findAllByType('select')[1];
}

function submit() {
  return renderer.root.findByType('form').props.onSubmit({ preventDefault() {} });
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
    await act(async () => resolve({ ...committed, model: 'gpt-4o-mini' }));
    expect(onCommitted).toHaveBeenCalledTimes(1);
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
