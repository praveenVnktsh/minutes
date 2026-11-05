'use client';

import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { Download, FolderOpen } from 'lucide-react';
import { Button } from './ui/button';
import { Label } from './ui/label';
import { TranscriptModelCard } from './TranscriptModelCard';
import { TranscriptionUnsupportedNotice } from './TranscriptionUnsupportedNotice';
import { FinalPassTimingSettings, LiveEngineSettings } from './LiveTranscriptionModelSettings';
import { BYTES_PER_MIB, formatBytes } from '@/lib/download-display';
import { cn } from '@/lib/utils';
import { panel } from '@/lib/theme-classes';
import {
  SPEECH_MODEL_DOWNLOAD_PROGRESS_EVENT,
  SPEECH_MODEL_PURPOSE,
  missingSpeechBytes,
  speechModel,
  speechModelCardStatus,
  speechModelsApi,
  transcriptionOsSupported,
  type SpeechModelDownloadProgressEvent,
  type SpeechModelKind,
  type SpeechModelsStatus,
} from '@/lib/speechModels';

type ProgressByKind = Partial<Record<SpeechModelKind, SpeechModelDownloadProgressEvent>>;

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** A stand-in event so a card shows progress the moment its download is asked for. */
function pendingProgress(kind: SpeechModelKind, sizeBytes: number): SpeechModelDownloadProgressEvent {
  return {
    modelId: '',
    kind,
    progress: 0,
    downloadedMb: 0,
    totalMb: sizeBytes / BYTES_PER_MIB,
    speedMbps: 0,
    etaSeconds: null,
    status: 'downloading',
    error: null,
  };
}

function Section({ title, description, children }: { title: string; description: string; children: ReactNode }) {
  return (
    <section className="space-y-3">
      <div>
        <Label className="mb-1 block text-sm font-medium text-ink">{title}</Label>
        <p className="mx-1 text-xs text-ink-muted">{description}</p>
      </div>
      {children}
    </section>
  );
}

/**
 * Settings → Transcription: the four speech models the app runs (all English,
 * all on this Mac), where the live model runs, and when the transcript is
 * redone after a meeting. There is nothing to choose between; each model is
 * downloaded or not.
 */
export function TranscriptSettings() {
  const [status, setStatus] = useState<SpeechModelsStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [progress, setProgress] = useState<ProgressByKind>({});
  const progressRef = useRef(progress);
  progressRef.current = progress;
  const [cancelling, setCancelling] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setStatus(await speechModelsApi.status());
      setLoadError(null);
    } catch (error) {
      console.error('Failed to load the speech models:', error);
      setLoadError(errorText(error));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const unlisten = listen<SpeechModelDownloadProgressEvent>(SPEECH_MODEL_DOWNLOAD_PROGRESS_EVENT, (event) => {
      const update = event.payload;
      setProgress((previous) => ({ ...previous, [update.kind]: update }));
      if (update.status !== 'downloading') {
        if (update.status !== 'completed') setCancelling(false);
        void refresh();
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [refresh]);

  const downloading =
    Object.values(progress).some((event) => event?.status === 'downloading')
    || (status?.models.some((model) => model.downloading) ?? false);

  const download = useCallback(async (kinds?: SpeechModelKind[]) => {
    const requested = (status?.models ?? []).filter(
      (model) => !model.downloaded && (!kinds || kinds.includes(model.kind)),
    );
    setProgress((previous) => {
      const next = { ...previous };
      for (const model of requested) next[model.kind] = pendingProgress(model.kind, model.sizeBytes);
      return next;
    });
    try {
      await speechModelsApi.download(kinds);
    } catch (error) {
      // A failed model reports itself through its progress event; anything
      // else (another download already running, no network) only lands here.
      console.warn('Speech model download did not finish:', error);
      const reported = requested.some((model) => {
        const state = progressRef.current[model.kind]?.status;
        return state === 'error' || state === 'cancelled';
      });
      if (!reported) toast.error('Could not download the speech models', { description: errorText(error) });
    } finally {
      // Nothing is downloading once the call returns: clear the stand-ins of
      // models a cancel or failure left queued.
      setProgress((previous) => {
        const next = { ...previous };
        for (const model of requested) {
          if (next[model.kind]?.status === 'downloading') delete next[model.kind];
        }
        return next;
      });
      setCancelling(false);
      void refresh();
    }
  }, [status, refresh]);

  const cancel = useCallback(() => {
    setCancelling(true);
    speechModelsApi.cancelDownload().catch((error) => {
      setCancelling(false);
      console.error('Failed to cancel the speech model download:', error);
    });
  }, []);

  const remove = useCallback(async (kind: SpeechModelKind) => {
    try {
      await speechModelsApi.deleteModel(kind);
      setProgress((previous) => {
        const next = { ...previous };
        delete next[kind];
        return next;
      });
    } catch (error) {
      toast.error('Could not delete the model', { description: errorText(error) });
    } finally {
      void refresh();
    }
  }, [refresh]);

  const openFolder = useCallback(() => {
    speechModelsApi.openFolder().catch((error) => {
      toast.error('Could not open the models folder', { description: errorText(error) });
    });
  }, []);

  const card = (kind: SpeechModelKind) => {
    const model = speechModel(status, kind);
    if (!model) return null;
    const event = progress[kind] ?? null;
    const cardStatus = speechModelCardStatus(model, event);
    const isDownloading = typeof cardStatus === 'object' && 'Downloading' in cardStatus;
    return (
      <TranscriptModelCard
        key={kind}
        displayName={model.displayName}
        details={`${SPEECH_MODEL_PURPOSE[kind]} · ${formatBytes(model.sizeBytes)}`}
        status={cardStatus}
        sizeMb={model.sizeBytes / BYTES_PER_MIB}
        isSelected
        isRecommended={false}
        isCancelling={cancelling && isDownloading}
        deleteDisabled={downloading}
        downloadDisabled={downloading}
        onSelect={() => {}}
        onDownload={() => void download([kind])}
        onCancel={cancel}
        onDelete={() => void remove(kind)}
      />
    );
  };

  const live = speechModel(status, 'live-asr');
  const missingBytes = missingSpeechBytes(status);
  // Before macOS 15 the models cannot run, so there is nothing to download.
  const osSupported = transcriptionOsSupported(status);

  return (
    <div className="space-y-8 pb-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="mx-1 text-xs text-ink-muted">
          Transcription runs on this Mac in English. Nothing leaves your computer.
        </p>
        <div className="flex flex-wrap items-center gap-2">
          {osSupported && missingBytes > 0 && (
            <Button variant="outline" size="sm" disabled={downloading} onClick={() => void download()}>
              <Download className="h-4 w-4" />
              Download all ({formatBytes(missingBytes)})
            </Button>
          )}
          <Button variant="ghost" size="sm" onClick={openFolder}>
            <FolderOpen className="h-4 w-4" />
            Open folder
          </Button>
        </div>
      </div>

      {loadError && (
        <div className={cn(panel.error, 'rounded-lg p-3 text-sm')}>
          Could not read the speech models: {loadError}
        </div>
      )}

      {!osSupported && <TranscriptionUnsupportedNotice />}

      {osSupported && (
        <>
          <Section
            title="Live transcription"
            description="Streams text while you record. After the meeting, the transcript is redone with Parakeet."
          >
            {card('live-asr')}
            {live && !live.downloaded && progress['live-asr']?.status !== 'downloading' && (
              <p className="mx-1 text-xs text-ink-muted">
                Until it is downloaded, meetings are recorded without live text and transcribed afterwards.
              </p>
            )}
            <LiveEngineSettings />
          </Section>

          <Section
            title="Transcript after meetings"
            description="Both are needed to transcribe a meeting after it ends, to import audio and to re-transcribe."
          >
            {card('final-asr')}
            {card('vad')}
            <FinalPassTimingSettings />
          </Section>

          <Section
            title="Speakers"
            description="Labels who said what after each meeting. Without it, transcripts have no speaker names."
          >
            {card('diarization')}
          </Section>
        </>
      )}
    </div>
  );
}
