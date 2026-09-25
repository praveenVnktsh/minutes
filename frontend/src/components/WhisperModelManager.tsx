import React, { useState, useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'framer-motion';
import { toast } from 'sonner';
import {
  ModelInfo,
  ModelStatus,
  getModelIcon,
  formatFileSize,
  getModelTagline,
  WhisperAPI
} from '../lib/whisper';
import { listenAll, useModelDownload, type ModelDownloadSource } from '@/hooks/useModelDownload';
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from '@/components/ui/accordion';
import { DEFAULT_WHISPER_MODEL } from '@/constants/modelDefaults';
import { TranscriptModelCard } from './TranscriptModelCard';

interface ModelManagerProps {
  selectedModel?: string;
  onModelSelect?: (modelName: string) => void;
  className?: string;
  autoSave?: boolean;
}

const WHISPER_DISPLAY_NAMES: Record<string, string> = {
  'large-v3-turbo-q5_0': 'Large V3 Turbo',
  'large-v3-q5_0': 'Large V3',
};

function whisperDetails(model: ModelInfo): string {
  return `${getModelTagline(model.name, model.speed, model.accuracy)} • ${formatFileSize(model.size_mb)}`;
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
    return WHISPER_DISPLAY_NAMES[modelName] ?? `Whisper ${modelName}`;
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
      <div className={`rounded-lg border border-error/30 bg-error-subtle p-4 ${className}`}>
        <p className="text-sm font-medium text-error">Failed to load models</p>
        <p className="mt-1 text-xs text-error">{error}</p>
      </div>
    );
  }

  // Legacy models are no longer offered, so only list the ones already on disk.
  const basicModels = models.filter(m => !m.legacy);
  const advancedModels = models.filter(m => m.legacy && m.status !== 'Missing');

  return (
    <div className={`space-y-3 ${className}`}>
      {/* Basic Models */}
      <div className="space-y-3">
        {basicModels.map((model) => {
          const isRecommended = model.name === DEFAULT_WHISPER_MODEL;
          return (
            <TranscriptModelCard
              key={model.name}
              displayName={getDisplayName(model.name)}
              details={whisperDetails(model)}
              status={model.status}
              sizeMb={model.size_mb}
              isSelected={selectedModel === model.name}
              isRecommended={isRecommended}
              isCancelling={modelDownload.cancelling.has(model.name)}
              onSelect={() => selectModel(model.name)}
              onDownload={() => downloadModel(model.name)}
              onCancel={() => cancelDownload(model.name)}
              onDelete={() => deleteModel(model.name)}
            />
          );
        })}
      </div>

      {/* Advanced Models */}
      {advancedModels.length > 0 && (
        <Accordion type="single" collapsible className="w-full">
          <AccordionItem value="advanced-models">
            <AccordionTrigger>
              <span className='text-lg'>Older Models</span>
            </AccordionTrigger>
            <AccordionContent>
              <div className="space-y-3 pt-4">
                {advancedModels.map((model) => (
                  <TranscriptModelCard
                    key={model.name}
                    displayName={getDisplayName(model.name)}
                    details={whisperDetails(model)}
                    status={model.status}
                    sizeMb={model.size_mb}
                    isSelected={selectedModel === model.name}
                    isRecommended={false}
                    isCancelling={modelDownload.cancelling.has(model.name)}
                    onSelect={() => selectModel(model.name)}
                    onDownload={() => downloadModel(model.name)}
                    onCancel={() => cancelDownload(model.name)}
                    onDelete={() => deleteModel(model.name)}
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
