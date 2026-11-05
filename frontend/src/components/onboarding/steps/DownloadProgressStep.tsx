import React, { useEffect, useState, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { Mic, Sparkles, Check, Loader2, Download, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { OnboardingContainer } from '../OnboardingContainer';
import { useOnboarding } from '@/contexts/OnboardingContext';
import { toast } from 'sonner';
import { motion, AnimatePresence } from 'framer-motion';
import { formatBytes, formatBytesPerSecond, formatEta, mibToBytes } from '@/lib/download-display';
import { speechModelsApi } from '@/lib/speechModels';
import { TranscriptionUnsupportedNotice } from '@/components/TranscriptionUnsupportedNotice';
import {
  BUILTIN_AI_DOWNLOAD_PROGRESS_EVENT,
  type BuiltInAIDownloadProgressEvent,
} from '@/lib/builtin-ai';
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
const SPEECH_CANCEL_CONSEQUENCE =
  'Meetings cannot be transcribed until the speech models are downloaded.';
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
    selectedSummaryModel,
    speechDownloaded,
    transcriptionSupported,
    setSpeechDownloaded,
    speechDownloadState,
    speechDownloadError,
    speechProgressInfo,
    speechSizeBytes,
    summaryModelDownloaded,
    summaryModelProgressInfo,
    summaryModelSizeBytes,
    isBackgroundDownloading,
    startBackgroundDownloads,
    retrySpeechDownload,
    retrySummaryDownload,
    cancelSpeechDownload,
    cancelSummaryDownload,
  } = useOnboarding();

  // The transfers are authorised by the click on the previous screen, so on arrival a
  // download is either already running, already finished, or was never started at all —
  // and the last of those has to look different from the first. The speech run's state
  // lives on the context, which owns the download call; this screen only adds the gap
  // between asking to cancel and the run reporting it stopped.
  const [speechStopping, setSpeechStopping] = useState(false);
  const speechStatus: DownloadStatus =
    speechStopping && speechDownloadState === 'downloading' ? 'stopping' : speechDownloadState;

  // Before macOS 15 there is no speech download to wait for: the step is skipped and only
  // the summary engine is shown.
  const speechReady = speechDownloaded || !transcriptionSupported;

  const [summaryStatus, setSummaryStatus] = useState<DownloadStatus>(() =>
    summaryModelDownloaded
      ? 'completed'
      : isBackgroundDownloading && selectedSummaryModel
      ? 'downloading'
      : 'idle'
  );
  const [summaryError, setSummaryError] = useState<string | undefined>();

  const [confirmingCancel, setConfirmingCancel] = useState<'speech' | 'summary' | null>(null);

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

    console.log('[DownloadProgressStep] Retrying speech model download');
    retryingRef.current = true;
    setConfirmingCancel(null);

    try {
      // Through the context rather than a bare invoke, so the shared in-flight set knows
      // this transfer is running again. It returns once the run has settled.
      await retrySpeechDownload();
    } catch (error) {
      console.error('[DownloadProgressStep] Retry failed:', error);
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
      // Routed through the context like the speech model retry above, for the
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

  const handleCancelSpeech = async () => {
    setConfirmingCancel(null);
    setSpeechStopping(true);

    try {
      // The card says "stopping" until the run reports the models cancelled.
      await cancelSpeechDownload();
    } catch (error) {
      console.error('[DownloadProgressStep] Failed to cancel the speech model download:', error);
      setSpeechStopping(false);
      toast.error('Could not cancel the download', {
        description: 'The speech models are still downloading.',
      });
    }
  };

  useEffect(() => {
    if (speechDownloadState !== 'downloading') setSpeechStopping(false);
  }, [speechDownloadState]);

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

  // The summary model name resolves asynchronously and may still have been empty when the
  // previous screen fired the downloads, so this catches up once it arrives. It starts the
  // summary model and nothing else: the speech models are started by that click, or
  // by the card's own control here.
  useEffect(() => {
    if (summaryDownloadStartedRef.current) return;
    if (!selectedSummaryModel) return;
    if (summaryModelDownloaded) return;
    if (summaryStatus !== 'idle') return;
    summaryDownloadStartedRef.current = true;

    startSummaryDownload();
  }, [selectedSummaryModel, summaryModelDownloaded, summaryStatus]);

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
    if (summaryModelDownloaded) setSummaryStatus('completed');
  }, [summaryModelDownloaded]);

  const startSummaryDownload = async () => {
    if (summaryModelDownloaded || !selectedSummaryModel) return;

    setConfirmingCancel(null);
    setSummaryError(undefined);
    setSummaryStatus('downloading');

    try {
      await startBackgroundDownloads({
        includeSpeech: false,
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
    // Nothing to verify when this macOS cannot run the speech models.
    if (transcriptionSupported) {
      try {
        const actuallyAvailable = (await speechModelsApi.status()).batchReady;

        if (actuallyAvailable && !speechDownloaded) {
          console.log('[DownloadProgressStep] Speech models available but state not updated');
          setSpeechDownloaded(true);
        } else if (!actuallyAvailable && (speechStatus === 'error' || speechStatus === 'cancelled')) {
          toast.error('Speech models required', {
            description: 'Please retry the download before continuing.',
          });
          return;
        }
      } catch (error) {
        console.warn('[DownloadProgressStep] Failed to verify the speech models:', error);
      }
    }

    // Check if downloads are complete for toast notification
    const downloadsComplete =
      (!transcriptionSupported || speechStatus === 'completed') && summaryStatus === 'completed';

    // Show toast if downloads still in progress
    if (!downloadsComplete) {
      toast.info('Downloads will continue in the background', {
        description: 'You can start using the app. The remaining models finish in the background.',
        duration: 5000,
      });
    }

    // Permissions come next
    goNext();
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
  const continueLabel = speechReady
    ? 'Continue'
    : speechStatus === 'downloading'
    ? `Downloading speech models… ${Math.round(speechProgressInfo.percent)}%`
    : speechStatus === 'stopping'
    ? 'Stopping speech models…'
    : speechStatus === 'cancelled' || speechStatus === 'error'
    ? 'Speech models needed to continue'
    : 'Waiting for speech models';

  const continueBusy =
    !speechReady && (speechStatus === 'downloading' || speechStatus === 'stopping');

  return (
    <OnboardingContainer
      title="Getting things ready"
      description="Next, a quick check that Minutes can hear you."
      step={3}
      totalSteps={5}
    >
      <div className="flex flex-col items-center space-y-6">
        {/* Download Cards */}
        <div className="w-full max-w-lg space-y-4">
          {!transcriptionSupported ? (
            <TranscriptionUnsupportedNotice />
          ) : renderDownloadCard({
            title: 'Speech Models',
            icon: <Mic className="w-5 h-5 text-ink-muted" />,
            status: speechStatus,
            error: speechDownloadError,
            percent: speechStatus === 'completed' ? 100 : speechProgressInfo.percent,
            downloadedBytes: speechProgressInfo.downloadedBytes,
            totalBytes: speechProgressInfo.totalBytes,
            speedMbps: speechProgressInfo.speedMbps,
            etaSeconds: speechProgressInfo.etaSeconds,
            // The run's own total once it has one; the missing models' size before.
            catalogueBytes: speechProgressInfo.totalBytes > 0 ? speechProgressInfo.totalBytes : speechSizeBytes,
            consequence: SPEECH_CANCEL_CONSEQUENCE,
            startLabel: speechStatus === 'error' ? 'Try Again' : 'Start download',
            confirmingCancel: confirmingCancel === 'speech',
            onRequestCancel: () => setConfirmingCancel('speech'),
            onDismissCancel: () => setConfirmingCancel(null),
            onConfirmCancel: handleCancelSpeech,
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
            than only once the speech models have finished. */}
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
                    {speechReady
                      ? 'You can continue while this finishes'
                      : "The summary engine won't hold you up"}
                  </p>
                  <p className="text-ink mt-1">
                    {speechReady
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
            disabled={!speechReady}
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
