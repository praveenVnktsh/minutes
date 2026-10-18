'use client';

import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Switch } from '@/components/ui/switch';
import { liveTimelineApi } from '@/lib/liveTimeline';

/** The "Live summary" toggle: short points, decisions and actions while
 * recording, kept as the meeting's chapters. On by default. */
export function LiveSummarySettings() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let disposed = false;
    liveTimelineApi.getSettings()
      .then((settings) => { if (!disposed) setEnabled(settings.enabled); })
      .catch((error) => {
        console.warn('Could not load the live summary setting:', error);
        if (!disposed) setEnabled(true);
      });
    return () => { disposed = true; };
  }, []);

  const change = async (next: boolean) => {
    const previous = enabled;
    setEnabled(next);
    setSaving(true);
    try {
      setEnabled((await liveTimelineApi.setSettings(next)).enabled);
    } catch (error) {
      setEnabled(previous);
      toast.error('Could not save the live summary setting', {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="rounded-lg border border-hairline bg-surface-raised p-6 shadow-sm">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h3 className="mb-2 text-lg font-semibold text-ink">AI notes while recording</h3>
          <p id="live-summary-description" className="text-sm text-ink-muted">
            About once a minute, write key points, decisions and action items into your
            notes under their topics.
          </p>
        </div>
        <Switch
          aria-label="AI notes while recording"
          aria-describedby="live-summary-description"
          checked={enabled ?? true}
          disabled={enabled === null || saving}
          onCheckedChange={(checked) => void change(checked)}
        />
      </div>
    </div>
  );
}
