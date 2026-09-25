'use client';

import { useState } from 'react';
import { Mic, Pause, Play, Square } from 'lucide-react';
import type { SummaryResponse } from '@/types/summary';
import { TooltipProvider } from '@/components/ui/tooltip';
import { StatusFeedback } from '@/components/ui/status-feedback';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useRecordingController } from '@/contexts/RecordingControllerContext';
import Analytics from '@/lib/analytics';

interface RecordingControlsProps {
  isRecording: boolean;
  barHeights: string[];
  onRecordingStop: (callApi?: boolean) => void | Promise<void>;
  onRecordingStart: () => void | Promise<void>;
  onTranscriptReceived: (summary: SummaryResponse) => void;
  onTranscriptionError?: (message: string) => void;
  onStopInitiated?: () => void;
  isRecordingDisabled: boolean;
  isParentProcessing: boolean;
  selectedDevices?: { micDevice: string | null; systemDevice: string | null };
  meetingName?: string;
}

function formatTime(time: number): string {
  const minutes = Math.floor(time / 60);
  const seconds = Math.floor(time % 60);
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

export const RecordingControls: React.FC<RecordingControlsProps> = ({
  isRecording,
  onRecordingStop,
  onRecordingStart,
  onStopInitiated,
  isRecordingDisabled,
  isParentProcessing,
}) => {
  const recordingState = useRecordingState();
  const controller = useRecordingController();
  const [localPending, setLocalPending] = useState(false);
  const pending = localPending || controller.isCommandPending;

  const run = async (operation: () => void | Promise<void>) => {
    if (pending) return;
    setLocalPending(true);
    try {
      await operation();
    } catch {
      // The controller owns persistent, actionable feedback.
    } finally {
      setLocalPending(false);
    }
  };

  return (
    <TooltipProvider>
      <div className="flex flex-col space-y-2">
        <div className="flex min-h-14 items-center gap-2 rounded-2xl border border-[var(--hairline)] bg-[var(--surface-raised)] p-1.5 shadow-[0_12px_35px_rgba(45,43,37,0.14)] backdrop-blur">
          {isParentProcessing ? (
            <div className="flex items-center gap-2 px-4 py-2">
              <div className="h-4 w-4 animate-spin rounded-full border-2 border-[var(--hairline)] border-t-[var(--ink)]" />
              <span className="text-sm text-[var(--ink-muted)]">Finishing your meeting...</span>
            </div>
          ) : !isRecording ? (
            <button
              type="button"
              onClick={() => {
                Analytics.trackButtonClick('start_recording', 'recording_controls');
                void run(onRecordingStart);
              }}
              disabled={pending || isRecordingDisabled}
              className="flex h-11 min-w-[174px] items-center justify-center gap-2 rounded-xl bg-brand px-5 text-sm font-semibold text-brand-foreground transition hover:opacity-90 disabled:opacity-40"
            >
              {controller.command === 'start' ? (
                <div className="h-4 w-4 animate-spin rounded-full border-2 border-current border-t-transparent" />
              ) : <Mic className="h-4 w-4" />}
              {controller.command === 'start' ? 'Starting...' : 'Start meeting'}
            </button>
          ) : (
            <>
              <div className="flex min-w-[104px] items-center gap-2 px-3 text-sm font-medium text-[var(--ink-muted)]">
                <span className={`h-2 w-2 rounded-full ${recordingState.isPaused ? 'bg-paused' : 'animate-pulse bg-recording'}`} />
                {formatTime(recordingState.activeDuration ?? recordingState.recordingDuration ?? 0)}
                {recordingState.isPaused ? <span className="sr-only">Paused</span> : null}
              </div>
              <button
                type="button"
                onClick={() => void run(recordingState.isPaused
                  ? controller.resumeRecording
                  : controller.pauseRecording)}
                disabled={pending}
                className="flex h-10 w-10 items-center justify-center rounded-xl text-[var(--ink-muted)] hover:bg-[var(--surface-2)] disabled:opacity-40"
                aria-label={recordingState.isPaused ? 'Resume recording' : 'Pause recording'}
                title={recordingState.isPaused ? 'Resume recording' : 'Pause recording'}
              >
                {recordingState.isPaused ? <Play className="h-4 w-4" /> : <Pause className="h-4 w-4" />}
              </button>
              <button
                type="button"
                onClick={() => {
                  Analytics.trackButtonClick('stop_recording', 'recording_controls');
                  onStopInitiated?.();
                  void run(() => onRecordingStop(true));
                }}
                disabled={pending}
                className="flex h-10 items-center gap-2 rounded-xl bg-recording px-4 text-sm font-semibold text-recording-foreground hover:opacity-90 disabled:opacity-40"
              >
                <Square className="h-3.5 w-3.5 fill-current" />
                {controller.command === 'stop' || controller.command === 'finalize' ? 'Ending...' : 'End meeting'}
              </button>
            </>
          )}
        </div>
        {controller.feedback && !controller.feedbackManagedGlobally ? (
          <div className="flex items-center gap-2">
            <StatusFeedback
              tone={controller.feedback.kind === 'warning' ? 'warning' : 'error'}
              actionLabel={controller.canRetryFeedback ? 'Retry' : undefined}
              onAction={controller.canRetryFeedback ? () => void controller.retryFeedback() : undefined}
              className="max-w-xl rounded-lg bg-[var(--surface-raised)] px-3 py-2"
            >
              {controller.feedback.title}: {controller.feedback.message}
            </StatusFeedback>
            {controller.feedback.settingsSection ? (
              <button
                type="button"
                className="text-xs font-semibold text-[var(--ink-muted)] underline underline-offset-2 hover:no-underline"
                onClick={() => void controller.openFeedbackSettings()}
              >
                Open settings
              </button>
            ) : null}
          </div>
        ) : null}
      </div>
    </TooltipProvider>
  );
};
