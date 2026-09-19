'use client';

import { useEffect, useId, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { CheckCircle2, Download, ExternalLink, Eye, EyeOff, RefreshCw } from 'lucide-react';
import { BuiltInModelManager } from '@/components/BuiltInModelManager';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { SaveFeedback } from '@/components/ui/status-feedback';
import { useConfig } from '@/contexts/ConfigContext';
import { useOllamaDownload } from '@/contexts/OllamaDownloadContext';
import type { ModelConfig, ModelProvider, ProviderApiKeys } from '@/types/modelConfig';
import { cn, isOllamaNotInstalledError } from '@/lib/utils';

interface RemoteModel {
  id?: string;
  name?: string;
  display_name?: string;
}

export interface ModelConfigFormProps {
  layout?: 'inline' | 'dialog';
  onCommitted?: (config: ModelConfig) => void;
  onCancel?: () => void;
  showCancel?: boolean;
}

const PROVIDERS: Array<{ value: ModelProvider; label: string }> = [
  { value: 'builtin-ai', label: 'Built-in AI (offline)' },
  { value: 'claude', label: 'Claude' },
  { value: 'custom-openai', label: 'Custom OpenAI-compatible server' },
  { value: 'groq', label: 'Groq' },
  { value: 'ollama', label: 'Ollama' },
  { value: 'openai', label: 'OpenAI' },
  { value: 'openrouter', label: 'OpenRouter' },
];

const FALLBACK_MODELS: Partial<Record<ModelProvider, string[]>> = {
  claude: ['claude-sonnet-4-5-20250929', 'claude-haiku-4-5-20251001', 'claude-3-5-sonnet-latest'],
  groq: ['llama-3.3-70b-versatile', 'llama-3.1-70b-versatile', 'mixtral-8x7b-32768'],
  openai: ['gpt-4o', 'gpt-4o-mini', 'gpt-4-turbo', 'gpt-4'],
};

const KEY_PROVIDERS = new Set<ModelProvider>(['claude', 'groq', 'openai', 'openrouter']);

function providerKey(provider: ModelProvider, keys: ProviderApiKeys): string | null | undefined {
  return KEY_PROVIDERS.has(provider) ? keys[provider as keyof ProviderApiKeys] : undefined;
}

function numberOrNull(value: string): number | null {
  if (!value.trim()) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function messageFrom(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function ModelConfigForm({
  layout = 'inline',
  onCommitted,
  onCancel,
  showCancel = false,
}: ModelConfigFormProps) {
  const {
    modelConfig,
    commitModelConfig,
    isModelConfigLoading,
    isModelConfigSaving,
    providerApiKeys,
    modelOptions,
  } = useConfig();
  const { isDownloading, getProgress } = useOllamaDownload();
  const id = useId();
  const initialized = useRef(false);
  const submitting = useRef(false);
  const [draft, setDraft] = useState<ModelConfig | null>(null);
  const [baseline, setBaseline] = useState<ModelConfig | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const [isLoadingModels, setIsLoadingModels] = useState(false);
  const [modelLoadError, setModelLoadError] = useState('');
  const [saveError, setSaveError] = useState('');
  const [saved, setSaved] = useState(false);
  const [showApiKey, setShowApiKey] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [testing, setTesting] = useState(false);

  useEffect(() => {
    if (!initialized.current && !isModelConfigLoading) {
      initialized.current = true;
      const committed = {
        ...modelConfig,
        apiKey: providerKey(modelConfig.provider, providerApiKeys) ?? modelConfig.apiKey,
      };
      setDraft(committed);
      setBaseline(committed);
      setModels(modelOptions[modelConfig.provider] ?? []);
    }
  }, [isModelConfigLoading, modelConfig, modelOptions, providerApiKeys]);

  useEffect(() => {
    if (!initialized.current || !draft || !baseline || JSON.stringify(draft) !== JSON.stringify(baseline)) return;
    const next = {
      ...modelConfig,
      apiKey: providerKey(modelConfig.provider, providerApiKeys) ?? modelConfig.apiKey,
    };
    if (JSON.stringify(next) !== JSON.stringify(baseline)) {
      setDraft(next);
      setBaseline(next);
      setModels(modelOptions[next.provider] ?? []);
    }
  }, [baseline, draft, modelConfig, modelOptions, providerApiKeys]);

  const updateDraft = (patch: Partial<ModelConfig>) => {
    setSaved(false);
    setSaveError('');
    setDraft(current => current ? { ...current, ...patch } : current);
  };

  const loadModels = async (provider: ModelProvider, apiKey?: string | null, endpoint?: string | null) => {
    setIsLoadingModels(true);
    setModelLoadError('');
    try {
      let result: RemoteModel[] = [];
      if (provider === 'ollama') {
        result = await invoke<RemoteModel[]>('get_ollama_models', { endpoint: endpoint?.trim() || null });
      } else if (provider === 'openrouter') {
        result = await invoke<RemoteModel[]>('get_openrouter_models');
      } else if (provider === 'openai' && apiKey?.trim()) {
        result = await invoke<RemoteModel[]>('get_openai_models', { apiKey });
      } else if (provider === 'claude' && apiKey?.trim()) {
        result = await invoke<RemoteModel[]>('get_anthropic_models', { apiKey });
      } else if (provider === 'groq' && apiKey?.trim()) {
        result = await invoke<RemoteModel[]>('get_groq_models', { apiKey });
      }
      const loaded = result.map(item => item.id || item.name).filter((value): value is string => Boolean(value));
      setModels(loaded.length ? loaded : FALLBACK_MODELS[provider] ?? modelOptions[provider] ?? []);
    } catch (error) {
      setModels(FALLBACK_MODELS[provider] ?? modelOptions[provider] ?? []);
      setModelLoadError(isOllamaNotInstalledError(messageFrom(error))
        ? 'Ollama is not installed or is not running.'
        : `Could not load models: ${messageFrom(error)}`);
    } finally {
      setIsLoadingModels(false);
    }
  };

  useEffect(() => {
    if (!draft) return;
    const fallback = FALLBACK_MODELS[draft.provider] ?? modelOptions[draft.provider] ?? [];
    setModels(current => current.length ? current : fallback);
    if (draft.provider === 'ollama' || draft.provider === 'openrouter') {
      void loadModels(draft.provider, draft.apiKey, draft.ollamaEndpoint);
    }
  // The draft is initialized only once; subsequent edits must never be reset by async model loading.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft?.provider]);

  if (!draft) {
    return <div role="status" className="py-8 text-sm text-ink-muted">Loading model settings…</div>;
  }

  const requiresApiKey = KEY_PROVIDERS.has(draft.provider);
  const isCustom = draft.provider === 'custom-openai';
  const canSave = Boolean(
    (isCustom ? draft.customOpenAIEndpoint?.trim() && draft.customOpenAIModel?.trim() : draft.model.trim())
    && (!requiresApiKey || draft.apiKey?.trim())
  );
  const dirty = JSON.stringify(draft) !== JSON.stringify(baseline);

  const changeProvider = (provider: ModelProvider) => {
    const available = FALLBACK_MODELS[provider] ?? modelOptions[provider] ?? [];
    const cached = typeof window === 'undefined'
      ? ''
      : JSON.parse(localStorage.getItem('providerModelMap') || '{}')[provider] as string | undefined;
    setModels(available);
    setModelLoadError('');
    updateDraft({
      provider,
      apiKey: providerKey(provider, providerApiKeys),
      model: cached && available.includes(cached) ? cached : available[0] ?? (provider === 'custom-openai' ? draft.customOpenAIModel ?? '' : ''),
    });
  };

  const save = async () => {
    if (!canSave || submitting.current) return;
    submitting.current = true;
    setSaveError('');
    try {
      const toCommit: ModelConfig = {
        ...draft,
        model: isCustom ? draft.customOpenAIModel?.trim() ?? '' : draft.model.trim(),
        apiKey: draft.apiKey === undefined ? undefined : draft.apiKey?.trim() || null,
        ollamaEndpoint: draft.ollamaEndpoint?.trim() || null,
        customOpenAIEndpoint: draft.customOpenAIEndpoint?.trim() || null,
        customOpenAIModel: draft.customOpenAIModel?.trim() || null,
        customOpenAIApiKey: draft.customOpenAIApiKey?.trim() || null,
      };
      const committed = await commitModelConfig(toCommit);
      const committedDraft = {
        ...committed,
        apiKey: committed.apiKey !== undefined
          ? committed.apiKey
          : providerKey(committed.provider, providerApiKeys),
      };
      setDraft(committedDraft);
      setBaseline(committedDraft);
      setSaved(true);
      onCommitted?.(committed);
    } catch (error) {
      setSaveError(messageFrom(error));
    } finally {
      submitting.current = false;
    }
  };

  const cancel = () => {
    if (baseline) setDraft(baseline);
    setSaveError('');
    setSaved(false);
    onCancel?.();
  };

  const testConnection = async () => {
    setTesting(true);
    setModelLoadError('');
    try {
      await invoke('api_test_custom_openai_connection', {
        endpoint: draft.customOpenAIEndpoint?.trim(),
        apiKey: draft.customOpenAIApiKey?.trim() || null,
        model: draft.customOpenAIModel?.trim(),
      });
    } catch (error) {
      setModelLoadError(`Connection failed: ${messageFrom(error)}`);
    } finally {
      setTesting(false);
    }
  };

  const downloadRecommendedOllamaModel = async () => {
    const model = 'gemma3:1b';
    if (isDownloading(model)) return;
    setModelLoadError('');
    try {
      await invoke('pull_ollama_model', {
        modelName: model,
        endpoint: draft.ollamaEndpoint?.trim() || null,
      });
      await loadModels('ollama', undefined, draft.ollamaEndpoint);
    } catch (error) {
      setModelLoadError(`Could not download ${model}: ${messageFrom(error)}`);
    }
  };

  const selectClass = 'h-10 w-full rounded-md border border-input bg-surface-raised px-3 text-sm text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus';

  return (
    <form className={cn('space-y-5', layout === 'dialog' && 'max-h-[70vh] overflow-y-auto pr-1')} onSubmit={event => { event.preventDefault(); void save(); }}>
      <div>
        <h3 className="text-lg font-semibold text-ink">Model settings</h3>
        <p className="mt-1 text-sm text-ink-muted">Changes apply only after Save completes.</p>
      </div>

      <div className="space-y-2">
        <Label htmlFor={`${id}-provider`}>Provider</Label>
        <select id={`${id}-provider`} className={selectClass} value={draft.provider} onChange={event => changeProvider(event.target.value as ModelProvider)} disabled={isModelConfigSaving}>
          {PROVIDERS.map(provider => <option key={provider.value} value={provider.value}>{provider.label}</option>)}
        </select>
      </div>

      {!isCustom && draft.provider !== 'builtin-ai' && (
        <div className="space-y-2">
          <div className="flex items-center justify-between gap-3">
            <Label htmlFor={`${id}-model`}>Model</Label>
            <Button type="button" size="sm" variant="ghost" disabled={isLoadingModels} onClick={() => void loadModels(draft.provider, draft.apiKey, draft.ollamaEndpoint)}>
              <RefreshCw className={cn('mr-1.5 h-3.5 w-3.5', isLoadingModels && 'animate-spin')} /> Refresh
            </Button>
          </div>
          <select id={`${id}-model`} className={selectClass} value={draft.model} onChange={event => updateDraft({ model: event.target.value })} disabled={isLoadingModels || isModelConfigSaving}>
            {!models.includes(draft.model) && draft.model && <option value={draft.model}>{draft.model}</option>}
            {models.map(model => <option key={model} value={model}>{model}</option>)}
          </select>
        </div>
      )}

      {draft.provider === 'builtin-ai' && (
        <BuiltInModelManager selectedModel={draft.model} layout={layout} onModelSelect={model => updateDraft({ model })} />
      )}

      {requiresApiKey && (
        <div className="space-y-2">
          <Label htmlFor={`${id}-api-key`}>{PROVIDERS.find(item => item.value === draft.provider)?.label} API key</Label>
          <div className="flex gap-2">
            <Input id={`${id}-api-key`} type={showApiKey ? 'text' : 'password'} value={draft.apiKey ?? ''} onChange={event => updateDraft({ apiKey: event.target.value })} autoComplete="off" disabled={isModelConfigSaving} />
            <Button type="button" variant="outline" size="icon" aria-label={showApiKey ? 'Hide API key' : 'Show API key'} onClick={() => setShowApiKey(value => !value)}>
              {showApiKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
            </Button>
          </div>
        </div>
      )}

      {draft.provider === 'ollama' && (
        <div className="space-y-2">
          <Label htmlFor={`${id}-ollama-endpoint`}>Ollama endpoint</Label>
          <Input id={`${id}-ollama-endpoint`} type="url" value={draft.ollamaEndpoint ?? ''} placeholder="http://localhost:11434" onChange={event => updateDraft({ ollamaEndpoint: event.target.value })} disabled={isModelConfigSaving} />
          <p className="text-xs text-ink-muted">Leave empty to use the local default endpoint, then refresh the model list.</p>
          {!isLoadingModels && models.length === 0 && (
            <div className="space-y-2 rounded-lg border border-info bg-info-subtle p-3">
              <p className="text-sm text-info">No Ollama models were found at this endpoint.</p>
              <div className="flex flex-wrap gap-2">
                <Button type="button" size="sm" variant="outline" onClick={() => void downloadRecommendedOllamaModel()} disabled={isDownloading('gemma3:1b')}>
                  <Download className="mr-2 h-4 w-4" />
                  {isDownloading('gemma3:1b') ? `Downloading ${Math.round(getProgress('gemma3:1b') ?? 0)}%` : 'Download gemma3:1b'}
                </Button>
                <Button type="button" size="sm" variant="outline" onClick={() => void invoke('open_external_url', { url: 'https://ollama.com/download' })}>
                  <ExternalLink className="mr-2 h-4 w-4" />Install Ollama
                </Button>
              </div>
            </div>
          )}
        </div>
      )}

      {isCustom && (
        <div className="space-y-4 rounded-lg border border-hairline p-4">
          <div className="space-y-2">
            <Label htmlFor={`${id}-custom-endpoint`}>Endpoint URL</Label>
            <Input id={`${id}-custom-endpoint`} type="url" value={draft.customOpenAIEndpoint ?? ''} placeholder="http://localhost:8000/v1" onChange={event => updateDraft({ customOpenAIEndpoint: event.target.value })} disabled={isModelConfigSaving} />
          </div>
          <div className="space-y-2">
            <Label htmlFor={`${id}-custom-model`}>Model name</Label>
            <Input id={`${id}-custom-model`} value={draft.customOpenAIModel ?? ''} onChange={event => updateDraft({ customOpenAIModel: event.target.value, model: event.target.value })} disabled={isModelConfigSaving} />
          </div>
          <div className="space-y-2">
            <Label htmlFor={`${id}-custom-key`}>API key (optional)</Label>
            <Input id={`${id}-custom-key`} type="password" value={draft.customOpenAIApiKey ?? ''} autoComplete="off" onChange={event => updateDraft({ customOpenAIApiKey: event.target.value })} disabled={isModelConfigSaving} />
          </div>
          <Button type="button" variant="outline" size="sm" onClick={() => void testConnection()} disabled={testing || !draft.customOpenAIEndpoint?.trim() || !draft.customOpenAIModel?.trim()}>
            {testing ? <RefreshCw className="mr-2 h-4 w-4 animate-spin" /> : <CheckCircle2 className="mr-2 h-4 w-4" />}
            {testing ? 'Testing…' : 'Test connection'}
          </Button>
          <button type="button" className="block text-sm font-medium text-ink underline-offset-4 hover:underline" aria-expanded={advancedOpen} onClick={() => setAdvancedOpen(value => !value)}>
            Advanced options
          </button>
          {advancedOpen && (
            <div className="grid gap-4 sm:grid-cols-3">
              <div className="space-y-2"><Label htmlFor={`${id}-max-tokens`}>Max tokens</Label><Input id={`${id}-max-tokens`} type="number" min="1" value={draft.maxTokens ?? ''} onChange={event => updateDraft({ maxTokens: numberOrNull(event.target.value) })} /></div>
              <div className="space-y-2"><Label htmlFor={`${id}-temperature`}>Temperature</Label><Input id={`${id}-temperature`} type="number" min="0" max="2" step="0.1" value={draft.temperature ?? ''} onChange={event => updateDraft({ temperature: numberOrNull(event.target.value) })} /></div>
              <div className="space-y-2"><Label htmlFor={`${id}-top-p`}>Top P</Label><Input id={`${id}-top-p`} type="number" min="0" max="1" step="0.1" value={draft.topP ?? ''} onChange={event => updateDraft({ topP: numberOrNull(event.target.value) })} /></div>
            </div>
          )}
        </div>
      )}

      {modelLoadError && <Alert variant="destructive"><AlertDescription>{modelLoadError}</AlertDescription></Alert>}
      {saveError && <Alert variant="destructive"><AlertDescription>Could not save model settings: {saveError}</AlertDescription></Alert>}

      <div className="flex flex-wrap items-center justify-between gap-3 border-t border-hairline pt-4">
        <SaveFeedback state={isModelConfigSaving ? 'saving' : saveError ? 'error' : saved ? 'saved' : dirty ? 'unsaved' : 'saved'} labels={{ saved: saved ? 'Model settings saved' : 'No unsaved changes', error: 'Model settings were not saved' }} />
        <div className="flex gap-2">
          {showCancel && <Button type="button" variant="outline" onClick={cancel} disabled={isModelConfigSaving}>Cancel</Button>}
          <Button type="submit" disabled={!canSave || !dirty || isModelConfigSaving}>{isModelConfigSaving ? 'Saving…' : 'Save'}</Button>
        </div>
      </div>
    </form>
  );
}
