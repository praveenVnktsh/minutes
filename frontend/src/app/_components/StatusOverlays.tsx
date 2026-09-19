'use client';

import { useState } from 'react';
import { StatusFeedback } from '@/components/ui/status-feedback';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import type { MeetingActivity } from '@/types/meetingActivity';

interface StatusOverlaysProps {
  // Status flags
  isProcessing?: boolean;      // Processing transcription after recording stops
  isSaving?: boolean;          // Saving transcript to database

  // Layout
  sidebarCollapsed: boolean;  // For responsive margin calculation
}

// Internal reusable component for individual status overlays
interface StatusOverlayProps {
  show: boolean;
  message: string;
  sidebarCollapsed: boolean;
}

function StatusOverlay({ show, message, sidebarCollapsed }: StatusOverlayProps) {
  if (!show) return null;

  return (
    <div className="fixed bottom-4 left-0 right-0 z-10">
      <div
        className="flex justify-center pl-8 transition-[margin] duration-300"
        style={{
          marginLeft: sidebarCollapsed ? '4rem' : '16rem'
        }}
      >
        <div className="w-2/3 max-w-[750px] flex justify-center">
          <div className="bg-surface-raised rounded-lg shadow-lg px-4 py-2 flex items-center space-x-2">
            <StatusFeedback pending tone="info" className="text-sm">
              {message}
            </StatusFeedback>
          </div>
        </div>
      </div>
    </div>
  );
}

export function MeetingActivityFeedback({
  activity,
  onRetry,
}: {
  activity: MeetingActivity;
  onRetry?: () => void | Promise<void>;
}) {
  const { cancelTranscription, pauseTranscription, resumeTranscription } = useMeetingActivity();
  const [commandError, setCommandError] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const pending = activity.status === 'starting'
    || activity.status === 'queued'
    || activity.status === 'saving'
    || activity.status === 'transcribing';
  const tone = activity.status === 'failed'
    ? 'error'
    : activity.status === 'ready'
      ? activity.warning ? 'warning' : 'success'
      : activity.status === 'cancelled'
        ? 'neutral'
        : activity.status === 'paused'
          ? 'paused'
          : activity.status === 'recording'
            ? 'recording'
        : 'info';
  const runAction = async (label: string, action: () => boolean | void | Promise<boolean | void>) => {
    setPendingAction(label);
    setCommandError(null);
    try {
      const accepted = await action();
      if (accepted === false) throw new Error(`${label} is no longer available for this task.`);
    } catch (error) {
      setCommandError(error instanceof Error ? error.message : String(error));
    } finally {
      setPendingAction(null);
    }
  };
  const actions: Array<{ label: string; run: () => boolean | void | Promise<boolean | void> }> = [];
  if (activity.status === 'failed' && onRetry) actions.push({ label: 'Retry', run: onRetry });
  if (activity.controls_available && activity.status === 'paused') {
    actions.push({ label: 'Resume', run: () => resumeTranscription(activity.task_id) });
  }
  if (activity.controls_available && activity.status === 'transcribing') {
    actions.push({ label: 'Pause', run: () => pauseTranscription(activity.task_id) });
  }
  if (activity.controls_available && ['queued', 'transcribing', 'paused'].includes(activity.status)) {
    actions.push({ label: 'Cancel', run: () => cancelTranscription(activity.task_id) });
  }
  const message = activity.error
    ?? activity.warning
    ?? activity.message
    ?? activity.stage?.replace(/_/g, ' ')
    ?? (activity.status === 'queued' ? 'Waiting to transcribe' : activity.status);

  return (
    <div className="flex flex-col gap-1">
      <StatusFeedback tone={tone} pending={pending || pendingAction !== null}>
        {message}
      </StatusFeedback>
      {actions.length > 0 && (
        <div className="flex gap-2" aria-label="Task actions">
          {actions.map((action) => (
            <button
              key={action.label}
              type="button"
              disabled={pendingAction !== null}
              className="text-xs font-semibold text-info underline underline-offset-2 disabled:opacity-50"
              onClick={() => void runAction(action.label, action.run)}
            >
              {pendingAction === action.label ? `${action.label}…` : action.label}
            </button>
          ))}
        </div>
      )}
      {commandError && <StatusFeedback tone="error">{commandError}</StatusFeedback>}
    </div>
  );
}

// Main exported component - renders multiple status overlays
export function StatusOverlays({
  isProcessing: legacyProcessing,
  isSaving: legacySaving,
  sidebarCollapsed
}: StatusOverlaysProps) {
  const { recording } = useMeetingActivity();
  const isProcessing = Boolean(legacyProcessing);
  const isSaving = Boolean(legacySaving) || recording?.status === 'saving';
  return (
    <>
      {/* Processing status overlay - shown after recording stops while finalizing transcription */}
      <StatusOverlay
        show={isProcessing}
        message="Finalizing transcription..."
        sidebarCollapsed={sidebarCollapsed}
      />

      {/* Saving status overlay - shown while saving transcript to database */}
      <StatusOverlay
        show={isSaving}
        message="Saving transcript..."
        sidebarCollapsed={sidebarCollapsed}
      />
    </>
  );
}
