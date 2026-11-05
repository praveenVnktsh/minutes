'use client';

import { useState } from 'react';
import { StatusFeedback } from '@/components/ui/status-feedback';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import type { MeetingActivity } from '@/types/meetingActivity';

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
