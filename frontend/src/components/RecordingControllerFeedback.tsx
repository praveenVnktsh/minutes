'use client';

import { X } from 'lucide-react';
import { StatusFeedback } from '@/components/ui/status-feedback';
import { useRecordingController } from '@/contexts/RecordingControllerContext';

/**
 * Persistent shell-level feedback for recording commands and setup failures.
 * Anchored top-center so it never covers the floating stop/pause controls at
 * the bottom of the live workspace (toasts sit top-right).
 */
export function RecordingControllerFeedback() {
  const controller = useRecordingController();
  if (!controller.feedback) return null;

  return (
    <div className="fixed top-10 left-1/2 z-50 w-[min(576px,calc(100vw-2rem))] -translate-x-1/2 rounded-xl border border-[var(--hairline)] bg-[var(--surface-raised)] px-4 py-3 shadow-lg">
      <div className="flex items-start gap-3">
        <StatusFeedback
          tone={controller.feedback.kind === 'warning' ? 'warning' : 'error'}
          actionLabel={controller.canRetryFeedback ? 'Retry' : undefined}
          onAction={controller.canRetryFeedback ? () => void controller.retryFeedback() : undefined}
          className="text-sm"
        >
          {controller.feedback.title}: {controller.feedback.message}
        </StatusFeedback>
        {controller.feedback.settingsSection ? (
          <button
            type="button"
            className="rounded-sm text-xs font-semibold text-[var(--ink-muted)] underline underline-offset-2 hover:no-underline"
            onClick={() => void controller.openFeedbackSettings()}
          >
            Open settings
          </button>
        ) : null}
        <button
          type="button"
          aria-label="Dismiss recording message"
          className="rounded p-1 text-[var(--ink-muted)] hover:bg-[var(--surface-2)]"
          onClick={controller.dismissFeedback}
        >
          <X className="h-4 w-4" />
        </button>
      </div>
    </div>
  );
}
