'use client';

import React, { createContext, useContext, useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { PermissionStatus, OnboardingPermissions, OnboardingStep, SetupCheckRecord } from '@/types/onboarding';
import { resolveOnboardingSummaryModelStatus } from '@/lib/onboarding-summary-model';
import { ParakeetAPI } from '@/lib/parakeet';
import type { CancelDownloadOutcome, ParakeetDownloadProgressEvent } from '@/lib/parakeet';
import { BUILTIN_AI_DOWNLOAD_PROGRESS_EVENT, BuiltInAIAPI } from '@/lib/builtin-ai';
import type { BuiltInAIDownloadProgressEvent } from '@/lib/builtin-ai';
import { mibToBytes } from '@/lib/download-display';
import { DEFAULT_PARAKEET_MODEL } from '@/constants/modelDefaults';

// Step 1: Welcome, 2: Setup Overview, 3: Download Progress, 4: Permissions (macOS only),
// 5: Mic Check. Non-macOS jumps from 3 to 5. The mic check is the last step and the only
// one that calls completeOnboarding().
const LAST_STEP: OnboardingStep = 5;

interface OnboardingStatus {
  version: string;
  completed: boolean;
  current_step: number;
  model_status: {
    parakeet: string;
    summary: string;
    selected_summary_model?: string;
  };
  last_updated: string;
  setup_check?: SetupCheckRecord;
}

export interface SummaryModelProgressInfo {
  percent: number;
  downloadedMb: number;
  totalMb: number;
  downloadedBytes: number;
  totalBytes: number;
  speedMbps: number;
  /** Whole seconds left, or null while the backend rate is too unstable to name one. */
  etaSeconds: number | null;
}

export interface ParakeetProgressInfo {
  percent: number;
  downloadedMb: number;
  totalMb: number;
  downloadedBytes: number;
  totalBytes: number;
  speedMbps: number;
  /** Whole seconds left, or null while the backend rate is too unstable to name one. */
  etaSeconds: number | null;
}

const IDLE_PROGRESS: ParakeetProgressInfo = {
  percent: 0,
  downloadedMb: 0,
  totalMb: 0,
  downloadedBytes: 0,
  totalBytes: 0,
  speedMbps: 0,
  etaSeconds: null,
};

/**
 * The byte counts are what the UI prints; the MB fields are the same number
 * divided by 1024² upstream, so they are a lossy last resort for a payload that
 * predates the byte fields rather than a reason to show zero.
 */
function progressBytes(bytes: number | undefined, mb: number | undefined): number {
  return bytes ?? Math.round(mibToBytes(mb ?? 0));
}

/** Which transfer a slot belongs to, so cancelling one does not claim the other stopped. */
type DownloadKind = 'parakeet' | 'summary';

interface OnboardingContextType {
  currentStep: number;
  parakeetDownloaded: boolean;
  parakeetProgress: number;
  parakeetProgressInfo: ParakeetProgressInfo;
  summaryModelDownloaded: boolean;
  summaryModelProgress: number;
  summaryModelProgressInfo: SummaryModelProgressInfo;
  selectedSummaryModel: string;
  recommendedSummaryModel: string;
  // Exact catalogue sizes, known before a byte moves. null means "not known yet" — a screen
  // showing a guessed size before the transfer starts is worse than one showing none.
  parakeetSizeBytes: number | null;
  summaryModelSizeBytes: number | null;
  databaseExists: boolean;
  isBackgroundDownloading: boolean;
  // Permissions
  permissions: OnboardingPermissions;
  permissionsSkipped: boolean;
  // The last recorded setup check outcome, restored from disk so a user who re-enters the flow
  // is not told nothing is known when something is.
  setupCheck: SetupCheckRecord | null;
  // Navigation
  goToStep: (step: number) => void;
  goNext: () => void;
  goPrevious: () => void;
  // Setters
  setParakeetDownloaded: (value: boolean) => void;
  setSummaryModelDownloaded: (value: boolean) => void;
  setSelectedSummaryModel: (value: string) => void;
  setDatabaseExists: (value: boolean) => void;
  setPermissionStatus: (permission: keyof OnboardingPermissions, status: PermissionStatus) => void;
  setPermissionsSkipped: (skipped: boolean) => void;
  completeOnboarding: (setupCheck?: SetupCheckRecord | null) => Promise<void>;
  startBackgroundDownloads: (options: StartBackgroundDownloadsOptions) => Promise<void>;
  retryParakeetDownload: () => Promise<void>;
  // Same retry contract as retryParakeetDownload, for the summary model.
  retrySummaryDownload: () => Promise<void>;
  // The Parakeet worker may not have noticed the request yet, so the outcome reaches the
  // caller instead of being swallowed: 'pending' needs a different message from 'cancelled'.
  cancelParakeetDownload: () => Promise<CancelDownloadOutcome>;
  cancelSummaryDownload: () => Promise<void>;
}

interface StartBackgroundDownloadsOptions {
  includeParakeet: boolean;
  includeSummary: boolean;
  summaryModel?: string;
}

const OnboardingContext = createContext<OnboardingContextType | undefined>(undefined);

export function OnboardingProvider({ children }: { children: React.ReactNode }) {
  const [currentStep, setCurrentStep] = useState(1);
  const [completed, setCompleted] = useState(false);
  const [parakeetDownloaded, setParakeetDownloaded] = useState(false);
  const [parakeetProgress, setParakeetProgress] = useState(0);
  const [parakeetProgressInfo, setParakeetProgressInfo] = useState<ParakeetProgressInfo>(IDLE_PROGRESS);
  const [summaryModelDownloaded, setSummaryModelDownloaded] = useState(false);
  const [summaryModelProgress, setSummaryModelProgress] = useState(0);
  const [summaryModelProgressInfo, setSummaryModelProgressInfo] =
    useState<SummaryModelProgressInfo>(IDLE_PROGRESS);
  const [selectedSummaryModel, setSelectedSummaryModel] = useState<string>('');
  const [recommendedSummaryModel, setRecommendedSummaryModel] = useState<string>('');
  const [parakeetSizeBytes, setParakeetSizeBytes] = useState<number | null>(null);
  const [summaryModelSizeBytes, setSummaryModelSizeBytes] = useState<number | null>(null);
  const [databaseExists, setDatabaseExists] = useState(false);
  const [isBackgroundDownloading, setIsBackgroundDownloading] = useState(false);
  // Transfers believed to be in flight. Cancelling or finishing one only lowers the
  // downloading flag once the set is empty, so one model's cancel cannot hide the other's
  // progress.
  const activeDownloadsRef = useRef<Set<DownloadKind>>(new Set());

  const releaseDownloadSlot = useCallback((kind: DownloadKind) => {
    activeDownloadsRef.current.delete(kind);
    if (activeDownloadsRef.current.size === 0) {
      setIsBackgroundDownloading(false);
    }
  }, []);

  // Permissions state
  const [permissions, setPermissions] = useState<OnboardingPermissions>({
    microphone: 'not_determined',
    systemAudio: 'not_determined',
    screenRecording: 'not_determined',
  });
  const [permissionsSkipped, setPermissionsSkipped] = useState(false);

  // The last recorded setup check outcome, restored from the saved status on load.
  const [setupCheck, setSetupCheck] = useState<SetupCheckRecord | null>(null);

  const saveTimeoutRef = useRef<NodeJS.Timeout>();

  const initializeSummaryModelSelection = async (preferredModel = selectedSummaryModel) => {
    try {
      const recommendedModel = await invoke<string>('builtin_ai_get_recommended_model');
      setRecommendedSummaryModel(recommendedModel);
      const modelToCheck = preferredModel || recommendedModel;
      setSelectedSummaryModel(modelToCheck);

      const selectedModelReady = await invoke<boolean>('builtin_ai_is_model_ready', {
        modelName: modelToCheck,
        refresh: true,
      });
      const resolved = resolveOnboardingSummaryModelStatus({
        selectedModel: preferredModel,
        recommendedModel,
        selectedModelReady,
      });

      setSelectedSummaryModel(resolved.selectedSummaryModel);
      setSummaryModelDownloaded(resolved.summaryModelDownloaded);
      console.log('[OnboardingContext] Set recommended model:', resolved.selectedSummaryModel);

      return resolved;
    } catch (error) {
      console.error('[OnboardingContext] Failed to initialize summary model:', error);
      return null;
    }
  };

  const requestSummaryModelDownload = (modelName: string) => {
    console.log('[OnboardingContext] Starting Summary Model download');
    invoke('builtin_ai_download_model', { modelName })
      .catch(err => {
        console.error('[OnboardingContext] Summary Model download failed:', err);
      });
  };

  // Load status on mount and initialize database
  useEffect(() => {
    loadOnboardingStatus();
    checkDatabaseStatus();
    initializeDatabaseInBackground();
  }, []);

  // Initialize database silently in background (moved from SetupOverviewStep)
  const initializeDatabaseInBackground = async () => {
    try {
      console.log('[OnboardingContext] Starting background database initialization');
      const isFirstLaunch = await invoke<boolean>('check_first_launch');

      if (!isFirstLaunch) {
        console.log('[OnboardingContext] Database exists, skipping initialization');
        setDatabaseExists(true);
        return;
      }

      // First launch - attempt auto-detection and import
      await performAutoDetection();
    } catch (error) {
      console.error('[OnboardingContext] Database initialization failed:', error);
      // Don't throw - database init failure shouldn't block onboarding
    }
  };

  const performAutoDetection = async () => {
    // Check Homebrew (macOS only)
    if (typeof navigator !== 'undefined' && navigator.platform?.toLowerCase().includes('mac')) {
      const homebrewDbPath = '/usr/local/var/meetily/meeting_minutes.db';
      try {
        const homebrewCheck = await invoke<{ exists: boolean; size: number } | null>(
          'check_homebrew_database',
          { path: homebrewDbPath }
        );

        if (homebrewCheck?.exists) {
          console.log('[OnboardingContext] Found Homebrew database, importing');
          await invoke('import_and_initialize_database', { legacyDbPath: homebrewDbPath });
          setDatabaseExists(true);
          return;
        }
      } catch (e) {
        console.log('[OnboardingContext] Homebrew check failed, continuing:', e);
      }
    }

    // Check default legacy database location
    try {
      const legacyPath = await invoke<string | null>('check_default_legacy_database');
      if (legacyPath) {
        console.log('[OnboardingContext] Found legacy database, importing');
        await invoke('import_and_initialize_database', { legacyDbPath: legacyPath });
        setDatabaseExists(true);
        return;
      }
    } catch (e) {
      console.log('[OnboardingContext] Legacy check failed, continuing:', e);
    }

    // No legacy database found - initialize fresh
    console.log('[OnboardingContext] No legacy database found, initializing fresh');
    await invoke('initialize_fresh_database');
    setDatabaseExists(true);
  };

  const isCompletingRef = useRef(false);

  // Auto-save on state change (debounced)
  useEffect(() => {
    if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);

    // Don't auto-save if completed (to avoid overwriting completion status)
    // Also don't auto-save if we are currently in the process of completing
    if (completed || isCompletingRef.current) return;

    saveTimeoutRef.current = setTimeout(() => {
      saveOnboardingStatus();
    }, 1000);

    return () => {
      if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);
    };
  }, [currentStep, parakeetDownloaded, summaryModelDownloaded, completed]);

  // Listen to Parakeet download progress
  useEffect(() => {
    const unlisten = listen<ParakeetDownloadProgressEvent>(
      'parakeet-model-download-progress',
      (event) => {
        const {
          modelName,
          progress,
          downloaded_bytes,
          total_bytes,
          downloaded_mb,
          total_mb,
          speed_mbps,
          eta_seconds,
          status,
        } = event.payload;
        if (modelName !== DEFAULT_PARAKEET_MODEL) return;

        if (status === 'cancelled') {
          releaseDownloadSlot('parakeet');
          setParakeetDownloaded(false);
          setParakeetProgress(0);
          setParakeetProgressInfo(IDLE_PROGRESS);
          return;
        }

        setParakeetProgress(progress);
        setParakeetProgressInfo({
          percent: progress,
          downloadedMb: downloaded_mb ?? 0,
          totalMb: total_mb ?? 0,
          downloadedBytes: progressBytes(downloaded_bytes, downloaded_mb),
          totalBytes: progressBytes(total_bytes, total_mb),
          speedMbps: speed_mbps ?? 0,
          // Absent means indeterminate, never "no time left".
          etaSeconds: eta_seconds ?? null,
        });
        if (status === 'completed') {
          setParakeetDownloaded(true);
          releaseDownloadSlot('parakeet');
        }
      }
    );

    const unlistenComplete = listen<{ modelName: string }>(
      'parakeet-model-download-complete',
      (event) => {
        const { modelName } = event.payload;
        if (modelName === DEFAULT_PARAKEET_MODEL) {
          setParakeetDownloaded(true);
          setParakeetProgress(100);
          releaseDownloadSlot('parakeet');
        }
      }
    );

    const unlistenError = listen<{ modelName: string; error: string }>(
      'parakeet-model-download-error',
      (event) => {
        const { modelName } = event.payload;
        if (modelName === DEFAULT_PARAKEET_MODEL) {
          console.error('Parakeet download error:', event.payload.error);
          releaseDownloadSlot('parakeet');
        }
      }
    );

    return () => {
      unlisten.then(fn => fn());
      unlistenComplete.then(fn => fn());
      unlistenError.then(fn => fn());
    };
  }, [releaseDownloadSlot]);

  // Listen to summary model (Built-in AI) download progress
  useEffect(() => {
    const unlisten = listen<BuiltInAIDownloadProgressEvent>(
      BUILTIN_AI_DOWNLOAD_PROGRESS_EVENT,
      (event) => {
        const {
          model,
          progress,
          downloaded_bytes,
          total_bytes,
          downloaded_mb,
          total_mb,
          speed_mbps,
          eta_seconds,
          status,
        } = event.payload;
        if (selectedSummaryModel && model === selectedSummaryModel) {
          if (status === 'cancelled') {
            releaseDownloadSlot('summary');
            setSummaryModelDownloaded(false);
            setSummaryModelProgress(0);
            setSummaryModelProgressInfo(IDLE_PROGRESS);
            return;
          }

          setSummaryModelProgress(progress);
          setSummaryModelProgressInfo({
            percent: progress,
            downloadedMb: downloaded_mb ?? 0,
            totalMb: total_mb ?? 0,
            downloadedBytes: progressBytes(downloaded_bytes, downloaded_mb),
            totalBytes: progressBytes(total_bytes, total_mb),
            speedMbps: speed_mbps ?? 0,
            // Absent means indeterminate, never "no time left".
            etaSeconds: eta_seconds ?? null,
          });
          if (status === 'completed' || progress >= 100) {
            setSummaryModelDownloaded(true);
            releaseDownloadSlot('summary');
          } else if (status === 'error') {
            console.error('[OnboardingContext] Summary Model download error:', event.payload.error);
            releaseDownloadSlot('summary');
          }
        }
      }
    );

    return () => {
      unlisten.then(fn => fn());
    };
  }, [selectedSummaryModel, releaseDownloadSlot]);

  // Catalogue sizes are read up front — before anything is downloaded — so the setup screen
  // can say what the install costs while the user can still decide. A failure leaves the
  // value null and the screen says nothing rather than something wrong.
  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        await ParakeetAPI.init();
        const models = await ParakeetAPI.getAvailableModels();
        if (cancelled) return;

        const model = models.find(m => m.name === DEFAULT_PARAKEET_MODEL);
        if (model && model.size_bytes > 0) {
          setParakeetSizeBytes(model.size_bytes);
        } else {
          console.warn('[OnboardingContext] No catalogue size for Parakeet model:', DEFAULT_PARAKEET_MODEL);
        }
      } catch (error) {
        console.warn('[OnboardingContext] Failed to load Parakeet catalogue size:', error);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  // The summary model is not known on mount - it arrives from the recommendation - so this
  // follows the selection rather than running once.
  useEffect(() => {
    if (!selectedSummaryModel) {
      setSummaryModelSizeBytes(null);
      return;
    }

    let cancelled = false;

    (async () => {
      try {
        const info = await BuiltInAIAPI.getModelInfo(selectedSummaryModel);
        if (cancelled) return;

        if (info && info.size_bytes > 0) {
          setSummaryModelSizeBytes(info.size_bytes);
        } else {
          setSummaryModelSizeBytes(null);
          console.warn('[OnboardingContext] No catalogue size for summary model:', selectedSummaryModel);
        }
      } catch (error) {
        if (cancelled) return;
        setSummaryModelSizeBytes(null);
        console.warn('[OnboardingContext] Failed to load summary model catalogue size:', error);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [selectedSummaryModel]);

  const checkDatabaseStatus = async () => {
    try {
      const isFirstLaunch = await invoke<boolean>('check_first_launch');
      setDatabaseExists(!isFirstLaunch);
      console.log('[OnboardingContext] Database exists:', !isFirstLaunch);
    } catch (error) {
      console.error('[OnboardingContext] Failed to check database status:', error);
      setDatabaseExists(false);
    }
  };

  const loadOnboardingStatus = async () => {
    try {
      const status = await invoke<OnboardingStatus | null>('get_onboarding_status');
      if (status) {
        console.log('[OnboardingContext] Loaded saved status:', status);

        if (status.completed) {
          setCurrentStep(status.current_step);
          setCompleted(true);
          setParakeetDownloaded(status.model_status.parakeet === 'downloaded');
          setSummaryModelDownloaded(status.model_status.summary === 'downloaded');
          if (status.model_status.selected_summary_model) {
            setSelectedSummaryModel(status.model_status.selected_summary_model);
          }
          setSetupCheck(status.setup_check ?? null);
          console.log('[OnboardingContext] Restored completed onboarding status without model verification');
          return;
        }

        // Don't trust saved status - verify actual model status on disk
        const verifiedStatus = await verifyModelStatus(status);

        setCurrentStep(verifiedStatus.currentStep);
        setCompleted(verifiedStatus.completed);
        setParakeetDownloaded(verifiedStatus.parakeetDownloaded);
        setSummaryModelDownloaded(verifiedStatus.summaryModelDownloaded);
        if (verifiedStatus.selectedSummaryModel) {
          setSelectedSummaryModel(verifiedStatus.selectedSummaryModel);
        }
        setSetupCheck(status.setup_check ?? null);

        console.log('[OnboardingContext] Verified status:', verifiedStatus);

        // Check if any downloads are active to restore isBackgroundDownloading state
        await checkActiveDownloads();
      } else {
        await initializeSummaryModelSelection();
      }
    } catch (error) {
      console.error('[OnboardingContext] Failed to load onboarding status:', error);
    }
  };

  // Verify that models actually exist on disk, not just trust saved JSON
  const verifyModelStatus = async (savedStatus: OnboardingStatus) => {
    let parakeetDownloaded = false;
    let summaryModelDownloaded = false;
    let selectedSummaryModel = '';

    // Verify Parakeet model exists on disk
    try {
      await invoke('parakeet_init');
      parakeetDownloaded = await invoke<boolean>('parakeet_has_available_models');
      console.log('[OnboardingContext] Parakeet verified on disk:', parakeetDownloaded);
    } catch (error) {
      console.warn('[OnboardingContext] Failed to verify Parakeet:', error);
      parakeetDownloaded = false;
    }

    // Verify the selected/recommended Summary model exists on disk.
    try {
      const recommendedModel = await invoke<string>('builtin_ai_get_recommended_model');
      setRecommendedSummaryModel(recommendedModel);
      const savedSelectedModel = savedStatus.model_status.selected_summary_model || '';
      const modelToCheck = savedSelectedModel || recommendedModel;
      const selectedModelReady = await invoke<boolean>('builtin_ai_is_model_ready', {
        modelName: modelToCheck,
        refresh: true,
      });
      const resolved = resolveOnboardingSummaryModelStatus({
        selectedModel: savedSelectedModel,
        recommendedModel,
        selectedModelReady,
      });
      selectedSummaryModel = resolved.selectedSummaryModel;
      summaryModelDownloaded = resolved.summaryModelDownloaded;
      console.log('[OnboardingContext] Summary model verified on disk:', summaryModelDownloaded, 'model:', selectedSummaryModel);
    } catch (error) {
      console.warn('[OnboardingContext] Failed to verify Summary model:', error);
      summaryModelDownloaded = false;
    }

    // Resume on the step the user left off on, so someone who quit during the mic check comes
    // back to the mic check instead of watching a finished download again.
    let currentStep = savedStatus.current_step;
    const completed = savedStatus.completed;

    // A step saved by a different version of the flow must not strand the user past the last screen
    if (currentStep > LAST_STEP) {
      currentStep = LAST_STEP;
    }

    // Trust the completed status - don't revert based on model downloads
    // Downloads continue in background; user stays in main app regardless
    return {
      currentStep,
      completed,
      parakeetDownloaded,
      summaryModelDownloaded,
      selectedSummaryModel,
    };
  };

  const saveOnboardingStatus = async () => {
    // Safety check: if we are in the process of completing, DO NOT save
    // This prevents a race condition where a download completion event triggers a save
    // that overwrites the "completed" status set by completeOnboarding
    if (isCompletingRef.current) {
      console.log('[OnboardingContext] Skipping saveOnboardingStatus because completion is in progress');
      return;
    }

    try {
      await invoke('save_onboarding_status_cmd', {
        status: {
          version: '1.0',
          completed: completed,
          current_step: currentStep,
          model_status: {
            parakeet: parakeetDownloaded ? 'downloaded' : 'not_downloaded',
            summary: summaryModelDownloaded ? 'downloaded' : 'not_downloaded',
            selected_summary_model: selectedSummaryModel || undefined,
          },
          last_updated: new Date().toISOString(),
          // The Rust side also preserves a stored record when the incoming one is None - belt and
          // braces, since the settings surface writes this field too, from outside this context.
          setup_check: setupCheck ?? undefined,
        },
      });
    } catch (error) {
      console.error('[OnboardingContext] Failed to save onboarding status:', error);
    }
  };

  const completeOnboarding = async (setupCheck?: SetupCheckRecord | null) => {
    try {
      // Set completion flag to prevent race conditions with auto-save
      isCompletingRef.current = true;

      // Clear any pending auto-saves
      if (saveTimeoutRef.current) {
        clearTimeout(saveTimeoutRef.current);
        saveTimeoutRef.current = undefined;
      }

      let modelToSave = selectedSummaryModel;
      if (!modelToSave) {
        modelToSave = await invoke<string>('builtin_ai_get_recommended_model');
        setSelectedSummaryModel(modelToSave);
      }

      const selectedModelReady = await invoke<boolean>('builtin_ai_is_model_ready', {
        modelName: modelToSave,
        refresh: true,
      });
      setSummaryModelDownloaded(selectedModelReady);
      if (!selectedModelReady) {
        requestSummaryModelDownload(modelToSave);
      }

      // Onboarding always uses builtin-ai with selected model
      await invoke('complete_onboarding', {
        model: modelToSave,
        setupCheck,
      });
      setCompleted(true);
      console.log('[OnboardingContext] Onboarding completed with model:', modelToSave);

      // Reset the flag so subsequent state updates can be saved
      isCompletingRef.current = false;
    } catch (error) {
      console.error('[OnboardingContext] Failed to complete onboarding:', error);
      isCompletingRef.current = false; // Reset flag on error
      throw error; // Re-throw so the mic check step can surface the failure
    }
  };

  // Start background downloads for models.
  const startBackgroundDownloads = async ({
    includeParakeet,
    includeSummary,
    summaryModel,
  }: StartBackgroundDownloadsOptions) => {
    console.log('[OnboardingContext] Starting background downloads:', {
      includeParakeet,
      includeSummary,
      summaryModel,
    });

    try {
      const shouldStartParakeet = includeParakeet && !parakeetDownloaded;
      const shouldStartSummary = includeSummary && !summaryModelDownloaded && !!summaryModel;

      if (!shouldStartParakeet && !shouldStartSummary) {
        if (includeSummary && !summaryModelDownloaded && !summaryModel) {
          console.warn('[OnboardingContext] Summary Model download skipped until recommendation is loaded');
        }
        return;
      }

      setIsBackgroundDownloading(true);

      // Start Parakeet download first (speech recognition - always required)
      if (shouldStartParakeet) {
        console.log('[OnboardingContext] Starting Parakeet download');
        activeDownloadsRef.current.add('parakeet');
        invoke('parakeet_download_model', { modelName: DEFAULT_PARAKEET_MODEL })
          .catch(err => console.error('[OnboardingContext] Parakeet download failed:', err));
      }

      // Start selected Summary Model download immediately so completion cannot race the request.
      if (shouldStartSummary && summaryModel) {
        activeDownloadsRef.current.add('summary');
        requestSummaryModelDownload(summaryModel);
      }
    } catch (error) {
      console.error('[OnboardingContext] Failed to start background downloads:', error);
      setIsBackgroundDownloading(false);
      throw error;
    }
  };

  // Check if any models are currently downloading (for re-entry)
  const checkActiveDownloads = async () => {
    try {
      const models = await invoke<any[]>('parakeet_get_available_models');
      const isDownloading = models.some(m => m.status && (typeof m.status === 'object' ? 'Downloading' in m.status : m.status === 'Downloading'));
      
      if (isDownloading) {
        console.log('[OnboardingContext] Detected active background downloads on mount');
        activeDownloadsRef.current.add('parakeet');
        setIsBackgroundDownloading(true);
      }
      
      // Also check for Built-in AI downloads if possible (though less critical as Parakeet is the main blocker)
      
    } catch (error) {
      console.warn('[OnboardingContext] Failed to check active downloads:', error);
    }
  };

  const retryParakeetDownload = async () => {
    console.log('[OnboardingContext] Retrying Parakeet download');
    activeDownloadsRef.current.add('parakeet');
    setIsBackgroundDownloading(true);
    try {
      await invoke('parakeet_retry_download', { modelName: DEFAULT_PARAKEET_MODEL });
    } catch (error) {
      console.error('[OnboardingContext] Retry failed:', error);
      releaseDownloadSlot('parakeet');
      throw error;
    }
  };

  const retrySummaryDownload = async () => {
    if (!selectedSummaryModel) {
      throw new Error('Summary model recommendation is not ready yet');
    }

    console.log('[OnboardingContext] Retrying Summary Model download');
    activeDownloadsRef.current.add('summary');
    setIsBackgroundDownloading(true);
    try {
      await invoke('builtin_ai_download_model', { modelName: selectedSummaryModel });
    } catch (error) {
      console.error('[OnboardingContext] Summary Model retry failed:', error);
      releaseDownloadSlot('summary');
      throw error;
    }
  };

  // Cancellation. The Parakeet backend answers 'pending' when the worker has not yet reached a
  // point where it can stop, so the outcome goes back to the caller unchanged: only 'cancelled'
  // means the transfer is really over and the progress readout can be cleared here. A 'pending'
  // cancel is finished by the cancelled progress event when the worker gets there.
  const cancelParakeetDownload = async (): Promise<CancelDownloadOutcome> => {
    console.log('[OnboardingContext] Cancelling Parakeet download');
    const outcome = await ParakeetAPI.cancelDownload(DEFAULT_PARAKEET_MODEL);

    setParakeetDownloaded(false);
    if (outcome === 'cancelled') {
      setParakeetProgress(0);
      setParakeetProgressInfo(IDLE_PROGRESS);
      releaseDownloadSlot('parakeet');
    }

    return outcome;
  };

  const cancelSummaryDownload = async (): Promise<void> => {
    if (!selectedSummaryModel) {
      console.warn('[OnboardingContext] Summary Model cancel ignored: no model selected');
      return;
    }

    console.log('[OnboardingContext] Cancelling Summary Model download:', selectedSummaryModel);
    await BuiltInAIAPI.cancelDownload(selectedSummaryModel);

    setSummaryModelDownloaded(false);
    setSummaryModelProgress(0);
    setSummaryModelProgressInfo(IDLE_PROGRESS);
    releaseDownloadSlot('summary');
  };

  const setPermissionStatus = useCallback((permission: keyof OnboardingPermissions, status: PermissionStatus) => {
    setPermissions((prev: OnboardingPermissions) => ({
      ...prev,
      [permission]: status,
    }));
  }, []);

  const goToStep = useCallback((step: number) => {
    setCurrentStep(Math.max(1, Math.min(step, LAST_STEP)));
  }, []);

  const goNext = useCallback(() => {
    setCurrentStep((prev: number) => {
      const next = prev + 1;
      // Don't go past the mic check
      return Math.min(next, LAST_STEP);
    });
  }, []);

  const goPrevious = useCallback(() => {
    setCurrentStep((prev: number) => {
      const previous = prev - 1;
      // Don't go below step 1
      return Math.max(previous, 1);
    });
  }, []);

  return (
    <OnboardingContext.Provider
      value={{
        currentStep,
        parakeetDownloaded,
        parakeetProgress,
        parakeetProgressInfo,
        summaryModelDownloaded,
        summaryModelProgress,
        summaryModelProgressInfo,
        selectedSummaryModel,
        recommendedSummaryModel,
        parakeetSizeBytes,
        summaryModelSizeBytes,
        databaseExists,
        isBackgroundDownloading,
        permissions,
        permissionsSkipped,
        setupCheck,
        goToStep,
        goNext,
        goPrevious,
        setParakeetDownloaded,
        setSummaryModelDownloaded,
        setSelectedSummaryModel,
        setDatabaseExists,
        setPermissionStatus,
        setPermissionsSkipped,
        completeOnboarding,
        startBackgroundDownloads,
        retryParakeetDownload,
        retrySummaryDownload,
        cancelParakeetDownload,
        cancelSummaryDownload,
      }}
    >
      {children}
    </OnboardingContext.Provider>
  );
}

export function useOnboarding() {
  const context = useContext(OnboardingContext);
  if (!context) {
    throw new Error('useOnboarding must be used within OnboardingProvider');
  }
  return context;
}
