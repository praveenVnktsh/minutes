import { describe, expect, test } from 'bun:test';
import {
  ConfigSaveInProgressError,
  ConfigService,
  InvalidModelConfigError,
  LatestRequestGuard,
} from './configService';
import { mergeProviderKeyHydration, type ModelConfig, type ProviderApiKeys } from '@/types/modelConfig';

type Invoke = ConstructorParameters<typeof ConfigService>[0];
type Emit = ConstructorParameters<typeof ConfigService>[1];

const baseConfig: ModelConfig = {
  provider: 'openai',
  model: 'gpt-4o',
  whisperModel: 'large-v3',
  apiKey: 'secret-key',
};

function durable(config: ModelConfig): ModelConfig {
  return { ...config, ollamaEndpoint: config.ollamaEndpoint ?? null };
}

describe('ConfigService.commitModelConfig', () => {
  test('rejects invalid drafts without writing or publishing', async () => {
    const calls: string[] = [];
    const service = new ConfigService(
      (async (command: string) => {
        calls.push(command);
        return {};
      }) as Invoke,
      (async () => {
        calls.push('emit');
      }) as Emit,
    );

    await expect(service.commitModelConfig({ ...baseConfig, model: ' ' }))
      .rejects.toBeInstanceOf(InvalidModelConfigError);
    expect(calls).toEqual([]);
  });

  test('rejects a concurrent save while the active save is pending', async () => {
    let finishSave: (() => void) | undefined;
    const pendingSave = new Promise<void>((resolve) => {
      finishSave = resolve;
    });
    const service = new ConfigService(
      (async (command: string, payload?: Record<string, unknown>) => {
        if (command === 'api_save_model_config') await pendingSave;
        return durable(payload as unknown as ModelConfig);
      }) as Invoke,
      (async () => {}) as Emit,
    );

    const firstSave = service.commitModelConfig(baseConfig);
    await expect(service.commitModelConfig({ ...baseConfig, model: 'gpt-4.1' }))
      .rejects.toBeInstanceOf(ConfigSaveInProgressError);

    finishSave?.();
    await expect(firstSave).resolves.toMatchObject(baseConfig);
  });

  test('atomically sends every custom field and accepts an empty generic model', async () => {
    const invokes: Array<{ command: string; payload?: Record<string, unknown> }> = [];
    const notifications: Array<{ event: string; payload?: unknown }> = [];
    let returnedConfig: ModelConfig;
    const service = new ConfigService(
      (async (command: string, payload?: Record<string, unknown>) => {
        invokes.push({ command, payload });
        returnedConfig = {
          ...(payload as unknown as ModelConfig),
          apiKey: null,
          customOpenAIEndpoint: 'https://models.example.test/v1',
          customOpenAIModel: 'private-model',
          customOpenAIApiKey: 'custom-secret',
          maxTokens: 4096,
          temperature: 0.4,
          topP: 0.9,
        };
        return returnedConfig;
      }) as Invoke,
      (async (event: string, payload?: unknown) => {
        notifications.push({ event, payload });
      }) as Emit,
    );
    const draft: ModelConfig = {
      provider: 'custom-openai',
      model: '',
      whisperModel: ' large-v3 ',
      customOpenAIEndpoint: ' HTTPS://models.example.test/v1 ',
      customOpenAIModel: ' private-model ',
      customOpenAIApiKey: ' custom-secret ',
      maxTokens: 4096,
      temperature: 0.4,
      topP: 0.9,
    };

    const committed = await service.commitModelConfig(draft);

    expect(committed).toBe(returnedConfig!);
    expect(invokes).toEqual([{
      command: 'api_save_model_config',
      payload: {
        provider: 'custom-openai',
        model: 'private-model',
        whisperModel: 'large-v3',
        apiKey: null,
        ollamaEndpoint: null,
        apiKeyAction: 'preserve',
        customOpenAIConfig: {
          endpoint: 'https://models.example.test/v1',
          apiKey: 'custom-secret',
          model: 'private-model',
          maxTokens: 4096,
          temperature: 0.4,
          topP: 0.9,
        },
      },
    }]);
    expect(notifications).toEqual([{ event: 'model-config-invalidated', payload: undefined }]);
  });

  test('uses explicit provider-key preserve, set, and clear actions', async () => {
    const actions: unknown[] = [];
    const service = new ConfigService(
      (async (_command: string, payload?: Record<string, unknown>) => {
        actions.push(payload?.apiKeyAction);
        return durable(payload as unknown as ModelConfig);
      }) as Invoke,
      (async () => {}) as Emit,
    );

    await service.commitModelConfig({ ...baseConfig, apiKey: undefined });
    await service.commitModelConfig(baseConfig);
    await service.commitModelConfig({ ...baseConfig, apiKey: null });
    expect(actions).toEqual(['preserve', 'set', 'clear']);
  });

  test('propagates persistence failures and does not publish them', async () => {
    const published: unknown[] = [];
    const service = new ConfigService(
      (async () => {
        throw new Error('database unavailable');
      }) as Invoke,
      (async (_event: string, payload?: unknown) => {
        published.push(payload);
      }) as Emit,
    );

    await expect(service.commitModelConfig(baseConfig)).rejects.toThrow('database unavailable');
    expect(published).toEqual([]);
  });

  test('returns a durable commit even when invalidation transport fails', async () => {
    const committed = durable(baseConfig);
    const service = new ConfigService(
      (async () => committed) as Invoke,
      (async () => {
        throw new Error('event unavailable');
      }) as Emit,
    );

    await expect(service.commitModelConfig(baseConfig)).resolves.toBe(committed);
  });

  test('serial commits publish the native result for A and then B', async () => {
    const persisted: ModelConfig[] = [];
    const service = new ConfigService(
      (async (_command: string, payload?: Record<string, unknown>) => {
        const result = durable(payload as unknown as ModelConfig);
        persisted.push(result);
        return result;
      }) as Invoke,
      (async () => {}) as Emit,
    );

    const first = await service.commitModelConfig({ ...baseConfig, model: 'model-a' });
    const second = await service.commitModelConfig({ ...baseConfig, model: 'model-b' });
    expect(first.model).toBe('model-a');
    expect(second.model).toBe('model-b');
    expect(persisted.map(config => config.model)).toEqual(['model-a', 'model-b']);
  });
});

describe('LatestRequestGuard', () => {
  test('prevents a delayed hydration from replacing a newer result', () => {
    const guard = new LatestRequestGuard();
    const delayed = guard.begin();
    const latest = guard.begin();

    expect(guard.isCurrent(delayed)).toBe(false);
    expect(guard.isCurrent(latest)).toBe(true);
  });

  test('allows reconciliation after an invalidated or failed request', () => {
    const guard = new LatestRequestGuard();
    const failed = guard.begin();
    guard.invalidate();
    const retry = guard.begin();

    expect(guard.isCurrent(failed)).toBe(false);
    expect(guard.isCurrent(retry)).toBe(true);
  });
});

describe('provider key hydration', () => {
  test('preserves failed reads and providers changed after hydration began', () => {
    const current: ProviderApiKeys = {
      claude: 'old-claude',
      groq: 'old-groq',
      openai: 'just-committed',
      openrouter: null,
    };
    const started = { claude: 0, groq: 0, openai: 0, openrouter: 0 };
    const currentRevisions = { ...started, openai: 1 };

    expect(mergeProviderKeyHydration(current, [
      { provider: 'claude', apiKey: 'loaded-claude', succeeded: true },
      { provider: 'groq', apiKey: null, succeeded: false },
      { provider: 'openai', apiKey: 'stale-openai', succeeded: true },
      { provider: 'openrouter', apiKey: null, succeeded: true },
    ], started, currentRevisions)).toEqual({
      claude: 'loaded-claude',
      groq: 'old-groq',
      openai: 'just-committed',
      openrouter: null,
    });
  });
});
