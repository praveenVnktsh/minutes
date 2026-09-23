'use client';

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Button } from '@/components/ui/button';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { cn } from '@/lib/utils';
import { Download, RefreshCw, BadgeAlert, Trash2 } from 'lucide-react';
import { toast } from 'sonner';
import { formatSummaryModelSizeLabelFromMb } from '@/lib/onboarding-summary-model';
import { listenAll, useModelDownload, type ModelDownloadSource } from '@/hooks/useModelDownload';
import {
  BuiltInAIAPI,
  BUILTIN_AI_DOWNLOAD_PROGRESS_EVENT,
  type BuiltInAIDownloadProgressEvent,
} from '@/lib/builtin-ai';

interface ModelInfo {
  name: string;
  display_name: string;
  status: {
    type: 'not_downloaded' | 'downloading' | 'available' | 'corrupted' | 'error';
    progress?: number;
  };
  size_mb: number;
  context_size: number;
  description: string;
  gguf_file: string;
}

interface BuiltInModelManagerProps {
  selectedModel: string;
  onModelSelect: (model: string) => void;
  layout?: 'inline' | 'dialog';
}

// Module-level so it is referentially stable across renders.
const builtInDownloadSource: ModelDownloadSource = {
  subscribe: (emit) =>
    listenAll([
      listen<BuiltInAIDownloadProgressEvent>(BUILTIN_AI_DOWNLOAD_PROGRESS_EVENT, (event) => {
        const payload = event.payload;
        const { model, progress, status } = payload;

        if (status === 'completed') {
          emit({ kind: 'complete', model });
        } else if (status === 'cancelled') {
          emit({ kind: 'cancelled', model });
        } else if (status === 'error') {
          emit({ kind: 'error', model, error: payload.error ?? '' });
        } else {
          emit({
            kind: 'progress',
            model,
            progress,
            detail: {
              downloadedMb: payload.downloaded_mb ?? 0,
              totalMb: payload.total_mb ?? 0,
              speedMbps: payload.speed_mbps ?? 0,
            },
          });
        }
      }),
    ]),
  start: (model) => BuiltInAIAPI.downloadModel(model),
  cancel: (model) => BuiltInAIAPI.cancelDownload(model),
};

export function BuiltInModelManager({
  selectedModel,
  onModelSelect,
  layout = 'inline',
}: BuiltInModelManagerProps) {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [isLoading, setIsLoading] = useState<boolean>(false);
  const [hasFetched, setHasFetched] = useState<boolean>(false);

  const fetchModels = async () => {
    try {
      setIsLoading(true);
      const data = (await invoke('builtin_ai_list_models')) as ModelInfo[];
      setModels(data);

      // Auto-select first available model if none selected
      if (data.length > 0 && !selectedModel) {
        const firstAvailable = data.find((m) => m.status.type === 'available');
        if (firstAvailable) {
          onModelSelect(firstAvailable.name);
        }
      }
    } catch (error) {
      console.error('Failed to fetch built-in AI models:', error);
      toast.error('Failed to load models');
    } finally {
      setIsLoading(false);
      setHasFetched(true);
    }
  };

  useEffect(() => {
    fetchModels();
  }, []);

  const modelDownload = useModelDownload({
    source: builtInDownloadSource,
    onComplete: (model) => {
      fetchModels();
      toast.success(`Model ${model} downloaded successfully`);
    },
    onCancelled: () => {
      fetchModels();
    },
    onError: (model) => {
      // Update model status to error locally instead of fetching from backend
      // Backend doesn't persist error status, so fetchModels() would return not_downloaded
      setModels((prevModels) =>
        prevModels.map((m) =>
          m.name === model
            ? {
                ...m,
                status: {
                  type: 'error',
                  progress: 0,
                } as any,
              }
            : m
        )
      );

      // Don't show error toast here - DownloadProgressToast already handles it
      // Don't call fetchModels() - it would overwrite error status with not_downloaded
    },
    onStartFailed: (model, error) => {
      console.error('Failed to download model:', error);

      // Check if this is a cancellation error (starts with "CANCELLED:")
      const errorMsg = String(error);
      if (errorMsg.startsWith('CANCELLED:')) {
        // Cancel handler already removed the model from downloading
        // Don't show error toast for cancellations - cancel function already shows info toast
        return;
      }

      // For real errors, show toast
      toast.error(`Failed to download ${model}`);

      // Refresh model list to get updated Error status from backend
      fetchModels();
    },
  });

  const downloadModel = (modelName: string) => {
    modelDownload.download(modelName);
  };

  const cancelDownload = async (modelName: string) => {
    try {
      await modelDownload.cancel(modelName);
      toast.info(`Download of ${modelName} cancelled`);
      modelDownload.settle(modelName);
    } catch (error) {
      console.error('Failed to cancel download:', error);
    }
  };

  const deleteModel = async (modelName: string) => {
    try {
      await invoke('builtin_ai_delete_model', { modelName });
      toast.success(`Model ${modelName} deleted`);
      fetchModels();
    } catch (error) {
      console.error('Failed to delete model:', error);
      toast.error(`Failed to delete ${modelName}`);
    }
  };

  // Don't show loading spinner if we have downloads in progress - show the model list instead
  if (isLoading && modelDownload.downloading.size === 0) {
    return (
      <div className="text-center py-8 text-muted-foreground">
        <RefreshCw className="mx-auto h-8 w-8 animate-spin mb-2" />
        Loading models...
      </div>
    );
  }

  // Only show "no models" message after fetch has completed
  if (hasFetched && models.length === 0) {
    return (
      <Alert>
        <AlertDescription>
          No models found. Download a model to get started with Built-in AI.
        </AlertDescription>
      </Alert>
    );
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-4">
        <h4 className="text-sm font-bold">Built-in AI Models</h4>
      </div>

      <div
        className={cn(
          'grid gap-4',
          layout === 'dialog' && 'max-h-[50vh] overflow-y-auto pr-2 pb-2'
        )}
      >
        {models.map((model) => {
          const progress = modelDownload.progress[model.name];
          const progressInfo = modelDownload.detail[model.name];
          const modelIsDownloading = modelDownload.downloading.has(model.name);
          const isAvailable = model.status.type === 'available';
          const isNotDownloaded = model.status.type === 'not_downloaded';
          const isCorrupted = model.status.type === 'corrupted';
          const isError = model.status.type === 'error';

          return (
            <div
              key={model.name}
              className={cn(
                'p-4 rounded-lg border transition-colors',
                modelIsDownloading
                  ? 'bg-surface-raised border-hairline'
                  : 'bg-card',
                selectedModel === model.name
                  ? 'ring-2 ring-ink border-ink'
                  : 'border-hairline hover:border-hairline',
                isAvailable && !modelIsDownloading && 'cursor-pointer'
              )}
              onClick={() => {
                if (isAvailable && !modelIsDownloading) {
                  onModelSelect(model.name);
                }
              }}
            >
            <div className="space-y-3">
              <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                <div className="min-w-0 flex-1">
                  <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
                    <span className="min-w-0 break-words text-base font-bold leading-snug text-ink">{model.display_name || model.name}</span>
                    {isAvailable && (
                      <>
                        <span className="flex shrink-0 items-center gap-1 text-xs font-medium text-green-600">
                          <span className="h-2 w-2 rounded-full bg-green-600"></span>
                          Ready
                        </span>
                        {selectedModel === model.name && (
                          <span className="shrink-0 rounded bg-blue-100 px-2 py-0.5 text-xs font-medium text-blue-700">
                            Selected
                          </span>
                        )}
                      </>
                    )}
                    {isCorrupted && (
                      <span className="flex shrink-0 items-center gap-1 rounded bg-red-100 px-2 py-0.5 text-xs font-medium text-red-700">
                        <BadgeAlert className="h-3 w-3" />
                        Corrupted
                      </span>
                    )}
                    {isError && (
                      <span className="shrink-0 rounded bg-red-100 px-2 py-0.5 text-xs font-medium text-red-700">
                        Error
                      </span>
                    )}
                  </div>
                </div>
                <div className="flex w-full shrink-0 flex-wrap items-center gap-2 sm:ml-4 sm:w-auto sm:justify-end">
                  {/* Not Downloaded - Show Download button */}
                  {isNotDownloaded && !modelIsDownloading && (
                    <Button
                      variant="outline"
                      size="sm"
                      className="min-w-[100px]"
                      onClick={(e) => {
                        e.stopPropagation();
                        downloadModel(model.name);
                      }}
                    >
                      <Download className="mr-2 h-4 w-4" />
                      Download
                    </Button>
                  )}
                  {/* Downloading - Show Cancel button */}
                  {modelIsDownloading && (
                    <Button
                      variant="outline"
                      size="sm"
                      className="min-w-[100px]"
                      onClick={(e) => {
                        e.stopPropagation();
                        cancelDownload(model.name);
                      }}
                    >
                      Cancel
                    </Button>
                  )}
                  {/* Error - Show Retry button */}
                  {isError && !modelIsDownloading && (
                    <Button
                      variant="outline"
                      size="sm"
                      className="min-w-[100px]"
                      onClick={(e) => {
                        e.stopPropagation();
                        downloadModel(model.name);
                      }}
                    >
                      <RefreshCw className="mr-2 h-4 w-4" />
                      Retry
                    </Button>
                  )}
                  {/* Corrupted - Show both Retry and Delete buttons */}
                  {isCorrupted && !modelIsDownloading && (
                    <>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={(e) => {
                          e.stopPropagation();
                          downloadModel(model.name);
                        }}
                      >
                        <RefreshCw className="mr-2 h-4 w-4" />
                        Retry
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={(e) => {
                          e.stopPropagation();
                          deleteModel(model.name);
                        }}
                      >
                        <Trash2 className="mr-2 h-4 w-4" />
                        Delete
                      </Button>
                    </>
                  )}
                  {/* Available - Show small trash icon (only if not currently selected) */}
                  {isAvailable && !modelIsDownloading && selectedModel !== model.name && (
                    <button
                      className="p-2 rounded hover:bg-surface-2 transition-colors text-ink-muted hover:text-red-600"
                      onClick={(e) => {
                        e.stopPropagation();
                        deleteModel(model.name);
                      }}
                      title="Delete model"
                    >
                      <Trash2 className="h-4 w-4" />
                    </button>
                  )}
                </div>
              </div>
              <div className="text-sm text-ink-muted">
                {model.description && (
                  <p className="mb-1">{model.description}</p>
                )}
                {(isError || isCorrupted) && (
                  <p className="mb-1 text-xs text-red-600">
                    {isError && typeof model.status === 'object' && 'Error' in model.status
                      ? (model.status as any).Error
                      : isCorrupted
                      ? 'File is corrupted. Retry download or delete.'
                      : 'An error occurred'}
                  </p>
                )}
                <div className="text-xs text-ink-muted">
                  <span>{formatSummaryModelSizeLabelFromMb(model.size_mb)} • {model.context_size} tokens</span>
                </div>
                </div>
              </div>

              {/* Download progress bar */}
              {modelIsDownloading && progress !== undefined && (
                <div className="mt-3 pt-3 border-t border-hairline">
                  <div className="flex items-center justify-between mb-1">
                    <span className="text-sm font-medium text-ink">Downloading...</span>
                    <span className="text-sm font-semibold text-ink">
                      {Math.round(progress)}%
                    </span>
                  </div>
                  <div className="text-sm text-ink-muted mb-2">
                    {progressInfo?.totalMb > 0 ? (
                      <>
                        {progressInfo.downloadedMb.toFixed(1)} MiB / {progressInfo.totalMb.toFixed(1)} MiB
                        {progressInfo.speedMbps > 0 && (
                          <span className="ml-2 text-ink-muted">
                            ({progressInfo.speedMbps.toFixed(1)} MiB/s)
                          </span>
                        )}
                      </>
                    ) : (
                      <span>{formatSummaryModelSizeLabelFromMb(model.size_mb)}</span>
                    )}
                  </div>
                  <div className="w-full h-2.5 bg-surface-2 rounded-full overflow-hidden">
                    <div
                      className="h-full bg-gradient-to-r from-gray-800 to-gray-900 rounded-full transition-all duration-300"
                      style={{ width: `${progress}%` }}
                    />
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
