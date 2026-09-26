'use client';

import { useState } from 'react';
import { Mic, Pause, Play, Sparkles, Square } from 'lucide-react';
import type { SummaryResponse } from '@/types/summary';
import { TooltipProvider } from '@/components/ui/tooltip';
import { StatusFeedback } from '@/components/ui/status-feedback';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useRecordingController } from '@/contexts/RecordingControllerContext';
import Analytics from '@/lib/analytics';
import { RecordingWaveform } from '@/components/RecordingWaveform';

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
  const hours = Math.floor(time / 3600);
  const minutes = Math.floor((time % 3600) / 60);
  const seconds = Math.floor(time % 60);
  const pad = (n: number) => n.toString().padStart(2, '0');
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(seconds)}` : `${pad(minutes)}:${pad(seconds)}`;
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
        <div className="flex h-[52px] items-center gap-3 rounded-full border border-hairline bg-[color-mix(in_srgb,var(--surface-raised)_90%,transparent)] p-1.5 shadow-[0_18px_40px_-16px_rgba(0,0,0,0.35)] backdrop-blur-xl">
          {isParentProcessing ? (
            <div className="flex items-center gap-2.5 px-5">
              <Sparkles className="h-4 w-4 animate-[spin_2.4s_linear_infinite] text-info" />
              <span className="animate-shimmer bg-[linear-gradient(90deg,var(--ink-subtle)_0%,var(--ink)_40%,var(--ink-subtle)_80%)] bg-[length:200%_100%] bg-clip-text text-sm font-semibold text-transparent">
                Finishing your meeting…
              </span>
            </div>
          ) : !isRecording ? (
            <button
              type="button"
              onClick={() => {
                Analytics.trackButtonClick('start_recording', 'recording_controls');
                void run(onRecordingStart);
              }}
              disabled={pending || isRecordingDisabled}
              className="flex h-10 min-w-[174px] items-center justify-center gap-2 rounded-full bg-brand px-5 text-sm font-semibold text-brand-foreground transition hover:opacity-90 disabled:opacity-40"
            >
              {controller.command === 'start' ? (
                <div className="h-4 w-4 animate-spin rounded-full border-2 border-current border-t-transparent" />
              ) : <Mic className="h-4 w-4" />}
              {controller.command === 'start' ? 'Starting...' : 'Start meeting'}
            </button>
          ) : (
            <>
              <div className="flex items-center gap-2.5 pl-3">
                <span className={`h-2 w-2 flex-none rounded-full ${recordingState.isPaused ? 'bg-paused' : 'animate-pulse bg-recording'}`} />
                <span className="font-mono text-sm font-semibold tabular-nums text-ink">
                  {formatTime(recordingState.activeDuration ?? recordingState.recordingDuration ?? 0)}
                </span>
                {recordingState.isPaused ? <span className="sr-only">Paused</span> : null}
              </div>
              <RecordingWaveform active={!recordingState.isPaused} className="h-7 w-[120px]" />
              <button
                type="button"
                onClick={() => void run(recordingState.isPaused
                  ? controller.resumeRecording
                  : controller.pauseRecording)}
                disabled={pending}
                className="flex h-9 w-9 items-center justify-center rounded-full bg-surface-2 text-ink transition hover:bg-selected disabled:opacity-40"
                aria-label={recordingState.isPaused ? 'Resume recording' : 'Pause recording'}
                title={recordingState.isPaused ? 'Resume recording' : 'Pause recording'}
              >
                {recordingState.isPaused ? <Play className="h-3.5 w-3.5 fill-current" /> : <Pause className="h-3.5 w-3.5 fill-current" />}
              </button>
              <button
                type="button"
                onClick={() => {
                  Analytics.trackButtonClick('stop_recording', 'recording_controls');
                  onStopInitiated?.();
                  void run(() => onRecordingStop(true));
                }}
                disabled={pending}
                aria-label="End meeting"
                title="End meeting"
                className="flex h-9 w-9 items-center justify-center rounded-full bg-recording text-recording-foreground transition hover:opacity-90 active:scale-90 disabled:opacity-40"
              >
                {controller.command === 'stop' || controller.command === 'finalize' ? (
                  <div className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-current border-t-transparent" />
                ) : <Square className="h-3.5 w-3.5 fill-current" />}
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
