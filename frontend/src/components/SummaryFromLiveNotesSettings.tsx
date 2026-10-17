'use client';

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { Switch } from '@/components/ui/switch';

export interface SummaryFromLiveNotesSettingsData {
  enabled: boolean;
}

export const summaryFromLiveNotesApi = {
  get: () => invoke<SummaryFromLiveNotesSettingsData>('api_get_summary_from_live_notes'),
  set: (enabled: boolean) =>
    invoke<SummaryFromLiveNotesSettingsData>('api_set_summary_from_live_notes', { enabled }),
};

/** "Build notes from live notes": enhanced notes use the live notes' topics
 * as their sections. On by default; off writes them from the whole
 * transcript, as before. */
export function SummaryFromLiveNotesSettings() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let disposed = false;
    summaryFromLiveNotesApi.get()
      .then((settings) => { if (!disposed) setEnabled(settings.enabled); })
      .catch((error) => {
        console.warn('Could not load the build-from-live-notes setting:', error);
        if (!disposed) setEnabled(true);
      });
    return () => { disposed = true; };
  }, []);

  const change = async (next: boolean) => {
    const previous = enabled;
    setEnabled(next);
    setSaving(true);
    try {
      setEnabled((await summaryFromLiveNotesApi.set(next)).enabled);
    } catch (error) {
      setEnabled(previous);
      toast.error('Could not save the setting', {
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
          <h3 className="mb-2 text-lg font-semibold text-ink">Build notes from live notes</h3>
          <p id="summary-from-live-notes-description" className="text-sm text-ink-muted">
            When a meeting has live notes, the enhanced notes follow their topics and fill in the
            details from the transcript, with links to the moments they come from. Turn this off to
            write them from the whole transcript instead.
          </p>
        </div>
        <Switch
          aria-label="Build notes from live notes"
          aria-describedby="summary-from-live-notes-description"
          checked={enabled ?? true}
          disabled={enabled === null || saving}
          onCheckedChange={(checked) => void change(checked)}
        />
      </div>
    </div>
  );
}
