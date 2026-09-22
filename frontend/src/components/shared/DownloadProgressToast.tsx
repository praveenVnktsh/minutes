'use client';

import React, { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { X, Download, Check, Loader2, ArrowBigDownDash } from 'lucide-react';
import type { ParakeetDownloadProgressEvent } from '@/lib/parakeet';
import { BUILTIN_AI_DOWNLOAD_PROGRESS_EVENT, type BuiltInAIDownloadProgressEvent } from '@/lib/builtin-ai';
import { BYTES_PER_MIB } from '@/lib/download-display';

interface DownloadProgress {
  modelName: string;
  displayName: string;
  progress: number;
  downloadedMb: number;
  totalMb: number;
  speedMbps: number;
  status: 'downloading' | 'completed' | 'error' | 'cancelled';
  unitLabel?: string;
  error?: string;
}

// Categorize error messages for better user experience
function categorizeError(error: string): string {
  const lowerError = error.toLowerCase();

  if (lowerError.includes('network') ||
    lowerError.includes('connection') ||
    lowerError.includes('timeout') ||
    lowerError.includes('failed to start download')) {
    return 'Network error - Check your internet connection';
  }

  if (lowerError.includes('status:') || lowerError.includes('http')) {
    return 'Server error - Download temporarily unavailable';
  }

  if (lowerError.includes('disk') ||
    lowerError.includes('write') ||
    lowerError.includes('file')) {
    return 'Storage error - Check available disk space';
  }

  if (lowerError.includes('invalid') || lowerError.includes('validation')) {
    return 'File validation failed - Please retry download';
  }

  // Fallback to original error
  return error;
}

// Custom toast component for download progress
function DownloadToastContent({
  download,
  onDismiss,
}: {
  download: DownloadProgress;
  onDismiss?: () => void;
}) {
  const isComplete = download.status === 'completed';
  const hasError = download.status === 'error';
  const isCancelled = download.status === 'cancelled';
  const unitLabel = download.unitLabel ?? 'MB';

  return (
    <div className="flex items-center gap-3 w-full max-w-sm bg-surface-raised rounded-lg shadow-lg border border-hairline p-3 relative">

      {/* Icon */}
      <div className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center ${isComplete ? 'bg-green-100' : hasError ? 'bg-red-100' : isCancelled ? 'bg-surface-2' : 'bg-surface-2'
        }`}>
        {isComplete ? (
          <Check className="w-4 h-4 text-green-600" />
        ) : hasError ? (
          <X className="w-4 h-4 text-red-600" />
        ) : isCancelled ? (
          <X className="w-4 h-4 text-ink-muted" />
        ) : (
          <ArrowBigDownDash className="size-5 text-ink-muted " />
        )}
      </div>

      {/* Dismiss */}
      {onDismiss && (
        <button
          type="button"
          onClick={onDismiss}
          aria-label={`Dismiss ${download.displayName} notification`}
          className="absolute top-1 right-1 rounded p-1 text-ink-muted transition-colors hover:bg-surface-2 hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      )}

      {/* Content */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center justify-between gap-2 mb-1 pr-5">
          <p className="text-sm font-medium text-ink truncate">
            {download.displayName}
          </p>
        </div>

        {hasError ? (
          <p className="text-xs text-red-600">{download.error || 'Download failed'}</p>
        ) : isComplete ? (
          <p className="text-xs text-green-600">Download complete</p>
        ) : isCancelled ? (
          <p className="text-xs text-ink-muted">Download cancelled</p>
        ) : (
          <>
            {/* Progress bar */}
            <div className="w-full h-1.5 bg-surface-2 rounded-full overflow-hidden mb-1.5">
              <div
                className="h-full bg-brand rounded-full transition-all duration-300"
                style={{ width: `${download.progress}%` }}
              />
            </div>

            {/* Progress text */}
            <div className="flex items-center justify-between text-xs text-ink-muted">
              <span>
                {download.downloadedMb.toFixed(1)} / {download.totalMb.toFixed(1)} {unitLabel}
              </span>
              <span className="flex items-center gap-1">
                {download.speedMbps > 0 && (
                  <span>{download.speedMbps.toFixed(1)} {unitLabel}/s</span>
                )}
                <span className="text-ink font-medium">
                  {Math.round(download.progress)}%
                </span>
              </span>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

// Hook to manage download progress toasts
export function useDownloadProgressToast() {
  const [downloads, setDownloads] = useState<Map<string, DownloadProgress>>(new Map());
  const [dismissedModels, setDismissedModels] = useState<Set<string>>(new Set());

  const updateDownload = useCallback((modelName: string, data: Partial<DownloadProgress>) => {
    setDownloads((prev) => {
      const updated = new Map(prev);
      const existing = updated.get(modelName) || {
        modelName,
        displayName: modelName,
        progress: 0,
        downloadedMb: 0,
        totalMb: 0,
        speedMbps: 0,
        status: 'downloading' as const,
      };

      updated.set(modelName, { ...existing, ...data });
      return updated;
    });
  }, []);

  const cleanupDownload = useCallback((modelName: string, delay: number = 4000) => {
    // Remove download from map after delay (allows toast to show and auto-dismiss)
    setTimeout(() => {
      setDownloads((prev) => {
        const updated = new Map(prev);
        updated.delete(modelName);
        return updated;
      });
    }, delay);
  }, []);

  const showDownloadToast = useCallback((download: DownloadProgress) => {
    const toastId = `download-${download.modelName}`;

    // Determine duration based on status
    const getDuration = () => {
      switch (download.status) {
        case 'completed': return 3000;      // 3 seconds
        case 'cancelled': return 5000;      // 5 seconds
        case 'error': return 10000;         // 10 seconds
        case 'downloading': return Infinity; // Manual dismiss only
      }
    };

    // Dismiss handler
    const dismissToast = () => {
      toast.dismiss(toastId);
      setDismissedModels(prev => {
        const next = new Set(prev);
        next.add(download.modelName);
        return next;
      });
    };

    toast.custom(
      (t) => (
        <DownloadToastContent
          download={download}
          onDismiss={dismissToast}
        />
      ),
      {
        position: 'bottom-center',
        id: toastId,
        duration: getDuration(),
      }
    );
  }, []);

  // Effect to handle toast visibility based on dismissed state
  useEffect(() => {
    downloads.forEach((download) => {
      const wasDismissed = dismissedModels.has(download.modelName);

      if (download.status === 'downloading') {
        // A dismissal only hides the in-progress toast, and it keeps hiding it
        // for as long as the download is still running.
        if (wasDismissed) {
          return;
        }
      } else if (wasDismissed) {
        // The download reached a terminal state (completed, error or cancelled),
        // so the dismissal has served its purpose. Clear it so the final toast
        // can appear and so a stale dismissal can't silence a later re-download
        // of the same model. This converges: once removed, the branch is skipped.
        setDismissedModels(prev => {
          const next = new Set(prev);
          next.delete(download.modelName);
          return next;
        });
      }

      showDownloadToast(download);
    });
  }, [downloads, dismissedModels, showDownloadToast]);

  // Listen to Parakeet download events
  useEffect(() => {
    const unlistenProgress = listen<ParakeetDownloadProgressEvent>(
      'parakeet-model-download-progress',
      (event) => {
        const { modelName, progress, downloaded_mb, total_mb, speed_mbps, status } = event.payload;
        const downloadData: DownloadProgress = {
          modelName,
          displayName: 'Transcription Model (Parakeet)',
          progress,
          downloadedMb: downloaded_mb ?? 0,
          totalMb: total_mb ?? 670,
          speedMbps: speed_mbps ?? 0,
          status: status === 'cancelled'
            ? 'cancelled'
            : status === 'completed'
              ? 'completed'
              : 'downloading',
        };

        updateDownload(modelName, downloadData);
        if (downloadData.status === 'cancelled') {
          cleanupDownload(modelName, 6000);
        }
      }
    );

    const unlistenComplete = listen<{ modelName: string }>(
      'parakeet-model-download-complete',
      (event) => {
        const { modelName } = event.payload;
        const downloadData: DownloadProgress = {
          modelName,
          displayName: 'Transcription Model (Parakeet)',
          progress: 100,
          downloadedMb: 670,
          totalMb: 670,
          speedMbps: 0,
          status: 'completed',
        };
        updateDownload(modelName, downloadData);
        // Clean up after 4 seconds (completion toast duration is 3s + 1s buffer)
        cleanupDownload(modelName, 4000);
      }
    );

    const unlistenError = listen<{ modelName: string; error: string }>(
      'parakeet-model-download-error',
      (event) => {
        const { modelName, error } = event.payload;
        const downloadData: DownloadProgress = {
          modelName,
          displayName: 'Transcription Model (Parakeet)',
          progress: 0,
          downloadedMb: 0,
          totalMb: 670,
          speedMbps: 0,
          status: 'error',
          error: categorizeError(error),
        };
        updateDownload(modelName, downloadData);
        // Clean up after 11 seconds (error toast duration is 10s + 1s buffer)
        cleanupDownload(modelName, 11000);
      }
    );

    return () => {
      unlistenProgress.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
      unlistenError.then((fn) => fn());
    };
  }, [updateDownload, cleanupDownload]);

  // Listen to Built-in AI summary model download events
  useEffect(() => {
    const unlisten = listen<BuiltInAIDownloadProgressEvent>(BUILTIN_AI_DOWNLOAD_PROGRESS_EVENT, (event) => {
      const { model, progress, downloaded_mb, total_mb, total_bytes, speed_mbps, status, error } = event.payload;

      // The backend sends total_bytes on every emission including completion,
      // so it takes priority over the decimal total_mb that can lag behind it.
      const totalMb = total_bytes ? total_bytes / BYTES_PER_MIB : (total_mb ?? 0);

      const downloadData: DownloadProgress = {
        modelName: model,
        displayName: `Summary Model (${model})`,
        progress: progress ?? 0,
        downloadedMb: downloaded_mb ?? 0,
        totalMb,
        speedMbps: speed_mbps ?? 0,
        unitLabel: 'MiB',
        status: status === 'completed' || progress >= 100
          ? 'completed'
          : status === 'cancelled'
            ? 'cancelled'
            : status === 'error'
              ? 'error'
              : 'downloading',
        error: status === 'error' ? categorizeError(error || 'Download failed') : undefined,
      };

      updateDownload(model, downloadData);

      // Clean up finished downloads after delay to prevent endless toasts
      if (downloadData.status === 'completed') {
        cleanupDownload(model, 4000);  // 3s toast + 1s buffer
      } else if (downloadData.status === 'error') {
        cleanupDownload(model, 11000); // 10s toast + 1s buffer
      } else if (downloadData.status === 'cancelled') {
        cleanupDownload(model, 6000);  // 5s toast + 1s buffer
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [updateDownload, cleanupDownload]);

  return { downloads };
}

// Component to initialize download toast listeners at app level
export function DownloadProgressToastProvider() {
  useDownloadProgressToast();
  return null;
}
