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
  onRetry?: () => void;
}) {
  const { cancelTranscription, pauseTranscription, resumeTranscription } = useMeetingActivity();
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
  const action = activity.status === 'failed' && onRetry
    ? { label: 'Retry', run: onRetry }
    : activity.controls_available && activity.status === 'paused'
      ? { label: 'Resume', run: () => void resumeTranscription(activity.task_id) }
      : activity.controls_available && activity.status === 'transcribing'
        ? { label: 'Pause', run: () => void pauseTranscription(activity.task_id) }
        : activity.controls_available && activity.status === 'queued'
          ? { label: 'Cancel', run: () => void cancelTranscription(activity.task_id) }
          : null;
  const message = activity.error
    ?? activity.warning
    ?? activity.message
    ?? activity.stage?.replace(/_/g, ' ')
    ?? (activity.status === 'queued' ? 'Waiting to transcribe' : activity.status);

  return (
    <StatusFeedback
      tone={tone}
      pending={pending}
      actionLabel={action?.label}
      onAction={action?.run}
    >
      {message}
    </StatusFeedback>
  );
}

// Main exported component - renders multiple status overlays
export function StatusOverlays({
  isProcessing: legacyProcessing,
  isSaving: legacySaving,
  sidebarCollapsed
}: StatusOverlaysProps) {
  const { recording, snapshot } = useMeetingActivity();
  const isProcessing = Boolean(legacyProcessing) || snapshot.activities.some((activity) => (
    activity.status === 'queued' || activity.status === 'transcribing'
  ));
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
