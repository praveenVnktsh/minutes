import React, { useEffect, useState, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Mic, Sparkles, Check, Loader2, Download, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';
import { toast } from 'sonner';
import { motion, AnimatePresence } from 'framer-motion';
import { formatBytes, formatBytesPerSecond, formatEta, mibToBytes } from '@/lib/download-display';
import type { ParakeetDownloadProgressEvent } from '@/lib/parakeet';
import {
  BUILTIN_AI_DOWNLOAD_PROGRESS_EVENT,
  type BuiltInAIDownloadProgressEvent,
} from '@/lib/builtin-ai';
import { DEFAULT_PARAKEET_MODEL } from '@/constants/modelDefaults';
import { cn } from '@/lib/utils';
import { badge, panel, progress as progressClasses, toneText } from '@/lib/theme-classes';

/**
 * 'idle' is "nothing is running and nothing is done" — the state a user lands in after
 * cancelling and coming back, which needs a way out. 'stopping' is the gap between the
 * backend accepting a cancel and the worker reaching a point where it can stop; starting
 * again during that gap is rejected by the download owner reservation, so the gap is a
 * state of its own rather than an early "cancelled".
 */
type DownloadStatus = 'idle' | 'downloading' | 'stopping' | 'completed' | 'cancelled' | 'error';

/** Said out loud before the cancel happens, not after it. */
const PARAKEET_CANCEL_CONSEQUENCE =
  'Recording will not work until the transcription engine is downloaded.';
const SUMMARY_CANCEL_CONSEQUENCE =
  'Summaries will fall back to an external provider you configure in settings.';

interface DownloadCardProps {
  title: string;
  icon: React.ReactNode;
  status: DownloadStatus;
  error?: string;
  percent: number;
  downloadedBytes: number;
  totalBytes: number;
  /** The backend's `speed_mbps` is bytes/1024²/s despite the name, so it is MiB/s. */
  speedMbps: number;
  etaSeconds: number | null;
  /** Exact catalogue size, or null while it is unknown — then the screen says nothing. */
  catalogueBytes: number | null;
  consequence: string;
  startLabel: string;
  confirmingCancel: boolean;
  onRequestCancel: () => void;
  onDismissCancel: () => void;
  onConfirmCancel: () => void;
  onStart: () => void;
}

export function DownloadProgressStep() {
  const {
    goNext,
    goToStep,
    selectedSummaryModel,
    parakeetDownloaded,
    setParakeetDownloaded,
    parakeetProgressInfo,
    parakeetSizeBytes,
    summaryModelDownloaded,
    summaryModelProgressInfo,
    summaryModelSizeBytes,
    isBackgroundDownloading,
    startBackgroundDownloads,
    retryParakeetDownload,
    retrySummaryDownload,
    cancelParakeetDownload,
    cancelSummaryDownload,
  } = useOnboarding();

  const [isMac, setIsMac] = useState(false);

  // The transfers are authorised by the click on the previous screen, so on arrival a
  // download is either already running, already finished, or was never started at all —
  // and the last of those has to look different from the first.
  const [parakeetStatus, setParakeetStatus] = useState<DownloadStatus>(() =>
    parakeetDownloaded ? 'completed' : isBackgroundDownloading ? 'downloading' : 'idle'
  );
  const [parakeetError, setParakeetError] = useState<string | undefined>();

  const [summaryStatus, setSummaryStatus] = useState<DownloadStatus>(() =>
    summaryModelDownloaded
      ? 'completed'
      : isBackgroundDownloading && selectedSummaryModel
      ? 'downloading'
      : 'idle'
  );
  const [summaryError, setSummaryError] = useState<string | undefined>();

  const [confirmingCancel, setConfirmingCancel] = useState<'parakeet' | 'summary' | null>(null);

  const summaryDownloadStartedRef = useRef(false);
  const retryingRef = useRef(false);
  const retryingSummaryRef = useRef(false);

  // Retry download handler
  const handleRetryDownload = async () => {
    // Prevent multiple simultaneous retries
    if (retryingRef.current) {
      console.log('[DownloadProgressStep] Retry already in progress, ignoring');
      return;
    }

    console.log('[DownloadProgressStep] Retrying Parakeet download');
    retryingRef.current = true;
    setConfirmingCancel(null);
    setParakeetError(undefined);
    setParakeetStatus('downloading');

    try {
      // Through the context rather than a bare invoke, so the shared in-flight set knows
      // this transfer is running again.
      await retryParakeetDownload();
      // Progress events will update state
    } catch (error) {
      console.error('[DownloadProgressStep] Retry failed:', error);
      setParakeetStatus('error');
      setParakeetError(error instanceof Error ? error.message : 'Retry failed');

      toast.error('Download retry failed', {
        description: 'Please check your connection and try again.',
      });
    } finally {
      // Allow retry again after 2 seconds
      setTimeout(() => {
        retryingRef.current = false;
      }, 2000);
    }
  };

  // Retry summary download handler
  const handleRetrySummaryDownload = async () => {
    // Prevent multiple simultaneous retries
    if (retryingSummaryRef.current) {
      console.log('[DownloadProgressStep] Summary retry already in progress, ignoring');
      return;
    }

    console.log('[DownloadProgressStep] Retrying summary model download');
    retryingSummaryRef.current = true;
    setConfirmingCancel(null);
    setSummaryError(undefined);
    setSummaryStatus('downloading');

    try {
      // Routed through the context like the transcription engine retry above, for the
      // same reason.
      await retrySummaryDownload();
    } catch (error) {
      console.error('[DownloadProgressStep] Summary retry failed:', error);
      setSummaryStatus('error');
      setSummaryError(error instanceof Error ? error.message : 'Retry failed');

      toast.error('Summary model download retry failed', {
        description: 'Please check your connection and try again.',
      });
    } finally {
      // Allow retry again after 2 seconds
      setTimeout(() => {
        retryingSummaryRef.current = false;
      }, 2000);
    }
  };

  const handleCancelParakeet = async () => {
    setConfirmingCancel(null);
    setParakeetStatus('stopping');

    try {
      const outcome = await cancelParakeetDownload();
      // 'pending' means the worker has been asked to stop but has not got there yet, and a
      // restart is refused until it has — so the card keeps saying "stopping" until the
      // cancelled progress event arrives.
      if (outcome === 'cancelled') {
        setParakeetStatus('cancelled');
      }
    } catch (error) {
      console.error('[DownloadProgressStep] Failed to cancel Parakeet download:', error);
      setParakeetStatus('downloading');
      toast.error('Could not cancel the download', {
        description: 'The transcription engine is still downloading.',
      });
    }
  };

  const handleCancelSummary = async () => {
    setConfirmingCancel(null);
    setSummaryStatus('stopping');

    try {
      await cancelSummaryDownload();
      setSummaryStatus('cancelled');
    } catch (error) {
      console.error('[DownloadProgressStep] Failed to cancel summary download:', error);
      setSummaryStatus('downloading');
      toast.error('Could not cancel the download', {
        description: 'The summary engine is still downloading.',
      });
    }
  };

  // Detect platform on mount
  useEffect(() => {
    const checkPlatform = async () => {
      try {
        const { platform } = await import('@tauri-apps/plugin-os');
        setIsMac(platform() === 'macos');
      } catch (e) {
        setIsMac(navigator.userAgent.includes('Mac'));
      }
    };

    checkPlatform();
  }, []);

  // The summary model name resolves asynchronously and may still have been empty when the
  // previous screen fired the downloads, so this catches up once it arrives. It starts the
  // summary model and nothing else: the transcription engine is started by that click, or
  // by the card's own control here.
  useEffect(() => {
    if (summaryDownloadStartedRef.current) return;
    if (!selectedSummaryModel) return;
    if (summaryModelDownloaded) return;
    if (summaryStatus !== 'idle') return;
    summaryDownloadStartedRef.current = true;

    startSummaryDownload();
  }, [selectedSummaryModel, summaryModelDownloaded, summaryStatus]);

  // Listen to Parakeet download progress. The numbers live on the context; what this needs
  // from the events is which state the card is in.
  useEffect(() => {
    const unlistenProgress = listen<ParakeetDownloadProgressEvent>(
      'parakeet-model-download-progress',
      (event) => {
        const { modelName, status } = event.payload;
        if (modelName !== DEFAULT_PARAKEET_MODEL) return;

        if (status === 'cancelled') {
          setParakeetStatus('cancelled');
          return;
        }

        setParakeetStatus(status === 'completed' ? 'completed' : 'downloading');
        setParakeetError(undefined);
      }
    );

    const unlistenComplete = listen<{ modelName: string }>(
      'parakeet-model-download-complete',
      (event) => {
        if (event.payload.modelName === DEFAULT_PARAKEET_MODEL) {
          setParakeetStatus('completed');
        }
      }
    );

    const unlistenError = listen<{ modelName: string; error: string }>(
      'parakeet-model-download-error',
      (event) => {
        if (event.payload.modelName === DEFAULT_PARAKEET_MODEL) {
          setParakeetStatus('error');
          setParakeetError(event.payload.error);
        }
      }
    );

    return () => {
      unlistenProgress.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
      unlistenError.then((fn) => fn());
    };
  }, []);

  // Listen to Summary Model download progress
  useEffect(() => {
    const unlisten = listen<BuiltInAIDownloadProgressEvent>(
      BUILTIN_AI_DOWNLOAD_PROGRESS_EVENT,
      (event) => {
        const { model, progress, status, error } = event.payload;
        if (!selectedSummaryModel || model !== selectedSummaryModel) return;

        if (status === 'cancelled') {
          setSummaryStatus('cancelled');
          return;
        }

        if (status === 'error') {
          setSummaryStatus('error');
          setSummaryError(error);
          return;
        }

        setSummaryStatus(status === 'completed' || progress >= 100 ? 'completed' : 'downloading');
        setSummaryError(undefined);
      }
    );

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [selectedSummaryModel]);

  // The context can learn a model is already on disk without any event reaching this screen.
  useEffect(() => {
    if (parakeetDownloaded) setParakeetStatus('completed');
  }, [parakeetDownloaded]);

  useEffect(() => {
    if (summaryModelDownloaded) setSummaryStatus('completed');
  }, [summaryModelDownloaded]);

  const startSummaryDownload = async () => {
    if (summaryModelDownloaded || !selectedSummaryModel) return;

    setConfirmingCancel(null);
    setSummaryError(undefined);
    setSummaryStatus('downloading');

    try {
      await startBackgroundDownloads({
        includeParakeet: false,
        includeSummary: true,
        summaryModel: selectedSummaryModel,
      });
    } catch (error) {
      console.error('Failed to start summary model download:', error);
      setSummaryStatus('error');
      setSummaryError(String(error));
    }
  };

  const handleContinue = async () => {
    // Verify actual model availability (catches state drift)
    try {
      await invoke('parakeet_init');
      const actuallyAvailable = await invoke<boolean>('parakeet_has_available_models');

      if (actuallyAvailable && !parakeetDownloaded) {
        console.log('[DownloadProgressStep] Model available but state not updated');
        setParakeetDownloaded(true);
        setParakeetStatus('completed');
      } else if (
        !actuallyAvailable &&
        (parakeetStatus === 'error' || parakeetStatus === 'cancelled')
      ) {
        toast.error('Transcription engine required', {
          description: 'Please retry the download before continuing.',
        });
        return;
      }
    } catch (error) {
      console.warn('[DownloadProgressStep] Failed to verify model:', error);
    }

    // Check if downloads are complete for toast notification
    const downloadsComplete = parakeetStatus === 'completed' && summaryStatus === 'completed';

    // Show toast if downloads still in progress
    if (!downloadsComplete) {
      toast.info('Downloads will continue in the background', {
        description: 'You can start using the app. Recording will be available once speech recognition is ready.',
        duration: 5000,
      });
    }

    if (isMac) {
      // macOS: Go to Permissions step next
      goNext();
    } else {
      // Non-macOS: Permissions step doesn't render here, so jump straight to the mic check
      goToStep(5);
    }
  };

  const renderDownloadCard = ({
    title,
    icon,
    status,
    error,
    percent,
    downloadedBytes,
    totalBytes,
    speedMbps,
    etaSeconds,
    catalogueBytes,
    consequence,
    startLabel,
    confirmingCancel: isConfirming,
    onRequestCancel,
    onDismissCancel,
    onConfirmCancel,
    onStart,
  }: DownloadCardProps) => {
    // The catalogue knows the size before a byte moves; the live total only corrects it if
    // the catalogue never answered. Both are bytes, so they cannot disagree by a base.
    const advertisedBytes = catalogueBytes ?? (totalBytes > 0 ? totalBytes : null);
    const progressTotalBytes = totalBytes > 0 ? totalBytes : catalogueBytes ?? 0;
    const showProgress =
      status === 'downloading' || status === 'stopping' || status === 'completed';
    const isDownloading = status === 'downloading';

    return (
      <div className="bg-surface-raised rounded-xl border border-hairline p-5">
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-full bg-surface-2 flex items-center justify-center">
              {icon}
            </div>
            <div>
              <h3 className="font-medium text-ink">{title}</h3>
              {advertisedBytes !== null && (
                <p className="text-sm text-ink-muted">{formatBytes(advertisedBytes)}</p>
              )}
            </div>
          </div>
          <div>
            {status === 'idle' && <span className="text-sm text-ink-muted">Not started</span>}
            {status === 'downloading' && <Loader2 className="w-5 h-5 text-ink animate-spin" />}
            {status === 'stopping' && <span className="text-sm text-ink-muted">Stopping…</span>}
            {status === 'completed' && (
              <div className={cn('w-6 h-6 rounded-full flex items-center justify-center', badge.success)}>
                <Check className="w-4 h-4" />
              </div>
            )}
            {status === 'error' && <span className={cn('text-sm', toneText.error)}>Failed</span>}
            {status === 'cancelled' && <span className="text-sm text-ink-muted">Cancelled</span>}
          </div>
        </div>

        {/* Progress Bar */}
        {showProgress && (
          <div className="space-y-2">
            <div className={cn('w-full h-2 rounded-full overflow-hidden', progressClasses.track)}>
              <div
                className={cn('h-full rounded-full transition-all duration-300', progressClasses.fill)}
                style={{ width: `${percent}%` }}
              />
            </div>
            <div className="flex items-baseline justify-between gap-3 text-sm">
              <span className="text-ink-muted">
                {formatBytes(downloadedBytes)} / {formatBytes(progressTotalBytes)}
              </span>
              <div className="flex items-baseline gap-2">
                {/* "How long" sits next to "how far"; while the rate is unstable this says so
                    rather than going blank or inventing a number. */}
                {isDownloading && percent < 100 && (
                  <span className="text-ink-muted">{formatEta(etaSeconds)}</span>
                )}
                <span className="font-semibold text-ink">{Math.round(percent)}%</span>
              </div>
            </div>
            {isDownloading && speedMbps > 0 && (
              <p className="text-xs text-ink-subtle">
                {formatBytesPerSecond(mibToBytes(speedMbps))}
              </p>
            )}
          </div>
        )}

        {status === 'error' && (
          <div className={cn('mt-3 p-3 rounded-md', panel.error)}>
            <p className="text-sm font-medium">Download Error</p>
            {error && <p className="text-xs mt-1">{error}</p>}
          </div>
        )}

        {(status === 'cancelled' || status === 'idle') && (
          <p className="mt-3 text-sm text-ink-muted">{consequence}</p>
        )}

        {/* Actions: a way to stop while it runs, a way to start again once it has stopped. */}
        <div className="mt-4">
          <AnimatePresence mode="wait" initial={false}>
            {isConfirming && status === 'downloading' ? (
              <motion.div
                key="confirm"
                initial={{ opacity: 0, y: -6 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -6 }}
                transition={{ duration: 0.2, ease: 'easeOut' }}
                className="rounded-lg bg-surface-2 p-3"
              >
                <p className="text-sm text-ink">Cancel this download?</p>
                <p className="mt-1 text-sm text-ink-muted">{consequence}</p>
                <div className="mt-3 flex items-center gap-2">
                  <Button variant="ghost" size="sm" onClick={onDismissCancel}>
                    Keep downloading
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={onConfirmCancel}
                    className={cn(toneText.error, 'hover:text-error')}
                  >
                    Cancel download
                  </Button>
                </div>
              </motion.div>
            ) : status === 'downloading' ? (
              <motion.div
                key="cancel"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={{ duration: 0.2, ease: 'easeOut' }}
              >
                <Button variant="ghost" size="sm" onClick={onRequestCancel}>
                  Cancel download
                </Button>
              </motion.div>
            ) : status === 'stopping' ? (
              <motion.p
                key="stopping"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={{ duration: 0.2, ease: 'easeOut' }}
                className="text-sm text-ink-muted"
              >
                Stopping the download — this can take a moment.
              </motion.p>
            ) : status === 'idle' || status === 'cancelled' || status === 'error' ? (
              <motion.div
                key="start"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={{ duration: 0.2, ease: 'easeOut' }}
              >
                <Button variant="outline" size="sm" onClick={onStart}>
                  <RefreshCw className="w-4 h-4" />
                  {startLabel}
                </Button>
              </motion.div>
            ) : null}
          </AnimatePresence>
        </div>
      </div>
    );
  };

  // Every state names what the button is waiting for; a bare spinner says only "wait", and
  // not for what or for how much longer.
  const continueLabel = parakeetDownloaded
    ? 'Continue'
    : parakeetStatus === 'downloading'
    ? `Downloading transcription engine… ${Math.round(parakeetProgressInfo.percent)}%`
    : parakeetStatus === 'stopping'
    ? 'Stopping transcription engine…'
    : parakeetStatus === 'cancelled' || parakeetStatus === 'error'
    ? 'Transcription engine needed to continue'
    : 'Waiting for transcription engine';

  const continueBusy =
    !parakeetDownloaded && (parakeetStatus === 'downloading' || parakeetStatus === 'stopping');

  return (
    <OnboardingContainer
      title="Getting things ready"
      description="Next, a quick check that Minutes can hear you."
      step={3}
      totalSteps={isMac ? 5 : 4}
    >
      <div className="flex flex-col items-center space-y-6">
        {/* Download Cards */}
        <div className="w-full max-w-lg space-y-4">
          {renderDownloadCard({
            title: 'Transcription Engine',
            icon: <Mic className="w-5 h-5 text-ink-muted" />,
            status: parakeetStatus,
            error: parakeetError,
            percent: parakeetProgressInfo.percent,
            downloadedBytes: parakeetProgressInfo.downloadedBytes,
            totalBytes: parakeetProgressInfo.totalBytes,
            speedMbps: parakeetProgressInfo.speedMbps,
            etaSeconds: parakeetProgressInfo.etaSeconds,
            catalogueBytes: parakeetSizeBytes,
            consequence: PARAKEET_CANCEL_CONSEQUENCE,
            startLabel: parakeetStatus === 'error' ? 'Try Again' : 'Start download',
            confirmingCancel: confirmingCancel === 'parakeet',
            onRequestCancel: () => setConfirmingCancel('parakeet'),
            onDismissCancel: () => setConfirmingCancel(null),
            onConfirmCancel: handleCancelParakeet,
            onStart: handleRetryDownload,
          })}

          {renderDownloadCard({
            title: 'Summary Engine',
            icon: <Sparkles className="w-5 h-5 text-ink-muted" />,
            status: summaryStatus,
            error: summaryError,
            percent: summaryModelProgressInfo.percent,
            downloadedBytes: summaryModelProgressInfo.downloadedBytes,
            totalBytes: summaryModelProgressInfo.totalBytes,
            speedMbps: summaryModelProgressInfo.speedMbps,
            etaSeconds: summaryModelProgressInfo.etaSeconds,
            catalogueBytes: summaryModelSizeBytes,
            consequence: SUMMARY_CANCEL_CONSEQUENCE,
            startLabel: summaryStatus === 'error' ? 'Try Again' : 'Start download',
            confirmingCancel: confirmingCancel === 'summary',
            onRequestCancel: () => setConfirmingCancel('summary'),
            onDismissCancel: () => setConfirmingCancel(null),
            onConfirmCancel: handleCancelSummary,
            onStart:
              summaryStatus === 'error' ? handleRetrySummaryDownload : startSummaryDownload,
          })}
        </div>

        {/* The summary model never blocks anyone, so say so while it is still running rather
            than only once the transcription engine has finished. */}
        <AnimatePresence>
          {summaryStatus === 'downloading' && (
            <motion.div
              initial={{ opacity: 0, y: -10 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -10 }}
              transition={{ duration: 0.3, ease: 'easeOut' }}
              className="w-full max-w-lg bg-surface-2 rounded-lg p-4 text-sm text-ink"
            >
              <div className="flex items-start gap-3">
                <Download className="w-5 h-5 text-ink-muted flex-shrink-0 mt-0.5" />
                <div>
                  <p className="font-medium">
                    {parakeetDownloaded
                      ? 'You can continue while this finishes'
                      : "The summary engine won't hold you up"}
                  </p>
                  <p className="text-ink mt-1">
                    {parakeetDownloaded
                      ? 'Download will continue in the background.'
                      : 'It keeps downloading in the background once you continue.'}
                  </p>
                </div>
              </div>
            </motion.div>
          )}
        </AnimatePresence>

        {/* Continue Button */}
        <div className="w-full max-w-sm">
          <Button
            onClick={handleContinue}
            disabled={!parakeetDownloaded}
            className="w-full h-11 bg-brand hover:bg-brand text-brand-foreground disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {continueBusy && <Loader2 className="w-4 h-4 animate-spin" />}
            {continueLabel}
          </Button>
        </div>
      </div>
    </OnboardingContainer>
  );
}
