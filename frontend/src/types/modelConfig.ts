import { DEFAULT_WHISPER_MODEL } from '@/constants/modelDefaults';

export type ModelProvider =
  | 'ollama'
  | 'groq'
  | 'claude'
  | 'openrouter'
  | 'openai'
  | 'builtin-ai'
  | 'custom-openai';

export interface ModelConfig {
  provider: ModelProvider;
  model: string;
  whisperModel: string;
  apiKey?: string | null;
  ollamaEndpoint?: string | null;
  customOpenAIEndpoint?: string | null;
  customOpenAIModel?: string | null;
  customOpenAIApiKey?: string | null;
  maxTokens?: number | null;
  temperature?: number | null;
  topP?: number | null;
}

export interface CustomOpenAIConfig {
  endpoint: string;
  apiKey: string | null;
  model: string;
  maxTokens: number | null;
  temperature: number | null;
  topP: number | null;
}

export type ProviderApiKeys = Record<'claude' | 'groq' | 'openai' | 'openrouter', string | null>;

export interface ProviderKeyHydrationResult {
  provider: keyof ProviderApiKeys;
  apiKey: string | null;
  succeeded: boolean;
}

export function mergeProviderKeyHydration(
  current: ProviderApiKeys,
  results: ProviderKeyHydrationResult[],
  startedRevisions: Record<keyof ProviderApiKeys, number>,
  currentRevisions: Record<keyof ProviderApiKeys, number>,
): ProviderApiKeys {
  const merged = { ...current };
  for (const result of results) {
    if (result.succeeded && startedRevisions[result.provider] === currentRevisions[result.provider]) {
      merged[result.provider] = result.apiKey;
    }
  }
  return merged;
}

export const DEFAULT_MODEL_CONFIG: ModelConfig = {
  provider: 'builtin-ai',
  model: 'qwen3.5:4b',
  whisperModel: DEFAULT_WHISPER_MODEL,
  ollamaEndpoint: null,
};
