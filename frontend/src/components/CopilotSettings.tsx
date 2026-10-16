'use client';

import { useEffect, useId, useState } from 'react';
import { toast } from 'sonner';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { copilotApi, type CopilotSettings as Settings } from '@/lib/copilot';

const PROVIDERS = [
  { value: '', label: 'Same as the summary model' },
  { value: 'builtin-ai', label: 'Built-in AI (offline)' },
  { value: 'claude', label: 'Claude' },
  { value: 'custom-openai', label: 'Custom OpenAI-compatible server' },
  { value: 'groq', label: 'Groq' },
  { value: 'ollama', label: 'Ollama' },
  { value: 'openai', label: 'OpenAI' },
  { value: 'openrouter', label: 'OpenRouter' },
];

const MODEL_HINTS: Record<string, string> = {
  'builtin-ai': 'qwen3.5:4b',
  claude: 'claude-haiku-4-5-20251001',
  groq: 'llama-3.3-70b-versatile',
  openai: 'gpt-4o-mini',
  ollama: 'qwen3:4b',
};

const fieldClass = 'h-10 w-full rounded-md border border-input bg-surface-raised px-3 text-sm text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus';

/** Settings → Summary → Copilot: suggestions, its own model, the debug line. */
export function CopilotSettings() {
  const id = useId();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [provider, setProvider] = useState('');
  const [model, setModel] = useState('');
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let disposed = false;
    copilotApi.getSettings()
      .then((loaded) => {
        if (disposed) return;
        setSettings(loaded);
        setProvider(loaded.model?.provider ?? '');
        setModel(loaded.model?.model ?? '');
      })
      .catch((error) => {
        console.warn('Could not load the Copilot settings:', error);
        if (!disposed) setSettings({ suggestionsEnabled: true, debug: false, model: null });
      });
    return () => { disposed = true; };
  }, []);

  const save = async (next: Settings) => {
    const previous = settings;
    setSettings(next);
    setSaving(true);
    try {
      setSettings(await copilotApi.setSettings(next));
    } catch (error) {
      setSettings(previous);
      toast.error('Could not save the Copilot settings', { description: String(error) });
    } finally {
      setSaving(false);
    }
  };

  const saveModel = (nextProvider: string, nextModel: string) => {
    if (!settings) return;
    const choice = nextProvider && nextModel.trim() ? { provider: nextProvider, model: nextModel.trim() } : null;
    if (JSON.stringify(choice) === JSON.stringify(settings.model)) return;
    void save({ ...settings, model: choice });
  };

  const disabled = settings === null || saving;

  return (
    <div className="space-y-5 rounded-lg border border-hairline bg-surface-raised p-6 shadow-sm">
      <div>
        <h3 className="mb-2 text-lg font-semibold text-ink">Copilot</h3>
        <p className="text-sm text-ink-muted">
          While recording, the dock&apos;s Chat panel becomes Copilot: ask about the last few minutes, and get a suggested reply
          when someone asks you something. Answers are saved to the meeting&apos;s AI chat.
        </p>
      </div>

      <div className="flex items-center justify-between gap-4">
        <div>
          <p className="text-sm font-medium text-ink">Suggestions</p>
          <p id={`${id}-suggestions`} className="text-xs text-ink-muted">
            When someone else asks you a question, draft a reply. Your own lines never trigger it.
          </p>
        </div>
        <Switch
          aria-label="Copilot suggestions"
          aria-describedby={`${id}-suggestions`}
          checked={settings?.suggestionsEnabled ?? true}
          disabled={disabled}
          onCheckedChange={(checked) => settings && void save({ ...settings, suggestionsEnabled: checked })}
        />
      </div>

      <div className="space-y-2">
        <Label htmlFor={`${id}-provider`}>Copilot model</Label>
        <select
          id={`${id}-provider`}
          className={fieldClass}
          value={provider}
          disabled={disabled}
          onChange={(event) => {
            const next = event.target.value;
            const nextModel = next ? (MODEL_HINTS[next] ?? '') : '';
            setProvider(next);
            setModel(nextModel);
            saveModel(next, nextModel);
          }}
        >
          {PROVIDERS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
        </select>
        {provider && (
          <input
            aria-label="Copilot model name"
            className={fieldClass}
            value={model}
            disabled={disabled}
            placeholder="Model name"
            onChange={(event) => setModel(event.target.value)}
            onBlur={() => saveModel(provider, model)}
          />
        )}
        <p className="text-xs text-ink-muted">
          Uses the API keys and endpoints set for the summary model. A small, fast model works best. A different built-in
          model than the summary&apos;s makes the app switch models during the meeting.
        </p>
      </div>

      <div className="flex items-center justify-between gap-4">
        <div>
          <p className="text-sm font-medium text-ink">Show Copilot debug line</p>
          <p id={`${id}-debug`} className="text-xs text-ink-muted">
            For developers: first-word latency, speed, reused prompt tokens and detector decisions under the Copilot input.
          </p>
        </div>
        <Switch
          aria-label="Show Copilot debug line"
          aria-describedby={`${id}-debug`}
          checked={settings?.debug ?? false}
          disabled={disabled}
          onCheckedChange={(checked) => settings && void save({ ...settings, debug: checked })}
        />
      </div>
    </div>
  );
}
