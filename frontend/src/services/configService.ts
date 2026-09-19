/**
 * Configuration Service
 *
 * Handles all configuration-related Tauri backend calls.
 * Owns reads and transactional model-configuration writes through Tauri.
 */

import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { TranscriptModelProps } from '@/components/TranscriptSettings';
import type { CustomOpenAIConfig, ModelConfig } from '@/types/modelConfig';

export type { CustomOpenAIConfig, ModelConfig } from '@/types/modelConfig';

export interface RecordingPreferences {
  preferred_mic_device: string | null;
  preferred_system_device: string | null;
  automatic_record_prompt: boolean;
}

type Invoke = typeof invoke;
type Emit = typeof emit;

export class ConfigSaveInProgressError extends Error {
  constructor() {
    super('A model configuration save is already in progress');
    this.name = 'ConfigSaveInProgressError';
  }
}

export class InvalidModelConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'InvalidModelConfigError';
  }
}

export class LatestRequestGuard {
  private revision = 0;

  begin(): number {
    this.revision += 1;
    return this.revision;
  }

  invalidate(): void {
    this.revision += 1;
  }

  isCurrent(request: number): boolean {
    return request === this.revision;
  }
}

function optionalTrimmed(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

export function validateModelConfig(config: ModelConfig): ModelConfig {
  const providers = new Set(['ollama', 'groq', 'claude', 'openrouter', 'openai', 'builtin-ai', 'custom-openai']);
  if (!providers.has(config.provider)) throw new InvalidModelConfigError('Unsupported model provider');

  const customModel = optionalTrimmed(config.customOpenAIModel);
  const model = config.provider === 'custom-openai' ? customModel ?? '' : config.model.trim();
  const whisperModel = config.whisperModel.trim();

  if (!model) throw new InvalidModelConfigError('Model is required');
  if (!whisperModel) throw new InvalidModelConfigError('Transcription model is required');

  const normalized: ModelConfig = {
    ...config,
    model,
    whisperModel,
    apiKey: config.apiKey === undefined ? undefined : optionalTrimmed(config.apiKey),
    ollamaEndpoint: optionalTrimmed(config.ollamaEndpoint),
  };

  if (config.provider !== 'custom-openai') return normalized;

  const rawEndpoint = optionalTrimmed(config.customOpenAIEndpoint);
  const endpoint = rawEndpoint?.replace(/^https?:\/\//i, (scheme) => scheme.toLowerCase()) ?? null;
  if (!endpoint) throw new InvalidModelConfigError('Custom OpenAI endpoint is required');
  if (!/^https?:\/\//.test(endpoint)) {
    throw new InvalidModelConfigError('Custom OpenAI endpoint must start with http:// or https://');
  }
  if (!customModel) throw new InvalidModelConfigError('Custom OpenAI model is required');
  if (config.maxTokens != null && (!Number.isInteger(config.maxTokens) || config.maxTokens < 1 || config.maxTokens > 2_147_483_647)) {
    throw new InvalidModelConfigError('Max tokens must be a positive 32-bit integer');
  }
  if (config.temperature != null && (!Number.isFinite(config.temperature) || config.temperature < 0 || config.temperature > 2)) {
    throw new InvalidModelConfigError('Temperature must be between 0 and 2');
  }
  if (config.topP != null && (!Number.isFinite(config.topP) || config.topP < 0 || config.topP > 1)) {
    throw new InvalidModelConfigError('Top P must be between 0 and 1');
  }

  return {
    ...normalized,
    model: customModel,
    customOpenAIEndpoint: endpoint,
    customOpenAIModel: customModel,
    customOpenAIApiKey: optionalTrimmed(config.customOpenAIApiKey),
    maxTokens: config.maxTokens ?? null,
    temperature: config.temperature ?? null,
    topP: config.topP ?? null,
  };
}

/**
 * Configuration Service
 * Singleton service for managing app configuration
 */
export class ConfigService {
  private saveInProgress = false;

  constructor(
    private readonly invokeCommand: Invoke = invoke,
    private readonly emitEvent: Emit = emit,
  ) {}

  /**
   * Get saved transcript model configuration
   * @returns Promise with { provider, model, apiKey }
   */
  async getTranscriptConfig(): Promise<TranscriptModelProps> {
    return this.invokeCommand<TranscriptModelProps>('api_get_transcript_config');
  }

  /**
   * Get saved summary model configuration
   * @returns Promise with { provider, model, whisperModel }
   */
  async getModelConfig(): Promise<ModelConfig | null> {
    return this.invokeCommand<ModelConfig | null>('api_get_model_config');
  }

  async getProviderApiKey(provider: string): Promise<string | null> {
    const apiKey = await this.invokeCommand<string>('api_get_api_key', { provider });
    return apiKey || null;
  }

  async loadModelConfig(): Promise<ModelConfig | null> {
    const config = await this.getModelConfig();
    if (!config || config.provider !== 'custom-openai') return config;
    if (config.customOpenAIEndpoint && config.customOpenAIModel) return config;

    const customConfig = await this.getCustomOpenAIConfig();
    if (!customConfig) return config;

    return {
      ...config,
      model: customConfig.model || config.model,
      customOpenAIEndpoint: customConfig.endpoint,
      customOpenAIModel: customConfig.model,
      customOpenAIApiKey: customConfig.apiKey,
      maxTokens: customConfig.maxTokens,
      temperature: customConfig.temperature,
      topP: customConfig.topP,
    };
  }

  /**
   * Get saved audio device preferences
   * @returns Promise with { preferred_mic_device, preferred_system_device }
   */
  async getRecordingPreferences(): Promise<RecordingPreferences> {
    return this.invokeCommand<RecordingPreferences>('get_recording_preferences');
  }

  /**
   * Get custom OpenAI configuration
   * @returns Promise with CustomOpenAIConfig or null if not configured
   */
  async getCustomOpenAIConfig(): Promise<CustomOpenAIConfig | null> {
    return this.invokeCommand<CustomOpenAIConfig | null>('api_get_custom_openai_config');
  }

  /**
   * Save custom OpenAI configuration
   * @param config - CustomOpenAIConfig to save
   * @returns Promise with result status
   */
  async saveCustomOpenAIConfig(config: CustomOpenAIConfig): Promise<{ status: string; message: string }> {
    return this.invokeCommand<{ status: string; message: string }>('api_save_custom_openai_config', {
      endpoint: config.endpoint,
      apiKey: config.apiKey,
      model: config.model,
      maxTokens: config.maxTokens,
      temperature: config.temperature,
      topP: config.topP,
    });
  }

  /**
   * Persists one complete model configuration and resolves with the exact value
   * callers may publish as committed state. Concurrent saves are rejected.
   */
  async commitModelConfig(config: ModelConfig): Promise<ModelConfig> {
    if (this.saveInProgress) throw new ConfigSaveInProgressError();

    const committed = validateModelConfig(config);
    this.saveInProgress = true;
    try {
      const customOpenAIConfig = committed.provider === 'custom-openai'
        ? {
          endpoint: committed.customOpenAIEndpoint!,
          apiKey: committed.customOpenAIApiKey ?? null,
          model: committed.customOpenAIModel!,
          maxTokens: committed.maxTokens ?? null,
          temperature: committed.temperature ?? null,
          topP: committed.topP ?? null,
        }
        : null;
      const apiKeyAction = committed.provider === 'custom-openai' || committed.apiKey === undefined
        ? 'preserve'
        : committed.apiKey === null
          ? 'clear'
          : 'set';

      const durableConfig = await this.invokeCommand<ModelConfig>('api_save_model_config', {
        provider: committed.provider,
        model: committed.model,
        whisperModel: committed.whisperModel,
        apiKey: committed.provider === 'custom-openai' ? null : committed.apiKey ?? null,
        ollamaEndpoint: committed.ollamaEndpoint ?? null,
        apiKeyAction,
        customOpenAIConfig,
      });

      try {
        void this.emitEvent('model-config-invalidated').catch(() => undefined);
      } catch {
        // Persistence succeeded; reconciliation notification is best effort.
      }
      return durableConfig;
    } finally {
      this.saveInProgress = false;
    }
  }

  /**
   * Test custom OpenAI connection
   * @param endpoint - API endpoint URL
   * @param apiKey - Optional API key
   * @param model - Model name
   * @returns Promise with test result
   */
  async testCustomOpenAIConnection(
    endpoint: string,
    apiKey: string | null,
    model: string
  ): Promise<{ status: string; message: string; http_status?: number }> {
    return this.invokeCommand<{ status: string; message: string; http_status?: number }>('api_test_custom_openai_connection', {
      endpoint,
      apiKey,
      model,
    });
  }
}

// Export singleton instance
export const configService = new ConfigService();
