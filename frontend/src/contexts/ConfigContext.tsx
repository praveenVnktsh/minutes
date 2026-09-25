'use client';

import React, { createContext, useContext, useState, useEffect, useCallback, useMemo, ReactNode, useRef } from 'react';
import type { TranscriptModelProps } from '@/components/TranscriptSettings';
import type { SelectedDevices } from '@/components/DeviceSelection';
import { ConfigSaveInProgressError, LatestRequestGuard, configService } from '@/services/configService';
import { invoke } from '@tauri-apps/api/core';
import Analytics from '@/lib/analytics';
import { BetaFeatures, BetaFeatureKey, loadBetaFeatures, saveBetaFeatures } from '@/types/betaFeatures';
import { DEFAULT_MODEL_CONFIG, mergeProviderKeyHydration, ModelConfig, ProviderApiKeys } from '@/types/modelConfig';
import { DEFAULT_PARAKEET_MODEL } from '@/constants/modelDefaults';

export interface OllamaModel {
  name: string;
  id: string;
  size: string;
  modified: string;
}

export interface StorageLocations {
  database: string;
  models: string;
  recordings: string;
}

export interface NotificationSettings {
  recording_notifications: boolean;
  time_based_reminders: boolean;
  meeting_reminders: boolean;
  respect_do_not_disturb: boolean;
  notification_sound: boolean;
  system_permission_granted: boolean;
  consent_given: boolean;
  manual_dnd_mode: boolean;
  notification_preferences: {
    show_recording_started: boolean;
    show_recording_stopped: boolean;
    show_recording_paused: boolean;
    show_recording_resumed: boolean;
    show_transcription_complete: boolean;
    show_meeting_reminders: boolean;
    show_system_errors: boolean;
    meeting_reminder_minutes: number[];
  };
}

interface ConfigContextType {
  // Model configuration
  modelConfig: ModelConfig;
  setModelConfig: (config: ModelConfig | ((prev: ModelConfig) => ModelConfig)) => void;
  commitModelConfig: (config: ModelConfig) => Promise<ModelConfig>;
  isModelConfigLoading: boolean;
  isModelConfigSaving: boolean;
  modelConfigSaveError: Error | null;

  // Transcript model configuration
  transcriptModelConfig: TranscriptModelProps;
  setTranscriptModelConfig: (config: TranscriptModelProps | ((prev: TranscriptModelProps) => TranscriptModelProps)) => void;

  // Device configuration
  selectedDevices: SelectedDevices;
  setSelectedDevices: (devices: SelectedDevices) => void;

  // Language preference
  selectedLanguage: string;
  setSelectedLanguage: (lang: string) => void;

  // UI preferences
  showConfidenceIndicator: boolean;
  toggleConfidenceIndicator: (checked: boolean) => void;

  // Beta features
  betaFeatures: BetaFeatures;
  toggleBetaFeature: (featureKey: BetaFeatureKey, enabled: boolean) => void;

  // Ollama models
  models: OllamaModel[];
  modelOptions: Record<ModelConfig['provider'], string[]>;
  error: string;

  // Summary configuration
  isAutoSummary: boolean;
  toggleIsAutoSummary: (checked: boolean) => void;

  // Provider-specific API keys
  providerApiKeys: ProviderApiKeys;
  updateProviderApiKey: (provider: string, apiKey: string | null) => void;

  // Preference settings (lazy loaded)
  notificationSettings: NotificationSettings | null;
  storageLocations: StorageLocations | null;
  isLoadingPreferences: boolean;
  loadPreferences: () => Promise<void>;
  updateNotificationSettings: (settings: NotificationSettings) => Promise<void>;
}

const ConfigContext = createContext<ConfigContextType | undefined>(undefined);

function isProviderWithApiKey(provider: string): provider is keyof ProviderApiKeys {
  return ['claude', 'groq', 'openai', 'openrouter'].includes(provider);
}

export function ConfigProvider({ children }: { children: ReactNode }) {
  // Model configuration state
  const [modelConfig, setModelConfigState] = useState<ModelConfig>(DEFAULT_MODEL_CONFIG);
  const [isModelConfigLoading, setIsModelConfigLoading] = useState(true);
  const [isModelConfigSaving, setIsModelConfigSaving] = useState(false);
  const [modelConfigSaveError, setModelConfigSaveError] = useState<Error | null>(null);
  const configRequestRef = useRef(new LatestRequestGuard());
  const keyRevisionRef = useRef<Record<keyof ProviderApiKeys, number>>({
    claude: 0,
    groq: 0,
    openai: 0,
    openrouter: 0,
  });
  const saveInProgressRef = useRef(false);
  const reconciliationPendingRef = useRef(false);

  // Compatibility setter for existing forms. New forms should keep local drafts
  // and call commitModelConfig instead of publishing edits through this setter.
  const setModelConfig = useCallback<ConfigContextType['setModelConfig']>((next) => {
    configRequestRef.current.invalidate();
    setModelConfigState(next);
  }, []);


  // Transcript model configuration state
  const [transcriptModelConfig, setTranscriptModelConfig] = useState<TranscriptModelProps>({
    provider: 'parakeet',
    model: DEFAULT_PARAKEET_MODEL,
    apiKey: null
  });

  // Provider-specific API keys (loaded once at startup)
  // Note: Gemini omitted for now - add when UI support is added
  const [providerApiKeys, setProviderApiKeys] = useState<ProviderApiKeys>({
    claude: null,
    groq: null,
    openai: null,
    openrouter: null,
  });

  // Ollama models list and error state
  const [models, setModels] = useState<OllamaModel[]>([]);
  const [error, setError] = useState<string>('');

  // Device configuration state
  const [selectedDevices, setSelectedDevices] = useState<SelectedDevices>({
    micDevice: null,
    systemDevice: null
  });

  // Language preference state
  const [selectedLanguage, setSelectedLanguage] = useState<string>(() => {
    if (typeof window !== 'undefined') {
      const saved = localStorage.getItem('primaryLanguage');
      return saved || 'auto';
    }
    return 'auto';
  });

  // UI preferences state
  const [showConfidenceIndicator, setShowConfidenceIndicator] = useState<boolean>(() => {
    if (typeof window !== 'undefined') {
      const saved = localStorage.getItem('showConfidenceIndicator');
      return saved !== null ? saved === 'true' : true;
    }
    return true;
  });

  // Summary configs
  const [isAutoSummary, setisAutoSummary] = useState<boolean>(() => {
    if (typeof window !== 'undefined') {
      const saved = localStorage.getItem('isAutoSummary');
      return saved !== null ? saved === 'true' : true
    }
    return true;
  });

  // Beta features state (localStorage)
  const [betaFeatures, setBetaFeatures] = useState<BetaFeatures>(() => {
    return loadBetaFeatures();
  });

  // Preference settings state (lazy loaded)
  const [notificationSettings, setNotificationSettings] = useState<NotificationSettings | null>(null);
  const [storageLocations, setStorageLocations] = useState<StorageLocations | null>(null);
  const [isLoadingPreferences, setIsLoadingPreferences] = useState(false);
  const preferencesLoadedRef = useRef(false);
  const isLoadingRef = useRef(false);

  // Load Ollama models (uses saved endpoint, re-runs when endpoint changes after config load)
  useEffect(() => {
    const loadModels = async () => {
      try {
        const endpoint = modelConfig.ollamaEndpoint || null;
        const modelList = await invoke<OllamaModel[]>('get_ollama_models', { endpoint });
        setModels(modelList);
        setError('');
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to load Ollama models');
        console.error('Error loading models:', err);
      }
    };
    loadModels();
  }, [modelConfig.ollamaEndpoint]);

  // Load transcript configuration on mount
  useEffect(() => {
    const loadTranscriptConfig = async () => {
      try {
        const config = await configService.getTranscriptConfig();
        if (config) {
          setTranscriptModelConfig({
            provider: config.provider || 'parakeet',
            model: config.model || DEFAULT_PARAKEET_MODEL,
            apiKey: config.apiKey || null
          });
        }
      } catch (error) {
        console.error('[ConfigContext] Failed to load transcript config:', error);
      }
    };
    loadTranscriptConfig();
  }, []);

  // Sync language preference to Rust on mount (fixes startup desync bug)
  useEffect(() => {
    if (selectedLanguage) {
      invoke('set_language_preference', { language: selectedLanguage })
        .then(() => {
          console.log('[ConfigContext] Synced language preference to Rust on startup:', selectedLanguage);
        })
        .catch(err => {
          console.error('[ConfigContext] Failed to sync language preference to Rust on startup:', err);
        });
    }
  }, []); 

  const cacheProviderModel = useCallback((config: ModelConfig) => {
    if (typeof window === 'undefined' || !config.model) return;
    try {
      const map = JSON.parse(localStorage.getItem('providerModelMap') || '{}');
      map[config.provider] = config.model;
      localStorage.setItem('providerModelMap', JSON.stringify(map));
    } catch {
      // This cache is advisory; native persistence remains authoritative.
    }
  }, []);

  const reconcileModelConfig = useCallback(async () => {
    const request = configRequestRef.current.begin();
    try {
      const data = await configService.loadModelConfig();
      if (!configRequestRef.current.isCurrent(request) || saveInProgressRef.current) return;
      if (data?.provider) {
        const loadedConfig = { ...DEFAULT_MODEL_CONFIG, ...data };
        setModelConfigState(loadedConfig);
        cacheProviderModel(loadedConfig);
      }
    } finally {
      if (configRequestRef.current.isCurrent(request)) setIsModelConfigLoading(false);
    }
  }, [cacheProviderModel]);

  const hydrateProviderApiKeys = useCallback(async () => {
    const providers: Array<keyof ProviderApiKeys> = ['claude', 'groq', 'openai', 'openrouter'];
    for (const provider of providers) keyRevisionRef.current[provider] += 1;
    const revisions = { ...keyRevisionRef.current };
    const results = await Promise.all(providers.map(async (provider) => {
      try {
        return { provider, apiKey: await configService.getProviderApiKey(provider), succeeded: true };
      } catch {
        return { provider, apiKey: null, succeeded: false };
      }
    }));

    setProviderApiKeys(previous => mergeProviderKeyHydration(
      previous,
      results,
      revisions,
      keyRevisionRef.current,
    ));
  }, []);

  const requestReconciliation = useCallback(() => {
    if (saveInProgressRef.current) {
      reconciliationPendingRef.current = true;
      return;
    }
    void reconcileModelConfig().catch(error => {
      console.error('[ConfigContext] Failed to reconcile model config:', error);
    });
    void hydrateProviderApiKeys();
  }, [hydrateProviderApiKeys, reconcileModelConfig]);

  useEffect(() => {
    void reconcileModelConfig().catch(error => {
      console.error('[ConfigContext] Failed to load model config:', error);
    });
    void hydrateProviderApiKeys();
  }, [hydrateProviderApiKeys, reconcileModelConfig]);

  // Listen for model config updates from other components
  useEffect(() => {
    let disposed = false;
    const registered: Array<() => void> = [];
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        for (const eventName of ['model-config-invalidated', 'model-config-updated']) {
          const unlisten = await listen(eventName, requestReconciliation);
          if (disposed) unlisten();
          else registered.push(unlisten);
        }
      } catch (error) {
        registered.splice(0).forEach(unlisten => unlisten());
        if (!disposed) console.error('[ConfigContext] Failed to listen for model config changes:', error);
      }
    })();

    return () => {
      disposed = true;
      registered.splice(0).forEach(unlisten => unlisten());
    };
  }, [requestReconciliation]);

  const commitModelConfig = useCallback(async (draft: ModelConfig): Promise<ModelConfig> => {
    if (saveInProgressRef.current) throw new ConfigSaveInProgressError();

    saveInProgressRef.current = true;
    configRequestRef.current.invalidate();
    reconciliationPendingRef.current = true;
    setIsModelConfigSaving(true);
    setModelConfigSaveError(null);
    try {
      const committed = await configService.commitModelConfig(draft);
      setModelConfigState(committed);

      if (
        isProviderWithApiKey(committed.provider)
        && committed.apiKey !== undefined
      ) {
        const provider = committed.provider;
        keyRevisionRef.current[provider] += 1;
        setProviderApiKeys(previous => ({
          ...previous,
          [provider]: committed.apiKey ?? null,
        }));
      }

      cacheProviderModel(committed);

      return committed;
    } catch (error) {
      const saveError = error instanceof Error ? error : new Error(String(error));
      setModelConfigSaveError(saveError);
      throw saveError;
    } finally {
      saveInProgressRef.current = false;
      setIsModelConfigSaving(false);
      if (reconciliationPendingRef.current) {
        reconciliationPendingRef.current = false;
        requestReconciliation();
      }
    }
  }, [cacheProviderModel, requestReconciliation]);

  // Load device preferences on mount
  useEffect(() => {
    const loadDevicePreferences = async () => {
      try {
        const prefs = await configService.getRecordingPreferences();
        if (prefs && (prefs.preferred_mic_device || prefs.preferred_system_device)) {
          setSelectedDevices({
            micDevice: prefs.preferred_mic_device,
            systemDevice: prefs.preferred_system_device
          });
          console.log('Loaded device preferences:', prefs);
        }
      } catch (error) {
        console.log('No device preferences found or failed to load:', error);
      }
    };
    loadDevicePreferences();
  }, []);

  // Calculate model options based on available models
  const modelOptions: Record<ModelConfig['provider'], string[]> = {
    ollama: models.map(model => model.name),
    claude: ['claude-sonnet-5', 'claude-opus-5', 'claude-fable-5-1', 'claude-haiku-4-5-20251001'],
    groq: ['llama-3.3-70b-versatile'],
    openrouter: [],
    openai: ['gpt-4', 'gpt-4-turbo', 'gpt-3.5-turbo'],
    'builtin-ai': [],
    'custom-openai': [],
  };

  // Toggle confidence indicator with localStorage persistence
  const toggleConfidenceIndicator = useCallback((checked: boolean) => {
    setShowConfidenceIndicator(checked);
    if (typeof window !== 'undefined') {
      localStorage.setItem('showConfidenceIndicator', checked.toString());
    }
    // Trigger a custom event to notify other components
    window.dispatchEvent(new CustomEvent('confidenceIndicatorChanged', { detail: checked }));
  }, []);

  const toggleIsAutoSummary = useCallback((checked: boolean) => {
    setisAutoSummary(checked);
    if (typeof window !== 'undefined') {
      localStorage.setItem('isAutoSummary', checked.toString());
    }
  }, [])

  // Toggle beta feature with localStorage persistence and analytics
  const toggleBetaFeature = useCallback((featureKey: BetaFeatureKey, enabled: boolean) => {
    setBetaFeatures(prev => {
      const updated = { ...prev, [featureKey]: enabled };
      saveBetaFeatures(updated);

      // Track analytics with specific feature
      Analytics.track('beta_feature_toggled', {
        feature: featureKey,
        enabled: enabled.toString(),
      }).catch(err => console.error('Failed to track beta feature toggle:', err));

      return updated;
    });
  }, []);

  // Update individual provider API key
  const updateProviderApiKey = useCallback((provider: string, apiKey: string | null) => {
    if (!isProviderWithApiKey(provider)) return;
    keyRevisionRef.current[provider] += 1;
    setProviderApiKeys(prev => ({ ...prev, [provider]: apiKey }));
  }, []);

  // Lazy load preference settings (only loads if not already cached)
  const loadPreferences = useCallback(async () => {
    // If already loaded, don't reload
    if (preferencesLoadedRef.current) {
      return;
    }

    // If currently loading, don't start another load
    if (isLoadingRef.current) {
      return;
    }

    isLoadingRef.current = true;
    setIsLoadingPreferences(true);
    try {
      // Load notification settings from backend
      let settings: NotificationSettings | null = null;
      try {
        settings = await invoke<NotificationSettings>('get_notification_settings');
        setNotificationSettings(settings);
      } catch (notifError) {
        console.error('[ConfigContext] Failed to load notification settings:', notifError);
        // Use default values if notification settings fail to load
        setNotificationSettings(null);
      }

      // Load storage locations
      const [dbDir, modelsDir, recordingsDir] = await Promise.all([
        invoke<string>('get_database_directory'),
        invoke<string>('whisper_get_models_directory'),
        invoke<string>('get_default_recordings_folder_path')
      ]);

      setStorageLocations({
        database: dbDir,
        models: modelsDir,
        recordings: recordingsDir
      });

      // Mark as loaded
      preferencesLoadedRef.current = true;
    } catch (error) {
      console.error('[ConfigContext] Failed to load preferences:', error);
    } finally {
      isLoadingRef.current = false;
      setIsLoadingPreferences(false);
    }
  }, []);

  // Update notification settings
  const updateNotificationSettings = useCallback(async (settings: NotificationSettings) => {
    try {
      await invoke('set_notification_settings', { settings });
      setNotificationSettings(settings);
    } catch (error) {
      console.error('[ConfigContext] Failed to update notification settings:', error);
      throw error; // Re-throw so component can handle error
    }
  }, []);

  // Wrapper for setSelectedLanguage that persists to localStorage and syncs to Rust
  const handleSetSelectedLanguage = useCallback((lang: string) => {
    setSelectedLanguage(lang);
    if (typeof window !== 'undefined') {
      localStorage.setItem('primaryLanguage', lang);
    }
    // Sync with Rust in-memory state for live recording
    invoke('set_language_preference', { language: lang }).catch(err =>
      console.error('Failed to sync language preference to Rust:', err)
    );
  }, []);

  const value: ConfigContextType = useMemo(() => ({
    modelConfig,
    setModelConfig,
    commitModelConfig,
    isModelConfigLoading,
    isModelConfigSaving,
    modelConfigSaveError,
    isAutoSummary,
    toggleIsAutoSummary,
    providerApiKeys,
    updateProviderApiKey,
    transcriptModelConfig,
    setTranscriptModelConfig,
    selectedDevices,
    setSelectedDevices,
    selectedLanguage,
    setSelectedLanguage: handleSetSelectedLanguage,
    showConfidenceIndicator,
    toggleConfidenceIndicator,
    betaFeatures,
    toggleBetaFeature,
    models,
    modelOptions,
    error,
    notificationSettings,
    storageLocations,
    isLoadingPreferences,
    loadPreferences,
    updateNotificationSettings,
  }), [
    modelConfig,
    setModelConfig,
    commitModelConfig,
    isModelConfigLoading,
    isModelConfigSaving,
    modelConfigSaveError,
    isAutoSummary,
    toggleIsAutoSummary,
    providerApiKeys,
    updateProviderApiKey,
    transcriptModelConfig,
    selectedDevices,
    selectedLanguage,
    handleSetSelectedLanguage,
    showConfidenceIndicator,
    toggleConfidenceIndicator,
    betaFeatures,
    toggleBetaFeature,
    models,
    modelOptions,
    error,
    notificationSettings,
    storageLocations,
    isLoadingPreferences,
    loadPreferences,
    updateNotificationSettings,
  ]);

  return (
    <ConfigContext.Provider value={value}>
      {children}
    </ConfigContext.Provider>
  );
}

export function useConfig() {
  const context = useContext(ConfigContext);
  if (context === undefined) {
    throw new Error('useConfig must be used within a ConfigProvider');
  }
  return context;
}
