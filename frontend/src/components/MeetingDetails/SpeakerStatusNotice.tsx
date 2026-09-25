'use client';

import { AlertCircle, Loader2, Users } from 'lucide-react';
import type { DiarizationStatus } from '@/hooks/useDiarizationStatus';
import { toneText } from '@/lib/theme-classes';

interface SpeakerStatusNoticeProps {
  status: DiarizationStatus | null;
  /** Progress of a speaker stage running inside a transcription pass, if any. */
  progress?: { percent: number; indeterminate: boolean; label: string } | null;
  /** True while a retry started from this notice is waiting to be picked up. */
  isStarting?: boolean;
  onRetry: () => void;
}

/**
 * One line under the transcript header saying whether speakers are identified,
 * with a retry when a run failed, was cut off by a quit, or never happened.
 * Renders nothing once speakers are applied.
 */
export function SpeakerStatusNotice({ status, progress, isStarting = false, onRetry }: SpeakerStatusNoticeProps) {
  if (progress || status?.state === 'running' || isStarting) {
    const label = progress?.label ?? 'Identifying speakers';
    const percent = progress && !progress.indeterminate ? ` · ${progress.percent}%` : '';
    return (
      <p className="mt-2 flex items-center gap-1.5 text-[11px] text-[var(--ink-subtle)]" role="status">
        <Loader2 className="h-3 w-3 animate-spin" />
        {label}…{percent} The transcript is ready; speaker names will fill in when this finishes.
      </p>
    );
  }
  if (!status || status.state === 'done') return null;

  const { icon, message, action } = (() => {
    switch (status.state) {
      case 'failed':
        return {
          icon: <AlertCircle className={`h-3 w-3 shrink-0 ${toneText.error}`} />,
          message: `Speaker identification failed: ${status.error}`,
          action: 'Retry',
        };
      case 'pending':
        return {
          icon: <AlertCircle className="h-3 w-3 shrink-0" />,
          message: 'Speaker identification was interrupted before it finished.',
          action: 'Retry',
        };
      case 'missing':
        return {
          icon: <Users className="h-3 w-3 shrink-0" />,
          message: 'Speakers have not been identified for this meeting.',
          action: 'Identify speakers',
        };
    }
  })();

  return (
    <p className="mt-2 flex min-w-0 items-center gap-1.5 text-[11px] text-[var(--ink-subtle)]">
      {icon}
      <span className="min-w-0 truncate" title={message}>{message}</span>
      <button
        type="button"
        onClick={onRetry}
        className="shrink-0 font-semibold text-[var(--ink-muted)] underline underline-offset-2 hover:text-ink"
      >
        {action}
      </button>
    </p>
  );
}
