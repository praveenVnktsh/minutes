import React, { useState, useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { motion, AnimatePresence } from 'framer-motion';
import { toast } from 'sonner';
import {
  ModelInfo,
  ModelStatus,
  getModelIcon,
  formatFileSize,
  getModelPerformanceBadge,
  isQuantizedModel,
  getModelTagline,
  WhisperAPI
} from '../lib/whisper';
import { listenAll, useModelDownload, type ModelDownloadSource } from '@/hooks/useModelDownload';
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from '@/components/ui/accordion';

interface ModelManagerProps {
  selectedModel?: string;
  onModelSelect?: (modelName: string) => void;
  className?: string;
  autoSave?: boolean;
}

const whisperDownloadSource: ModelDownloadSource = {
  subscribe: emit =>
    listenAll([
      listen<{ modelName: string; progress: number }>('model-download-progress', event => {
        emit({ kind: 'progress', model: event.payload.modelName, progress: event.payload.progress });
      }),
      listen<{ modelName: string }>('model-download-complete', event => {
        emit({ kind: 'complete', model: event.payload.modelName });
      }),
      listen<{ modelName: string; error: string }>('model-download-error', event => {
        emit({ kind: 'error', model: event.payload.modelName, error: event.payload.error });
      }),
    ]),
  start: modelName => WhisperAPI.downloadModel(modelName),
  cancel: modelName => WhisperAPI.cancelDownload(modelName),
};

export function ModelManager({
  selectedModel,
  onModelSelect,
  className = '',
  autoSave = false
}: ModelManagerProps) {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [initialized, setInitialized] = useState(false);
  const [hasUserSelection, setHasUserSelection] = useState(false);

  const cancellationReconciliationModelsRef = useRef<Set<string>>(new Set());
  const cancellationReconcileTimersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());

  const setModelStatus = (modelName: string, status: ModelStatus) => {
    setModels(prevModels =>
      prevModels.map(model => (model.name === modelName ? { ...model, status } : model))
    );
  };

  const modelDownload = useModelDownload({
    source: whisperDownloadSource,
    persistKey: 'downloading-models',
    onStart: modelName => {
      setModelStatus(modelName, { Downloading: { progress: 0 } });
      toast.info(`Downloading ${getDisplayName(modelName)}...`, {
        description: 'This may take a few minutes',
        duration: 5000
      });
    },
    onProgress: (modelName, progress) => {
      setModelStatus(modelName, { Downloading: { progress } });
    },
    onComplete: modelName => {
      const model = models.find(m => m.name === modelName);
      clearCancellationReconciliation(modelName);
      setModelStatus(modelName, 'Available');

      toast.success(`${getModelIcon(model?.accuracy || 'Good')} ${getDisplayName(modelName)} ready!`, {
        description: 'Model downloaded and ready to use',
        duration: 4000
      });

      if (onModelSelect) {
        onModelSelect(modelName);
        if (autoSave) {
          saveModelSelection(modelName);
        }
      }
    },
    onError: (modelName, error) => {
      clearCancellationReconciliation(modelName);
      setModelStatus(modelName, { Error: error });

      toast.error(`Failed to download ${getDisplayName(modelName)}`, {
        description: error,
        duration: 6000,
        action: {
          label: 'Retry',
          onClick: () => modelDownload.download(modelName)
        }
      });
    },
    onStartFailed: (modelName, err) => {
      console.error('Download failed:', err);
      setModelStatus(modelName, { Error: err instanceof Error ? err.message : 'Download failed' });
    },
  });

  const settleDownload = modelDownload.settle;

  // Read once: the in-flight set restored from localStorage before the model list loads.
  const persistedDownloadingRef = useRef(modelDownload.downloading);

  const clearCancellationReconciliation = (modelName: string) => {
    cancellationReconciliationModelsRef.current.delete(modelName);
    const timer = cancellationReconcileTimersRef.current.get(modelName);
    if (timer !== undefined) {
      clearTimeout(timer);
      cancellationReconcileTimersRef.current.delete(modelName);
    }
  };

  const reconcileCancellation = (modelName: string) => {
    if (cancellationReconciliationModelsRef.current.has(modelName)) return;

    cancellationReconciliationModelsRef.current.add(modelName);
    modelDownload.markCancelling(modelName);

    const scheduleNextCheck = () => {
      if (!cancellationReconciliationModelsRef.current.has(modelName)) return;
      const timer = setTimeout(() => {
        cancellationReconcileTimersRef.current.delete(modelName);
        void reconcile();
      }, 1000);
      cancellationReconcileTimersRef.current.set(modelName, timer);
    };

    const reconcile = async () => {
      try {
        const modelList = await WhisperAPI.getAvailableModels();
        if (!cancellationReconciliationModelsRef.current.has(modelName)) return;

        const model = modelList.find(candidate => candidate.name === modelName);

        const isStillDownloading = typeof model?.status === 'object' && 'Downloading' in model.status;
        if (model && !isStillDownloading) {
          clearCancellationReconciliation(modelName);
          modelDownload.settle(modelName);
          setModels(modelList);
          toast.info(
            model.status === 'Available'
              ? `${getDisplayName(modelName)} download completed before cancellation`
              : `${getDisplayName(modelName)} download cancelled`,
            { duration: 3000 }
          );
          return;
        }
      } catch (err) {
        console.warn('Failed to reconcile pending download cancellation:', err);
      }

      scheduleNextCheck();
    };

    void reconcile();
  };

  // Stop any cancellation polling on unmount.
  useEffect(() => {
    const reconcilingModels = cancellationReconciliationModelsRef.current;
    const reconcileTimers = cancellationReconcileTimersRef.current;
    return () => {
      for (const timer of reconcileTimers.values()) {
        clearTimeout(timer);
      }
      reconcileTimers.clear();
      reconcilingModels.clear();
    };
  }, []);

  // Initialize models
  useEffect(() => {
    if (initialized) return;

    const initializeModels = async () => {
      try {
        setLoading(true);
        await WhisperAPI.init();
        const modelList = await WhisperAPI.getAvailableModels();

        // Apply persisted downloading states
        const persistedDownloading = persistedDownloadingRef.current;
        const modelsWithDownloadState = modelList.map(model => {
          if (persistedDownloading.has(model.name) && model.status !== 'Available') {
            if (typeof model.status === 'object' && 'Corrupted' in model.status) {
              settleDownload(model.name);
              return model;
            } else if (model.status === 'Missing') {
              settleDownload(model.name);
              return model;
            } else {
              return { ...model, status: { Downloading: { progress: 0 } } as ModelStatus };
            }
          }
          return model;
        });

        setModels(modelsWithDownloadState);
        setInitialized(true);
      } catch (err) {
        console.error('Failed to initialize Whisper:', err);
        setError(err instanceof Error ? err.message : 'Failed to load models');
        toast.error('Failed to load transcription models', {
          description: err instanceof Error ? err.message : 'Unknown error',
          duration: 5000
        });
      } finally {
        setLoading(false);
      }
    };

    initializeModels();
  }, [initialized, selectedModel, onModelSelect, settleDownload]);

  const saveModelSelection = async (modelName: string) => {
    try {
      await invoke('api_save_transcript_config', {
        provider: 'localWhisper',
        model: modelName,
        apiKey: null
      });
    } catch (error) {
      console.error('Failed to save model selection:', error);
    }
  };

  const cancelDownload = async (modelName: string) => {
    const displayName = getDisplayName(modelName);

    try {
      const outcome = await modelDownload.cancel(modelName);
      if (outcome === 'pending') {
        reconcileCancellation(modelName);
        toast.info(`Cancelling ${displayName}...`, {
          description: 'The download is still shutting down. Retry will be available when cleanup completes.',
          duration: 4000
        });
        return;
      }

      // A worker can finish between the user's click and the cancellation command
      // acquiring its owner. Reconcile instead of overwriting a valid Available state.
      reconcileCancellation(modelName);
    } catch (err) {
      console.error('Failed to cancel download:', err);
      toast.error('Failed to cancel download', {
        description: err instanceof Error ? err.message : 'Unknown error',
        duration: 4000
      });
    }
  };

  const downloadModel = async (modelName: string) => {
    if (modelDownload.isBusy(modelName)) return;
    clearCancellationReconciliation(modelName);
    await modelDownload.download(modelName);
  };

  const selectModel = async (modelName: string) => {
    setHasUserSelection(true);

    if (onModelSelect) {
      onModelSelect(modelName);
    }

    if (autoSave) {
      await saveModelSelection(modelName);
    }

    const displayName = getDisplayName(modelName);
    toast.success(`Switched to ${displayName}`, {
      duration: 3000
    });
  };

  const deleteModel = async (modelName: string) => {
    const displayName = getDisplayName(modelName);

    try {
      await WhisperAPI.deleteCorruptedModel(modelName);

      // Refresh models list
      const modelList = await WhisperAPI.getAvailableModels();
      setModels(modelList);

      toast.success(`${displayName} deleted`, {
        description: 'Model removed to free up space',
        duration: 3000
      });

      // If deleted model was selected, clear selection
      if (selectedModel === modelName && onModelSelect) {
        onModelSelect('');
      }
    } catch (err) {
      console.error('Failed to delete model:', err);
      toast.error(`Failed to delete ${displayName}`, {
        description: err instanceof Error ? err.message : 'Delete failed',
        duration: 4000
      });
    }
  };

  const getDisplayName = (modelName: string): string => {
    const modelNameMapping: { [key: string]: string } = {
      "small": "Small",
      "medium-q5_0": "Medium",
      "large-v3-q5_0": "Large V3 Compressed",
      "large-v3-turbo": "Large V3 Turbo",
      "large-v3": "Large V3"
    };

    const basicModelNames = ["small", "medium-q5_0", "large-v3-q5_0", "large-v3-turbo", "large-v3"];
    if (basicModelNames.includes(modelName)) {
      return modelNameMapping[modelName] || modelName;
    }
    return `Whisper ${modelName}`;
  };

  if (loading) {
    return (
      <div className={`space-y-3 ${className}`}>
        <div className="animate-pulse space-y-3">
          <div className="h-20 bg-surface-2 rounded-lg"></div>
          <div className="h-20 bg-surface-2 rounded-lg"></div>
          <div className="h-20 bg-surface-2 rounded-lg"></div>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className={`bg-red-50 border border-red-200 rounded-lg p-4 ${className}`}>
        <p className="text-sm text-red-800">Failed to load models</p>
        <p className="text-xs text-red-600 mt-1">{error}</p>
      </div>
    );
  }

  const basicModelNames = ["small", "medium-q5_0", "large-v3-q5_0", "large-v3-turbo", "large-v3"];
  const basicModels = models.filter(m => basicModelNames.includes(m.name))
    .sort((a, b) => basicModelNames.indexOf(a.name) - basicModelNames.indexOf(b.name));
  const advancedModels = models.filter(m => !basicModelNames.includes(m.name));

  return (
    <div className={`space-y-3 ${className}`}>
      {/* Basic Models */}
      <div className="space-y-3">
        {basicModels.map((model) => {
          const isRecommended = model.name === 'base';
          return (
            <ModelCard
              key={model.name}
              model={model}
              isSelected={selectedModel === model.name}
              isRecommended={isRecommended}
              onSelect={() => {
                if (model.status === 'Available') {
                  selectModel(model.name);
                }
              }}
              onDownload={() => downloadModel(model.name)}
              onCancel={() => cancelDownload(model.name)}
              onDelete={() => deleteModel(model.name)}
              isDownloading={modelDownload.downloading.has(model.name)}
              isCancelling={modelDownload.cancelling.has(model.name)}
              displayName={getDisplayName(model.name)}
            />
          );
        })}
      </div>

      {/* Advanced Models */}
      {advancedModels.length > 0 && (
        <Accordion type="single" collapsible className="w-full">
          <AccordionItem value="advanced-models">
            <AccordionTrigger>
              <span className='text-lg'>Advanced Models</span>
            </AccordionTrigger>
            <AccordionContent>
              <div className="space-y-3 pt-4">
                {advancedModels.map((model) => (
                  <ModelCard
                    key={model.name}
                    model={model}
                    isSelected={selectedModel === model.name}
                    isRecommended={false}
                    onSelect={() => {
                      if (model.status === 'Available') {
                        selectModel(model.name);
                      }
                    }}
                    onDownload={() => downloadModel(model.name)}
                    onCancel={() => cancelDownload(model.name)}
                    onDelete={() => deleteModel(model.name)}
                    isDownloading={modelDownload.downloading.has(model.name)}
                    isCancelling={modelDownload.cancelling.has(model.name)}
                    displayName={getDisplayName(model.name)}
                  />
                ))}
              </div>
            </AccordionContent>
          </AccordionItem>
        </Accordion>
      )}

      {/* Helper text */}
      {selectedModel && (
        <motion.div
          initial={{ opacity: 0, y: -5 }}
          animate={{ opacity: 1, y: 0 }}
          className="text-xs text-ink-muted text-center pt-2"
        >
          Using {getDisplayName(selectedModel)} for transcription
        </motion.div>
      )}
    </div>
  );
}

// Model Card Component
interface ModelCardProps {
  model: ModelInfo;
  isSelected: boolean;
  isRecommended: boolean;
  onSelect: () => void;
  onDownload: () => void;
  onCancel: () => void;
  onDelete: () => void;
  isDownloading: boolean;
  isCancelling: boolean;
  displayName: string;
}

function ModelCard({
  model,
  isSelected,
  isRecommended,
  onSelect,
  onDownload,
  onCancel,
  onDelete,
  isDownloading,
  isCancelling,
  displayName
}: ModelCardProps) {
  const [isHovered, setIsHovered] = useState(false);

  const isAvailable = model.status === 'Available';
  const isMissing = model.status === 'Missing';
  const isError = typeof model.status === 'object' && 'Error' in model.status;
  const isCorrupted = typeof model.status === 'object' && 'Corrupted' in model.status;
  const downloadProgress =
    typeof model.status === 'object' && 'Downloading' in model.status
      ? model.status.Downloading.progress
      : null;

  return (
    <motion.div
      initial={{ opacity: 0, y: 5 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.2 }}
      onMouseEnter={() => setIsHovered(true)}
      onMouseLeave={() => setIsHovered(false)}
      className={`
        relative rounded-lg border-2 transition-all cursor-pointer
        ${isSelected && isAvailable
          ? 'border-blue-500 bg-blue-50'
          : isAvailable
            ? 'border-hairline hover:border-hairline bg-surface-raised'
            : 'border-hairline bg-surface-2'
        }
        ${isAvailable ? '' : 'cursor-default'}
      `}
      onClick={() => {
        if (isAvailable) onSelect();
      }}
    >
      {/* Recommended Badge */}
      {isRecommended && (
        <div className="absolute -top-2 -right-2 bg-blue-600 text-white text-xs px-2 py-0.5 rounded-full font-medium">
          Recommended
        </div>
      )}

      <div className="p-3">
        <div className="flex items-start justify-between mb-2">
          <div className="flex-1">
            {/* Model Name and Tagline */}
            <div className="flex items-center gap-2 flex-wrap mb-2">
              <span className="text-2xl">{getModelIcon(model.accuracy)}</span>
              <h3 className="font-semibold text-ink">{displayName}</h3>
              <span className="text-sm text-ink-muted">•</span>
              <span className="text-sm text-ink-muted">{getModelTagline(model.name, model.speed, model.accuracy)}</span>
              {isSelected && isAvailable && (
                <motion.span
                  initial={{ scale: 0 }}
                  animate={{ scale: 1 }}
                  className="bg-blue-600 text-white px-2 py-0.5 rounded-full text-xs font-medium flex items-center gap-1"
                >
                  ✓
                </motion.span>
              )}
              {isQuantizedModel(model.name) && (
                <span className={`px-2 py-0.5 rounded-full text-xs ${getModelPerformanceBadge(model.name).color === 'green'
                  ? 'bg-green-100 text-green-700'
                  : getModelPerformanceBadge(model.name).color === 'orange'
                    ? 'bg-orange-100 text-orange-700'
                    : 'bg-surface-2 text-ink'
                  }`}>
                  {getModelPerformanceBadge(model.name).label}
                </span>
              )}
            </div>

            {/* Model Specs */}
            <div className="flex items-center space-x-4 text-sm text-ink-muted ml-9 mt-1.5">
              <span className="flex items-center space-x-1">
                <span>📦</span>
                <span>{formatFileSize(model.size_mb)}</span>
              </span>
              <span className="flex items-center space-x-1">
                <span>🎯</span>
                <span>{model.accuracy} accuracy</span>
              </span>
              <span className="flex items-center space-x-1">
                <span>⚡</span>
                <span>{model.speed} processing</span>
              </span>
            </div>
          </div>

          {/* Status/Action */}
          <div className="ml-4 flex items-center gap-2">
            {isAvailable && (
              <>
                <div className="flex items-center gap-1.5 text-green-600">
                  <div className="w-2 h-2 bg-green-500 rounded-full"></div>
                  <span className="text-xs font-medium">Ready</span>
                </div>
                <AnimatePresence>
                  {isHovered && (
                    <motion.button
                      initial={{ opacity: 0, scale: 0.8 }}
                      animate={{ opacity: 1, scale: 1 }}
                      exit={{ opacity: 0, scale: 0.8 }}
                      transition={{ duration: 0.15 }}
                      onClick={(e) => {
                        e.stopPropagation();
                        onDelete();
                      }}
                      className="text-ink-subtle hover:text-red-600 transition-colors p-1"
                      title="Delete model to free up space"
                    >
                      <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" />
                      </svg>
                    </motion.button>
                  )}
                </AnimatePresence>
              </>
            )}

            {isMissing && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onDownload();
                }}
                className="bg-blue-600 text-white px-3 py-1.5 rounded-md text-sm font-medium hover:bg-blue-700 transition-colors"
              >
                Download
              </button>
            )}

            {downloadProgress === null && isError && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onDownload();
                }}
                className="bg-red-600 text-white px-3 py-1.5 rounded-md text-sm font-medium hover:bg-red-700 transition-colors"
              >
                Retry
              </button>
            )}

            {isCorrupted && (
              <div className="flex gap-2">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onDelete();
                  }}
                  className="bg-orange-600 text-white px-3 py-1.5 rounded-md text-sm font-medium hover:bg-orange-700 transition-colors"
                >
                  Delete
                </button>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onDownload();
                  }}
                  className="bg-blue-600 text-white px-3 py-1.5 rounded-md text-sm font-medium hover:bg-blue-700 transition-colors"
                >
                  Re-download
                </button>
              </div>
            )}
          </div>
        </div>

        {/* Full-width Download Progress Bar - PROMINENT */}
        {downloadProgress !== null && (
          <motion.div
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: 'auto' }}
            exit={{ opacity: 0, height: 0 }}
            className="mt-3 pt-3 border-t border-hairline"
          >
            <div className="flex items-center justify-between mb-2">
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium text-blue-600">
                  {isCancelling ? 'Cancelling…' : 'Downloading...'}
                </span>
                {!isCancelling && (
                  <span className="text-sm font-semibold text-blue-600">{Math.round(downloadProgress)}%</span>
                )}
              </div>
              {isCancelling ? (
                <span className="text-xs text-ink-muted font-medium px-2 py-1">
                  Cancellation requested
                </span>
              ) : (
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onCancel();
                  }}
                  className="text-xs text-ink-muted hover:text-red-600 font-medium transition-colors px-2 py-1 rounded hover:bg-red-50"
                  title="Cancel download"
                >
                  Cancel
                </button>
              )}
            </div>
            <div className="w-full h-2 bg-surface-2 rounded-full overflow-hidden">
              <motion.div
                className="h-full bg-gradient-to-r from-blue-500 to-blue-600 rounded-full"
                initial={{ width: 0 }}
                animate={{ width: `${downloadProgress}%` }}
                transition={{ duration: 0.3, ease: 'easeOut' }}
              />
            </div>
            <p className="text-xs text-ink-muted mt-1">
              {model.size_mb ? (
                <>
                  {formatFileSize(model.size_mb * downloadProgress / 100)} / {formatFileSize(model.size_mb)}
                </>
              ) : (
                'Downloading...'
              )}
            </p>
          </motion.div>
        )}
      </div>
    </motion.div>
  );
}
