'use client';

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Copy, RefreshCw, Send } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { SaveFeedback, type SaveFeedbackState } from '@/components/ui/status-feedback';

interface WebhookConfig {
  enabled: boolean;
  endpoint: string;
  signing_secret: string;
}

const EMPTY_CONFIG: WebhookConfig = {
  enabled: false,
  endpoint: '',
  signing_secret: '',
};

export function WebhookSettings() {
  const [config, setConfig] = useState<WebhookConfig>(EMPTY_CONFIG);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [savedConfig, setSavedConfig] = useState<WebhookConfig>(EMPTY_CONFIG);
  const [saveState, setSaveState] = useState<SaveFeedbackState>('saved');

  useEffect(() => {
    invoke<WebhookConfig>('get_webhook_config')
      .then(value => { setConfig(value); setSavedConfig(value); })
      .catch((error) => toast.error('Could not load webhook settings', {
        description: String(error),
      }))
      .finally(() => setLoading(false));
  }, []);

  const save = async (showToast = true) => {
    setSaving(true);
    setSaveState('saving');
    try {
      await invoke('set_webhook_config', { config });
      setSavedConfig(config);
      setSaveState('saved');
      if (showToast) toast.success('Webhook settings saved');
      return true;
    } catch (error) {
      setSaveState('error');
      toast.error('Could not save webhook settings', { description: String(error) });
      return false;
    } finally {
      setSaving(false);
    }
  };

  const testWebhook = async () => {
    setTesting(true);
    try {
      if (!await save(false)) return;
      await invoke('test_webhook');
      toast.success('Test webhook delivered');
    } catch (error) {
      toast.error('Test webhook failed', { description: String(error) });
    } finally {
      setTesting(false);
    }
  };

  const generateSecret = () => {
    const bytes = crypto.getRandomValues(new Uint8Array(32));
    const secret = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
    setConfig(current => ({ ...current, signing_secret: secret }));
  };

  const copySecret = async () => {
    try {
      await navigator.clipboard.writeText(config.signing_secret);
      toast.success('Signing secret copied');
    } catch (error) {
      toast.error('Could not copy signing secret', { description: String(error) });
    }
  };

  if (loading) {
    return <div className="mt-6 h-40 animate-pulse rounded-lg bg-surface-2" />;
  }

  return (
    <div className="mt-6 max-w-3xl space-y-6">
      <div>
        <h2 className="text-xl font-semibold">Transcription webhook</h2>
        <p className="mt-1 text-sm text-ink-muted">
          Notify your integration only after a meeting transcript has been finalized. Recording
          completion alone never sends a webhook.
        </p>
      </div>

      <div className="flex items-center justify-between rounded-lg border bg-surface-raised p-4">
        <div className="pr-4">
          <div className="font-medium">Enable completion webhook</div>
          <div className="text-sm text-ink-muted">
            Failed deliveries remain in a local outbox and retry automatically.
          </div>
        </div>
        <Switch
          aria-label="Enable transcription completion webhook"
          checked={config.enabled}
          onCheckedChange={enabled => setConfig(current => ({ ...current, enabled }))}
        />
      </div>

      <div className="space-y-2">
        <label htmlFor="webhook-endpoint" className="text-sm font-medium">Endpoint URL</label>
        <input
          id="webhook-endpoint"
          type="url"
          value={config.endpoint}
          onChange={event => setConfig(current => ({ ...current, endpoint: event.target.value }))}
          placeholder="https://hooks.example.com/minutes"
          className="w-full rounded-md border border-hairline bg-surface-raised px-3 py-2 text-sm outline-none focus:border-focus focus:ring-2 focus:ring-focus/30"
        />
        <p className="text-xs text-ink-muted">HTTPS is required, except for localhost development.</p>
      </div>

      <div className="space-y-2">
        <label htmlFor="webhook-secret" className="text-sm font-medium">Signing secret</label>
        <div className="flex gap-2">
          <input
            id="webhook-secret"
            type="password"
            value={config.signing_secret}
            onChange={event => setConfig(current => ({ ...current, signing_secret: event.target.value }))}
            placeholder="At least 16 characters"
            className="min-w-0 flex-1 rounded-md border border-hairline bg-surface-raised px-3 py-2 text-sm outline-none focus:border-focus focus:ring-2 focus:ring-focus/30"
          />
          <Button variant="outline" onClick={generateSecret} title="Generate secret">
            <RefreshCw className="h-4 w-4" />
          </Button>
          <Button
            variant="outline"
            onClick={copySecret}
            disabled={!config.signing_secret}
            title="Copy secret"
          >
            <Copy className="h-4 w-4" />
          </Button>
        </div>
        <p className="text-xs text-ink-muted">
          Verify the <code>x-meetily-signature</code> HMAC-SHA256 header in your receiver.
        </p>
      </div>

      <div className="rounded-lg border bg-surface-2 p-4 text-sm text-ink">
        Payloads contain meeting metadata and a Minutes path, but no transcript or audio. Calendar
        and external-ledger IDs are reserved as nullable fields for the later integration.
      </div>

      <div className="flex gap-3">
        <Button onClick={() => save()} disabled={saving || testing}>
          {saving ? 'Saving…' : 'Save settings'}
        </Button>
        <Button variant="outline" onClick={testWebhook} disabled={saving || testing || !config.enabled}>
          <Send className="mr-2 h-4 w-4" />
          {testing ? 'Sending…' : 'Send test webhook'}
        </Button>
      </div>
      <SaveFeedback
        state={saving ? 'saving' : saveState === 'error' ? 'error' : JSON.stringify(config) === JSON.stringify(savedConfig) ? 'saved' : 'unsaved'}
        labels={{ saving: 'Saving webhook settings', saved: 'Webhook settings saved', error: 'Could not save webhook settings', unsaved: 'Webhook changes not saved' }}
      />
    </div>
  );
}
