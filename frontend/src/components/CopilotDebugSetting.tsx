'use client';

import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Switch } from '@/components/ui/switch';
import { copilotApi, type CopilotSettings } from '@/lib/copilot';

/** Settings → Developer: the Copilot debug line under the Copilot input. */
export function CopilotDebugSetting() {
  const [settings, setSettings] = useState<CopilotSettings | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let disposed = false;
    copilotApi.getSettings()
      .then((loaded) => { if (!disposed) setSettings(loaded); })
      .catch((error) => console.warn('Could not load the Copilot settings:', error));
    return () => { disposed = true; };
  }, []);

  const change = async (debug: boolean) => {
    if (!settings) return;
    const previous = settings;
    setSettings({ ...settings, debug });
    setSaving(true);
    try {
      // Re-read first so the save never overwrites a change made elsewhere.
      const latest = await copilotApi.getSettings();
      setSettings(await copilotApi.setSettings({ ...latest, debug }));
    } catch (error) {
      setSettings(previous);
      toast.error('Could not save the Copilot settings', { description: String(error) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="flex items-center justify-between rounded-lg border border-hairline p-4">
      <div className="flex-1 pr-4">
        <div className="font-medium">Show Copilot debug line</div>
        <div className="text-sm text-ink-muted">
          First-word latency, speed, reused prompt tokens and detector decisions under the Copilot input.
        </div>
      </div>
      <Switch
        aria-label="Show Copilot debug line"
        checked={settings?.debug ?? false}
        disabled={settings === null || saving}
        onCheckedChange={(checked) => void change(checked)}
      />
    </div>
  );
}
