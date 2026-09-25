import React, { useState, useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'framer-motion';
import { toast } from 'sonner';
import {
  ModelStatus,
  ParakeetAPI,
  ParakeetDownloadProgressEvent,
  ParakeetModelInfo,
  getModelDisplayInfo,
  getModelDisplayName
} from '../lib/parakeet';
import { DEFAULT_PARAKEET_MODEL } from '@/constants/modelDefaults';
import { formatBytes } from '@/lib/download-display';
import { TranscriptModelCard } from './TranscriptModelCard';
import {
  ModelDownloadSource,
  UseModelDownloadOptions,
  listenAll,
  useModelDownload
} from '../hooks/useModelDownload';

interface ParakeetModelManagerProps {
  selectedModel?: string;
  onModelSelect?: (modelName: string) => void;
  className?: string;
  autoSave?: boolean;
}

// Module-level so it is referentially stable across renders/remounts.
function parakeetDetails(model: ParakeetModelInfo): string {
  const tagline = getModelDisplayInfo(model.name)?.tagline || model.description || '';
  return `${tagline} • ${formatBytes(model.size_bytes)}`;
}

const parakeetDownloadSource: ModelDownloadSource = {
  subscribe: emit =>
    listenAll([
      listen<ParakeetDownloadProgressEvent>('parakeet-model-download-progress', event => {
        const { modelName, progress, status, downloaded_mb, total_mb, speed_mbps } = event.payload;
        if (status === 'cancelled') {
          emit({ kind: 'cancelled', model: modelName });
          return;
        }
        const detail =
          downloaded_mb !== undefined && total_mb !== undefined
            ? { downloadedMb: downloaded_mb, totalMb: total_mb, speedMbps: speed_mbps ?? 0 }
            : undefined;
        emit({ kind: 'progress', model: modelName, progress, detail, completed: status === 'completed' });
      }),
      listen<{ modelName: string }>('parakeet-model-download-complete', event => {
        emit({ kind: 'complete', model: event.payload.modelName });
      }),
      listen<{ modelName: string; error: string }>('parakeet-model-download-error', event => {
        emit({ kind: 'error', model: event.payload.modelName, error: event.payload.error });
      })
    ]),
  start: model => ParakeetAPI.downloadModel(model),
  cancel: model => ParakeetAPI.cancelDownload(model)
};

export function ParakeetModelManager({
  selectedModel,
  onModelSelect,
  className = '',
  autoSave = false
}: ParakeetModelManagerProps) {
  const [models, setModels] = useState<ParakeetModelInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [initialized, setInitialized] = useState(false);

  // Refs for stable callbacks
  const onModelSelectRef = useRef(onModelSelect);
  const autoSaveRef = useRef(autoSave);

  useEffect(() => {
    onModelSelectRef.current = onModelSelect;
    autoSaveRef.current = autoSave;
  }, [onModelSelect, autoSave]);

  const latestStatusByModelRef = useRef<Map<string, ModelStatus>>(new Map());

  // `handlers` closes over `hook` for the Retry toast action; the closures
  // below only run later (after `hook` is assigned), so this is safe.
  const handlers: UseModelDownloadOptions = {
    source: parakeetDownloadSource,
    onStart: model => {
      const displayInfo = getModelDisplayInfo(model);
      const displayName = displayInfo?.friendlyName || model;

      setModels(prevModels =>
        prevModels.map(m =>
          m.name === model ? { ...m, status: { Downloading: { progress: 0 } } as ModelStatus } : m
        )
      );

      toast.info(`Downloading ${displayName}...`, {
        description: 'This may take a few minutes',
        duration: 5000 // Auto-dismiss after 5 seconds
      });
    },
    onProgress: (model, progress, event) => {
      const modelStatus: ModelStatus = event.completed ? 'Available' : { Downloading: { progress } };
      latestStatusByModelRef.current.set(model, modelStatus);
      setModels(prevModels => prevModels.map(m => (m.name === model ? { ...m, status: modelStatus } : m)));
    },
    onComplete: model => {
      const displayInfo = getModelDisplayInfo(model);
      const displayName = displayInfo?.friendlyName || model;
      latestStatusByModelRef.current.set(model, 'Available');

      setModels(prevModels =>
        prevModels.map(m => (m.name === model ? { ...m, status: 'Available' as ModelStatus } : m))
      );

      toast.success(`${displayInfo?.icon || '✓'} ${displayName} ready!`, {
        description: 'Model downloaded and ready to use',
        duration: 4000
      });

      // Auto-select after download using stable refs
      if (onModelSelectRef.current) {
        onModelSelectRef.current(model);
        if (autoSaveRef.current) {
          saveModelSelection(model);
        }
      }
    },
    onError: (model, error) => {
      const displayInfo = getModelDisplayInfo(model);
      const displayName = displayInfo?.friendlyName || model;
      latestStatusByModelRef.current.set(model, { Error: error });

      setModels(prevModels =>
        prevModels.map(m => (m.name === model ? { ...m, status: { Error: error } as ModelStatus } : m))
      );

      toast.error(`Failed to download ${displayName}`, {
        description: error,
        duration: 6000,
        action: {
          label: 'Retry',
          onClick: () => hook.download(model)
        }
      });
    },
    onCancelled: model => {
      latestStatusByModelRef.current.set(model, 'Missing');

      setModels(prevModels =>
        prevModels.map(m => (m.name === model ? { ...m, status: 'Missing' as ModelStatus } : m))
      );

      toast.info(`${getModelDisplayName(model)} download cancelled`, {
        duration: 3000
      });
    },
    onStartFailed: (model, err) => {
      console.error('Download failed:', err);
      const errorMessage = err instanceof Error ? err.message : 'Download failed';
      setModels(prev => prev.map(m => (m.name === model ? { ...m, status: { Error: errorMessage } } : m)));
    }
  };

  const hook = useModelDownload(handlers);

  // Initialize and load models
  useEffect(() => {
    if (initialized || !hook.listenersReady) return;

    const initializeModels = async () => {
      try {
        setLoading(true);
        await ParakeetAPI.init();
        const modelList = await ParakeetAPI.getAvailableModels();
        setModels(modelList.map(model => ({
          ...model,
          status: latestStatusByModelRef.current.get(model.name) ?? model.status
        })));

        setInitialized(true);
      } catch (err) {
        console.error('Failed to initialize Parakeet:', err);
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
  }, [initialized, hook.listenersReady, selectedModel, onModelSelect]);

  // Surface a failed listener registration the same way the old inline setup did.
  useEffect(() => {
    if (hook.listenError) {
      setError(hook.listenError);
      setLoading(false);
    }
  }, [hook.listenError]);

  const saveModelSelection = async (modelName: string) => {
    try {
      await invoke('api_save_transcript_config', {
        provider: 'parakeet',
        model: modelName,
        apiKey: null
      });
    } catch (error) {
      console.error('Failed to save model selection:', error);
    }
  };

  const cancelDownload = async (modelName: string) => {
    const displayInfo = getModelDisplayInfo(modelName);
    const displayName = displayInfo?.friendlyName || modelName;

    try {
      const outcome = await hook.cancel(modelName);
      if (outcome === 'pending') {
        toast.info(`Cancelling ${displayName}...`, {
          description: 'The download is still shutting down. Retry will be available when cleanup completes.',
          duration: 4000
        });
      }
    } catch (err) {
      console.error('Failed to cancel download:', err);
      toast.error('Failed to cancel download', {
        description: err instanceof Error ? err.message : 'Unknown error',
        duration: 4000
      });
    }
  };

  const selectModel = async (modelName: string) => {
    if (onModelSelect) {
      onModelSelect(modelName);
    }

    if (autoSave) {
      await saveModelSelection(modelName);
    }

    const displayInfo = getModelDisplayInfo(modelName);
    const displayName = displayInfo?.friendlyName || modelName;
    toast.success(`Switched to ${displayName}`, {
      duration: 3000
    });
  };

  const deleteModel = async (modelName: string) => {
    const displayInfo = getModelDisplayInfo(modelName);
    const displayName = displayInfo?.friendlyName || modelName;

    try {
      await ParakeetAPI.deleteCorruptedModel(modelName);

      // Refresh models list
      const modelList = await ParakeetAPI.getAvailableModels();
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

  if (loading) {
    return (
      <div className={`space-y-3 ${className}`}>
        <div className="animate-pulse space-y-3">
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

  const recommendedModel = models.find(m => m.name === DEFAULT_PARAKEET_MODEL);
  // Legacy models are no longer offered, so only list the ones already on disk.
  const otherModels = models.filter(m =>
    m.name !== DEFAULT_PARAKEET_MODEL && (!m.legacy || m.status !== 'Missing')
  );

  return (
    <div className={`space-y-3 ${className}`}>
      {/* Recommended Model */}
      {recommendedModel && (
        <TranscriptModelCard
          displayName={getModelDisplayName(recommendedModel.name)}
          details={parakeetDetails(recommendedModel)}
          status={recommendedModel.status}
          sizeMb={recommendedModel.size_mb}
          isSelected={selectedModel === recommendedModel.name}
          isRecommended={true}
          isCancelling={hook.cancelling.has(recommendedModel.name)}
          onSelect={() => selectModel(recommendedModel.name)}
          onDownload={() => hook.download(recommendedModel.name)}
          onCancel={() => cancelDownload(recommendedModel.name)}
          onDelete={() => deleteModel(recommendedModel.name)}
        />
      )}

      {/* Other Models */}
      {otherModels.length > 0 && (
        <div className="space-y-3">
          {otherModels.map(model => (
            <TranscriptModelCard
              key={model.name}
              displayName={getModelDisplayName(model.name)}
              details={parakeetDetails(model)}
              status={model.status}
              sizeMb={model.size_mb}
              isSelected={selectedModel === model.name}
              isRecommended={false}
              isCancelling={hook.cancelling.has(model.name)}
              onSelect={() => selectModel(model.name)}
              onDownload={() => hook.download(model.name)}
              onCancel={() => cancelDownload(model.name)}
              onDelete={() => deleteModel(model.name)}
            />
          ))}
        </div>
      )}

      {/* Helper text */}
      {selectedModel && (
        <motion.div
          initial={{ opacity: 0, y: -5 }}
          animate={{ opacity: 1, y: 0 }}
          className="text-xs text-ink-muted text-center pt-2"
        >
          Using {getModelDisplayName(selectedModel)} for transcription
        </motion.div>
      )}
    </div>
  );
}
