'use client';

import { useCallback, useEffect, useState } from 'react';
import { Download, Trash2 } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { SegmentedControl } from '@/components/ui/segmented-control';
import { builtInDownloadSource } from '@/components/BuiltInModelManager';
import { useConfig } from '@/contexts/ConfigContext';
import { useModelDownload } from '@/hooks/useModelDownload';
import {
  BuiltInAIAPI,
  LIVE_NOTES_MODEL,
  liveNotesApi,
  type BuiltInModelInfo,
  type LiveNotesModelChoice,
  type LiveNotesSettings,
} from '@/lib/builtin-ai';
import { formatSummaryModelSizeLabelFromMb } from '@/lib/onboarding-summary-model';
import { cn } from '@/lib/utils';
import { panel, progress as progressBar, toneFill, toneText } from '@/lib/theme-classes';

const CHOICES: { value: LiveNotesModelChoice; label: string }[] = [
  { value: 'summary', label: 'Same as summary model' },
  { value: LIVE_NOTES_MODEL, label: 'Gemma 4 E2B (faster, lighter)' },
];

/**
 * Settings → Summary → Live notes model: which built-in model writes the live notes, and the
 * download of the small live notes model. Nothing downloads until the Download button is
 * pressed; until then (and on 8 GB Macs) live notes use the summary model. Once it is on disk
 * it is the default on Macs with 16 GB or more, unless "Same as summary model" was picked.
 */
export function LiveNotesModelSettings() {
  const { modelConfig } = useConfig();
  const [settings, setSettings] = useState<LiveNotesSettings | null>(null);
  const [model, setModel] = useState<BuiltInModelInfo | null>(null);
  const [saving, setSaving] = useState(false);

  const refreshModel = useCallback(async () => {
    try {
      setModel(await BuiltInAIAPI.getModelInfo(LIVE_NOTES_MODEL));
    } catch (error) {
      console.warn('Could not load the live notes model:', error);
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    liveNotesApi.getSettings()
      .then((loaded) => { if (!disposed) setSettings(loaded); })
      .catch((error) => console.warn('Could not load the live notes settings:', error));
    void refreshModel();
    return () => { disposed = true; };
  }, [refreshModel]);

  const choose = async (next: LiveNotesModelChoice) => {
    const previous = settings;
    if (previous) setSettings({ ...previous, model: next });
    setSaving(true);
    try {
      setSettings(await liveNotesApi.setModel(next));
    } catch (error) {
      setSettings(previous);
      toast.error('Could not save the live notes model', {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setSaving(false);
    }
  };

  const download = useModelDownload({
    source: builtInDownloadSource,
    onComplete: (name) => {
      if (name !== LIVE_NOTES_MODEL) return;
      void refreshModel();
      // The download was asked for here, to use it: switch live notes to it.
      void choose(LIVE_NOTES_MODEL);
      toast.success('Live notes now use Gemma 4 E2B');
    },
    onCancelled: (name) => { if (name === LIVE_NOTES_MODEL) void refreshModel(); },
    onError: (name) => { if (name === LIVE_NOTES_MODEL) void refreshModel(); },
    onStartFailed: (name, error) => {
      if (name !== LIVE_NOTES_MODEL || String(error).startsWith('CANCELLED:')) return;
      toast.error('Could not download the live notes model');
      void refreshModel();
    },
  });

  const refreshSettings = useCallback(async () => {
    try {
      setSettings(await liveNotesApi.getSettings());
    } catch (error) {
      console.warn('Could not load the live notes settings:', error);
    }
  }, []);

  // Removing the file is not a choice: without it live notes fall back to the summary model,
  // and a later download makes Gemma the default again unless "Same as summary model" was picked.
  const remove = async () => {
    try {
      await BuiltInAIAPI.deleteModel(LIVE_NOTES_MODEL);
      toast.success('Live notes model removed');
    } catch (error) {
      toast.error('Could not remove the live notes model', {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      void refreshModel();
      void refreshSettings();
    }
  };

  const downloading = download.downloading.has(LIVE_NOTES_MODEL);
  const percent = download.progress[LIVE_NOTES_MODEL];
  const detail = download.detail[LIVE_NOTES_MODEL];
  const available = model?.status.type === 'available';
  const sizeLabel = model ? formatSummaryModelSizeLabelFromMb(model.size_mb) : '';
  const builtIn = modelConfig.provider === 'builtin-ai';
  const choice = settings?.model ?? 'summary';
  const options = CHOICES.map((option) => ({
    ...option,
    // Chosen only once it is on disk: choosing it is never a download.
    disabled: option.value === LIVE_NOTES_MODEL && !available,
  }));

  return (
    <div className="rounded-lg border border-hairline bg-surface-raised p-6 shadow-sm">
      <h3 className="mb-2 text-lg font-semibold text-ink">Live notes model</h3>
      <p id="live-notes-model-description" className="mb-4 text-sm text-ink-muted">
        The built-in model that writes live notes while you record. Gemma 4 E2B is a small
        model made for them: faster and lighter on the GPU, at a {sizeLabel || 'one-time'} download.
        {settings?.opt_in_by_default
          ? ' Once downloaded, live notes use it unless you pick "Same as summary model".'
          : ''}
      </p>

      <SegmentedControl
        aria-label="Live notes model"
        value={choice}
        options={options}
        disabled={settings === null || saving}
        onValueChange={(next) => void choose(next)}
      />

      <div className="mt-4 rounded-lg border border-hairline bg-surface-1 p-4">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-medium text-ink">Gemma 4 E2B</span>
              {available && (
                <span className={cn('flex items-center gap-1 text-xs font-medium', toneText.success)}>
                  <span className={cn('h-2 w-2 rounded-full', toneFill.success)} />
                  Ready
                </span>
              )}
              {model?.status.type === 'error' && (
                <span className={cn('text-xs font-medium', toneText.error)}>Download failed</span>
              )}
            </div>
            <p className="text-xs text-ink-muted">
              {sizeLabel}
              {settings?.download_pending && !available && !downloading
                ? ' · Downloads after the transcription and summary models finish.'
                : ''}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {!available && !downloading && (
              <Button variant="outline" size="sm" onClick={() => void download.download(LIVE_NOTES_MODEL)}>
                <Download className="mr-2 h-4 w-4" />
                Download
              </Button>
            )}
            {downloading && (
              <Button
                variant="outline"
                size="sm"
                onClick={async () => {
                  try {
                    await download.cancel(LIVE_NOTES_MODEL);
                    download.settle(LIVE_NOTES_MODEL);
                  } catch (error) {
                    console.error('Failed to cancel the live notes download:', error);
                  }
                }}
              >
                Cancel
              </Button>
            )}
            {available && !downloading && (
              <Button variant="outline" size="sm" onClick={() => void remove()}>
                <Trash2 className="mr-2 h-4 w-4" />
                Remove
              </Button>
            )}
          </div>
        </div>
        {downloading && percent !== undefined && (
          <div className="mt-3">
            <div className="mb-1 flex items-center justify-between text-sm text-ink">
              <span>
                {detail && detail.totalMb > 0
                  ? `${detail.downloadedMb.toFixed(1)} MiB / ${detail.totalMb.toFixed(1)} MiB`
                  : 'Downloading…'}
              </span>
              <span className="font-semibold">{Math.round(percent)}%</span>
            </div>
            <div
              className={cn('h-2 w-full overflow-hidden rounded-full', progressBar.track)}
              role="progressbar"
              aria-label="Live notes model download"
              aria-valuenow={Math.round(percent)}
              aria-valuemin={0}
              aria-valuemax={100}
            >
              <div
                className={cn('h-full rounded-full transition-all duration-300', progressBar.fill)}
                style={{ width: `${percent}%` }}
              />
            </div>
          </div>
        )}
      </div>

      {settings?.low_memory && (
        <p className={cn('mt-4 rounded-md p-3 text-sm', panel.warning)}>
          This Mac has 8 GB of memory: while recording, live notes stay on the summary model so
          only one model is loaded.
        </p>
      )}
      {!builtIn && (
        <p className={cn('mt-4 rounded-md p-3 text-sm', panel.neutral)}>
          Applies when Built-in AI is the summary provider. With another provider, live notes use
          that provider.
        </p>
      )}
    </div>
  );
}
