'use client';

import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { TranscriptModelCard, type TranscriptModelStatus } from '@/components/TranscriptModelCard';
import { BYTES_PER_MIB } from '@/lib/download-display';
import {
  LIVE_MODEL_DISPLAY_NAME,
  LIVE_MODEL_DOWNLOAD_PROGRESS_EVENT,
  liveTranscriptionApi,
  type LiveModelChoice,
  type LiveModelDownloadProgressEvent,
  type LiveTranscriptionSettings,
} from '@/lib/liveTranscriptionModels';

/** Card status for the live model from settings plus the latest progress event. */
export function liveModelCardStatus(
  settings: LiveTranscriptionSettings | null,
  progress: LiveModelDownloadProgressEvent | null,
): TranscriptModelStatus {
  if (progress?.status === 'downloading') return { Downloading: { progress: progress.progress } };
  if (progress?.status === 'error') return { Error: progress.error ?? 'Download failed' };
  if (progress?.status === 'completed' || settings?.liveModelStatus.downloaded) return 'Available';
  if (settings?.liveModelStatus.downloading) return { Downloading: { progress: progress?.progress ?? 0 } };
  return 'Missing';
}

/**
 * Settings for the two transcription passes: the live model that streams text
 * while recording, and whether the final model re-transcribes the recording
 * afterwards (the final model itself is chosen below, in TranscriptSettings).
 */
export function LiveTranscriptionModelSettings() {
  const [settings, setSettings] = useState<LiveTranscriptionSettings | null>(null);
  const [progress, setProgress] = useState<LiveModelDownloadProgressEvent | null>(null);
  const [saving, setSaving] = useState(false);
  const [cancelling, setCancelling] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setSettings(await liveTranscriptionApi.getSettings());
    } catch (error) {
      console.error('Failed to load live transcription settings:', error);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const unlisten = listen<LiveModelDownloadProgressEvent>(LIVE_MODEL_DOWNLOAD_PROGRESS_EVENT, (event) => {
      setProgress(event.payload);
      if (event.payload.status !== 'downloading') {
        setCancelling(false);
        void refresh();
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [refresh]);

  const save = useCallback(async (changes: { liveModel?: LiveModelChoice; autoFinalPass?: boolean }) => {
    const previous = settings;
    if (previous) setSettings({ ...previous, ...changes });
    setSaving(true);
    try {
      setSettings(await liveTranscriptionApi.setSettings(changes));
    } catch (error) {
      setSettings(previous);
      toast.error('Could not save transcription settings', {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setSaving(false);
    }
  }, [settings]);

  const download = useCallback(() => {
    setProgress({
      modelName: settings?.liveModelStatus.modelId ?? '',
      progress: 0,
      downloaded_mb: 0,
      total_mb: (settings?.liveModelStatus.sizeBytes ?? 0) / BYTES_PER_MIB,
      speed_mbps: 0,
      eta_seconds: null,
      status: 'downloading',
      error: null,
    });
    liveTranscriptionApi.download().catch((error) => {
      // The progress event carries the failure to the card and the toast.
      console.warn('Live model download did not finish:', error);
    });
  }, [settings]);

  const cancel = useCallback(() => {
    setCancelling(true);
    liveTranscriptionApi.cancelDownload().catch((error) => {
      setCancelling(false);
      console.error('Failed to cancel the live model download:', error);
    });
  }, []);

  const remove = useCallback(async () => {
    try {
      await liveTranscriptionApi.deleteModel();
      setProgress(null);
      await refresh();
    } catch (error) {
      toast.error('Could not delete the live model', {
        description: error instanceof Error ? error.message : String(error),
      });
    }
  }, [refresh]);

  const liveModel = settings?.liveModel ?? 'nemotron-streaming';
  const sizeMb = Math.round((settings?.liveModelStatus.sizeBytes ?? 0) / BYTES_PER_MIB);

  return (
    <div className="space-y-6 pb-6">
      <div>
        <Label className="mb-1 block text-sm font-medium text-ink">Live transcript model</Label>
        <p className="mx-1 mb-2 text-xs text-ink-muted">
          Streams text while you record. After the meeting, the final transcript model replaces it.
        </p>
        <div className="mx-1">
          <Select
            value={liveModel}
            disabled={!settings || saving}
            onValueChange={(value) => void save({ liveModel: value as LiveModelChoice })}
          >
            <SelectTrigger aria-label="Live transcript model" className="focus:ring-2 focus:ring-focus">
              <SelectValue placeholder="Select live model" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="nemotron-streaming">
                {LIVE_MODEL_DISPLAY_NAME} (Recommended)
              </SelectItem>
              <SelectItem value="chunked">Final transcript model, in chunks</SelectItem>
            </SelectContent>
          </Select>
        </div>
        {liveModel === 'nemotron-streaming' && (
          <div className="mt-4">
            <TranscriptModelCard
              displayName={LIVE_MODEL_DISPLAY_NAME}
              details={`NVIDIA streaming model for English, text in about half a second · ${sizeMb} MB`}
              status={liveModelCardStatus(settings, progress)}
              sizeMb={sizeMb}
              isSelected
              isRecommended
              isCancelling={cancelling}
              onSelect={() => {}}
              onDownload={download}
              onCancel={cancel}
              onDelete={() => void remove()}
            />
            {settings && !settings.liveModelStatus.downloaded && progress?.status !== 'downloading' && (
              <p className="mx-1 mt-2 text-xs text-ink-muted">
                Until it is downloaded, live text uses the final transcript model.
              </p>
            )}
          </div>
        )}
      </div>

      <div className="flex items-start justify-between gap-4 rounded-lg border border-hairline p-4">
        <div>
          <Label htmlFor="auto-final-transcript" className="text-sm font-medium text-ink">
            Automatic final transcript
          </Label>
          <p className="mt-1 text-xs text-ink-muted">
            When a meeting ends, re-transcribe the whole recording with the final model and identify
            speakers again. Replaces the live transcript; edited transcripts are never replaced.
          </p>
        </div>
        <Switch
          id="auto-final-transcript"
          aria-label="Automatic final transcript"
          checked={settings?.autoFinalPass ?? true}
          disabled={!settings || saving}
          onCheckedChange={(checked) => void save({ autoFinalPass: checked })}
        />
      </div>
    </div>
  );
}
