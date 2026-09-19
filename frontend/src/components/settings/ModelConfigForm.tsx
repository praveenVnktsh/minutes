'use client';

import { useEffect, useId, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { CheckCircle2, Download, ExternalLink, Eye, EyeOff, RefreshCw } from 'lucide-react';
import { BuiltInModelManager } from '@/components/BuiltInModelManager';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { SaveFeedback, StatusFeedback } from '@/components/ui/status-feedback';
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

function numberOrNull(value: string): number | null {
  if (!value.trim()) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function messageFrom(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function cachedModelFor(provider: ModelProvider, available: string[]): string {
  if (typeof window === 'undefined') return '';
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem('providerModelMap') || '{}');
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return '';
    const cached = (parsed as Record<string, unknown>)[provider];
    return typeof cached === 'string' && available.includes(cached) ? cached : '';
  } catch {
    return '';
  }
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
  const { isDownloading, getProgress, downloadingModels } = useOllamaDownload();
  const id = useId();
  const initialized = useRef(false);
  const submitting = useRef(false);
  const modelRequest = useRef(0);
  const connectionRequest = useRef(0);
  const draftRef = useRef<ModelConfig | null>(null);
  const draftKeys = useRef<ProviderApiKeys>({ ...providerApiKeys });
  const touchedKeys = useRef(new Set<keyof ProviderApiKeys>());
  const previousDownloads = useRef(new Set(downloadingModels));
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
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [connectionFeedback, setConnectionFeedback] = useState<{ tone: 'success' | 'error'; message: string } | null>(null);
  const [libraryFeedback, setLibraryFeedback] = useState('');
  const [libraryOperation, setLibraryOperation] = useState<string | null>(null);

  draftRef.current = draft;

  useEffect(() => {
    if (!initialized.current && !isModelConfigLoading) {
      initialized.current = true;
      const committed = { ...modelConfig };
      draftKeys.current = { ...providerApiKeys };
      if (KEY_PROVIDERS.has(modelConfig.provider)) {
        const provider = modelConfig.provider as keyof ProviderApiKeys;
        draftKeys.current[provider] = modelConfig.apiKey ?? null;
        touchedKeys.current.add(provider);
      }
      setDraft(committed);
      setBaseline(committed);
      setModels(modelOptions[modelConfig.provider] ?? []);
    }
  }, [isModelConfigLoading, modelConfig, modelOptions, providerApiKeys]);

  useEffect(() => {
    if (!initialized.current || !draft || !baseline || JSON.stringify(draft) !== JSON.stringify(baseline)) return;
    const next = { ...modelConfig };
    if (JSON.stringify(next) !== JSON.stringify(baseline)) {
      if (KEY_PROVIDERS.has(next.provider)) {
        const provider = next.provider as keyof ProviderApiKeys;
        draftKeys.current[provider] = next.apiKey ?? null;
        touchedKeys.current.add(provider);
      }
      setDraft(next);
      setBaseline(next);
      setModels(modelOptions[next.provider] ?? []);
    }
  }, [baseline, draft, modelConfig, modelOptions, providerApiKeys]);

  useEffect(() => {
    for (const provider of Object.keys(providerApiKeys) as Array<keyof ProviderApiKeys>) {
      if (!touchedKeys.current.has(provider)) draftKeys.current[provider] = providerApiKeys[provider];
    }
    if (draft && KEY_PROVIDERS.has(draft.provider) && !touchedKeys.current.has(draft.provider as keyof ProviderApiKeys)) {
      const hydrated = providerApiKeys[draft.provider as keyof ProviderApiKeys];
      if (draft.apiKey !== hydrated) setDraft(current => current ? { ...current, apiKey: hydrated } : current);
    }
  }, [draft, providerApiKeys]);

  useEffect(() => () => {
    modelRequest.current += 1;
    connectionRequest.current += 1;
  }, []);

  const updateDraft = (patch: Partial<ModelConfig>) => {
    if (submitting.current || isModelConfigSaving) return;
    setSaved(false);
    setSaveError('');
    setDraft(current => current ? { ...current, ...patch } : current);
  };

  const loadModels = async (provider: ModelProvider, apiKey?: string | null, endpoint?: string | null) => {
    const request = ++modelRequest.current;
    const identity = provider === 'ollama' ? endpoint?.trim() || '' : apiKey?.trim() || '';
    const isCurrentRequest = () => {
      const current = draftRef.current;
      if (request !== modelRequest.current || current?.provider !== provider) return false;
      return provider === 'ollama'
        ? (current.ollamaEndpoint?.trim() || '') === identity
        : !KEY_PROVIDERS.has(provider) || (current.apiKey?.trim() || '') === identity;
    };
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
      if (!isCurrentRequest()) return;
      setModels(loaded.length ? loaded : FALLBACK_MODELS[provider] ?? modelOptions[provider] ?? []);
    } catch (error) {
      if (!isCurrentRequest()) return;
      setModels(FALLBACK_MODELS[provider] ?? modelOptions[provider] ?? []);
      setModelLoadError(isOllamaNotInstalledError(messageFrom(error))
        ? 'Ollama is not installed or is not running.'
        : `Could not load models: ${messageFrom(error)}`);
    } finally {
      if (isCurrentRequest()) setIsLoadingModels(false);
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

  useEffect(() => {
    const completed = [...previousDownloads.current].some(model => !downloadingModels.has(model));
    previousDownloads.current = new Set(downloadingModels);
    if (completed && draft?.provider === 'ollama') {
      void loadModels('ollama', undefined, draft.ollamaEndpoint);
    }
  // Refreshing is keyed to the shared download lifecycle, not local form lifetime.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [downloadingModels]);

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
  const savePending = isSubmitting || isModelConfigSaving;

  const changeProvider = (provider: ModelProvider) => {
    if (submitting.current || isModelConfigSaving || libraryOperation) return;
    modelRequest.current += 1;
    connectionRequest.current += 1;
    setIsLoadingModels(false);
    setTesting(false);
    setConnectionFeedback(null);
    const available = FALLBACK_MODELS[provider] ?? modelOptions[provider] ?? [];
    const cached = cachedModelFor(provider, available);
    setModels(available);
    setModelLoadError('');
    updateDraft({
      provider,
      apiKey: KEY_PROVIDERS.has(provider) ? draftKeys.current[provider as keyof ProviderApiKeys] : undefined,
      model: cached || available[0] || (provider === 'custom-openai' ? draft.customOpenAIModel ?? '' : ''),
    });
  };

  const save = async () => {
    if (!canSave || submitting.current || isModelConfigSaving) return;
    submitting.current = true;
    setIsSubmitting(true);
    setSaveError('');
    let committedResult: ModelConfig | null = null;
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
      committedResult = committed;
      const committedDraft = { ...committed };
      if (KEY_PROVIDERS.has(committed.provider)) {
        draftKeys.current[committed.provider as keyof ProviderApiKeys] = committedDraft.apiKey ?? null;
      }
      setDraft(committedDraft);
      setBaseline(committedDraft);
      setSaved(true);
    } catch (error) {
      setSaveError(messageFrom(error));
      return;
    } finally {
      submitting.current = false;
      setIsSubmitting(false);
    }
    try {
      if (committedResult) onCommitted?.(committedResult);
    } catch (error) {
      console.error('Model configuration committed, but its completion callback failed:', error);
    }
  };

  const cancel = () => {
    if (baseline) setDraft(baseline);
    setSaveError('');
    setSaved(false);
    onCancel?.();
  };

  const testConnection = async () => {
    const request = ++connectionRequest.current;
    const identity = [draft.customOpenAIEndpoint, draft.customOpenAIApiKey, draft.customOpenAIModel].join('\0');
    setTesting(true);
    setConnectionFeedback(null);
    try {
      const result = await invoke<{ message?: string }>('api_test_custom_openai_connection', {
        endpoint: draft.customOpenAIEndpoint?.trim(),
        apiKey: draft.customOpenAIApiKey?.trim() || null,
        model: draft.customOpenAIModel?.trim(),
      });
      const current = draftRef.current;
      if (request === connectionRequest.current && current && identity === [current.customOpenAIEndpoint, current.customOpenAIApiKey, current.customOpenAIModel].join('\0')) {
        setConnectionFeedback({ tone: 'success', message: result?.message || 'Connection successful' });
      }
    } catch (error) {
      if (request === connectionRequest.current) setConnectionFeedback({ tone: 'error', message: `Connection failed: ${messageFrom(error)}` });
    } finally {
      if (request === connectionRequest.current) setTesting(false);
    }
  };

  const downloadRecommendedOllamaModel = async () => {
    const model = 'gemma3:1b';
    if (isDownloading(model) || submitting.current || isModelConfigSaving) return;
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

  const deleteOllamaModel = async (model: string) => {
    if (libraryOperation || submitting.current || isModelConfigSaving) return;
    setLibraryOperation(model);
    setLibraryFeedback('');
    const endpoint = draft.ollamaEndpoint?.trim() || '';
    try {
      await invoke('delete_ollama_model', { modelName: model, endpoint: endpoint || null });
      const current = draftRef.current;
      if (current?.provider !== 'ollama' || (current.ollamaEndpoint?.trim() || '') !== endpoint) return;
      const remaining = models.filter(item => item !== model);
      setModels(remaining);
      if (draft.model === model) updateDraft({ model: remaining[0] ?? '' });
      setLibraryFeedback(`${model} deleted from Ollama`);
    } catch (error) {
      setLibraryFeedback(`Could not delete ${model}: ${messageFrom(error)}`);
    } finally {
      setLibraryOperation(null);
    }
  };

  const changeConnectionField = (patch: Partial<ModelConfig>) => {
    connectionRequest.current += 1;
    setTesting(false);
    setConnectionFeedback(null);
    updateDraft(patch);
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
        <select id={`${id}-provider`} className={selectClass} value={draft.provider} onChange={event => changeProvider(event.target.value as ModelProvider)} disabled={savePending || Boolean(libraryOperation)}>
          {PROVIDERS.map(provider => <option key={provider.value} value={provider.value}>{provider.label}</option>)}
        </select>
      </div>

      {!isCustom && draft.provider !== 'builtin-ai' && (
        <div className="space-y-2">
          <div className="flex items-center justify-between gap-3">
            <Label htmlFor={`${id}-model`}>Model</Label>
            <Button type="button" size="sm" variant="ghost" disabled={isLoadingModels || savePending} onClick={() => void loadModels(draft.provider, draft.apiKey, draft.ollamaEndpoint)}>
              <RefreshCw className={cn('mr-1.5 h-3.5 w-3.5', isLoadingModels && 'animate-spin')} /> Refresh
            </Button>
          </div>
          <select id={`${id}-model`} className={selectClass} value={draft.model} onChange={event => updateDraft({ model: event.target.value })} disabled={isLoadingModels || savePending}>
            {!models.includes(draft.model) && draft.model && <option value={draft.model}>{draft.model}</option>}
            {models.map(model => <option key={model} value={model}>{model}</option>)}
          </select>
        </div>
      )}

      {draft.provider === 'builtin-ai' && (
        <div className={cn(savePending && 'pointer-events-none opacity-60')} aria-disabled={savePending}>
          <BuiltInModelManager selectedModel={draft.model} layout={layout} onModelSelect={model => updateDraft({ model })} />
        </div>
      )}

      {requiresApiKey && (
        <div className="space-y-2">
          <Label htmlFor={`${id}-api-key`}>{PROVIDERS.find(item => item.value === draft.provider)?.label} API key</Label>
          <div className="flex gap-2">
            <Input id={`${id}-api-key`} type={showApiKey ? 'text' : 'password'} value={draft.apiKey ?? ''} onChange={event => {
              const provider = draft.provider as keyof ProviderApiKeys;
              modelRequest.current += 1;
              setIsLoadingModels(false);
              touchedKeys.current.add(provider);
              draftKeys.current[provider] = event.target.value;
              updateDraft({ apiKey: event.target.value });
            }} autoComplete="off" disabled={savePending} />
            <Button type="button" variant="outline" size="icon" aria-label={showApiKey ? 'Hide API key' : 'Show API key'} onClick={() => setShowApiKey(value => !value)}>
              {showApiKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
            </Button>
          </div>
        </div>
      )}

      {draft.provider === 'ollama' && (
        <div className="space-y-2">
          <Label htmlFor={`${id}-ollama-endpoint`}>Ollama endpoint</Label>
          <Input id={`${id}-ollama-endpoint`} type="url" value={draft.ollamaEndpoint ?? ''} placeholder="http://localhost:11434" onChange={event => {
            modelRequest.current += 1;
            setIsLoadingModels(false);
            setModels([]);
            setModelLoadError('');
            updateDraft({ ollamaEndpoint: event.target.value });
          }} disabled={savePending} />
          <p className="text-xs text-ink-muted">Leave empty to use the local default endpoint, then refresh the model list.</p>
          {!isLoadingModels && models.length === 0 && (
            <div className="space-y-2 rounded-lg border border-info bg-info-subtle p-3">
              <p className="text-sm text-info">No Ollama models were found at this endpoint.</p>
              <div className="flex flex-wrap gap-2">
                <Button type="button" size="sm" variant="outline" onClick={() => void downloadRecommendedOllamaModel()} disabled={isDownloading('gemma3:1b') || savePending}>
                  <Download className="mr-2 h-4 w-4" />
                  {isDownloading('gemma3:1b') ? `Downloading ${Math.round(getProgress('gemma3:1b') ?? 0)}%` : 'Download gemma3:1b'}
                </Button>
                <Button type="button" size="sm" variant="outline" onClick={() => void invoke('open_external_url', { url: 'https://ollama.com/download' })}>
                  <ExternalLink className="mr-2 h-4 w-4" />Install Ollama
                </Button>
              </div>
            </div>
          )}
          {models.length > 0 && (
            <div className="space-y-2 pt-2" aria-label="Installed Ollama models">
              <p className="text-xs text-ink-muted">Deleting or downloading changes the local model library immediately. Model selection remains a draft until Save.</p>
              {models.map(model => (
                <div key={model} className="flex items-center justify-between gap-3 rounded-md border border-hairline bg-surface-2 px-3 py-2">
                  <button type="button" className="min-w-0 flex-1 truncate text-left text-sm" disabled={savePending} onClick={() => updateDraft({ model })}>{model}</button>
                  <Button type="button" size="sm" variant="destructive" aria-label={`Delete ${model} from Ollama`} disabled={Boolean(libraryOperation) || savePending} onClick={() => void deleteOllamaModel(model)}>
                    {libraryOperation === model ? 'Deleting…' : 'Delete'}
                  </Button>
                </div>
              ))}
            </div>
          )}
          {libraryFeedback && <StatusFeedback tone={libraryFeedback.startsWith('Could not') ? 'error' : 'success'}>{libraryFeedback}</StatusFeedback>}
        </div>
      )}

      {isCustom && (
        <div className="space-y-4 rounded-lg border border-hairline p-4">
          <div className="space-y-2">
            <Label htmlFor={`${id}-custom-endpoint`}>Endpoint URL</Label>
            <Input id={`${id}-custom-endpoint`} type="url" value={draft.customOpenAIEndpoint ?? ''} placeholder="http://localhost:8000/v1" onChange={event => changeConnectionField({ customOpenAIEndpoint: event.target.value })} disabled={savePending} />
          </div>
          <div className="space-y-2">
            <Label htmlFor={`${id}-custom-model`}>Model name</Label>
            <Input id={`${id}-custom-model`} value={draft.customOpenAIModel ?? ''} onChange={event => changeConnectionField({ customOpenAIModel: event.target.value, model: event.target.value })} disabled={savePending} />
          </div>
          <div className="space-y-2">
            <Label htmlFor={`${id}-custom-key`}>API key (optional)</Label>
            <Input id={`${id}-custom-key`} type="password" value={draft.customOpenAIApiKey ?? ''} autoComplete="off" onChange={event => changeConnectionField({ customOpenAIApiKey: event.target.value })} disabled={savePending} />
          </div>
          <Button type="button" variant="outline" size="sm" onClick={() => void testConnection()} disabled={testing || savePending || !draft.customOpenAIEndpoint?.trim() || !draft.customOpenAIModel?.trim()}>
            {testing ? <RefreshCw className="mr-2 h-4 w-4 animate-spin" /> : <CheckCircle2 className="mr-2 h-4 w-4" />}
            {testing ? 'Testing…' : 'Test connection'}
          </Button>
          {connectionFeedback && <StatusFeedback tone={connectionFeedback.tone}>{connectionFeedback.message}</StatusFeedback>}
          <button type="button" disabled={savePending} className="block text-sm font-medium text-ink underline-offset-4 hover:underline disabled:opacity-50" aria-expanded={advancedOpen} onClick={() => setAdvancedOpen(value => !value)}>
            Advanced options
          </button>
          {advancedOpen && (
            <div className="grid gap-4 sm:grid-cols-3">
              <div className="space-y-2"><Label htmlFor={`${id}-max-tokens`}>Max tokens</Label><Input id={`${id}-max-tokens`} type="number" min="1" value={draft.maxTokens ?? ''} disabled={savePending} onChange={event => updateDraft({ maxTokens: numberOrNull(event.target.value) })} /></div>
              <div className="space-y-2"><Label htmlFor={`${id}-temperature`}>Temperature</Label><Input id={`${id}-temperature`} type="number" min="0" max="2" step="0.1" value={draft.temperature ?? ''} disabled={savePending} onChange={event => updateDraft({ temperature: numberOrNull(event.target.value) })} /></div>
              <div className="space-y-2"><Label htmlFor={`${id}-top-p`}>Top P</Label><Input id={`${id}-top-p`} type="number" min="0" max="1" step="0.1" value={draft.topP ?? ''} disabled={savePending} onChange={event => updateDraft({ topP: numberOrNull(event.target.value) })} /></div>
            </div>
          )}
        </div>
      )}

      {modelLoadError && <Alert variant="destructive"><AlertDescription>{modelLoadError}</AlertDescription></Alert>}
      {saveError && <Alert variant="destructive"><AlertDescription>Could not save model settings: {saveError}</AlertDescription></Alert>}

      <div className="flex flex-wrap items-center justify-between gap-3 border-t border-hairline pt-4">
        <SaveFeedback state={savePending ? 'saving' : saveError ? 'error' : saved ? 'saved' : dirty ? 'unsaved' : 'saved'} labels={{ saved: saved ? 'Model settings saved' : 'No unsaved changes', error: 'Model settings were not saved' }} />
        <div className="flex gap-2">
          {showCancel && <Button type="button" variant="outline" onClick={cancel} disabled={savePending}>Cancel</Button>}
          <Button type="submit" disabled={!canSave || !dirty || savePending}>{savePending ? 'Saving…' : 'Save'}</Button>
        </div>
      </div>
    </form>
  );
}
