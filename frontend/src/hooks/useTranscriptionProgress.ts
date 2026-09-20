'use client';

import { useMemo } from 'react';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';
import type { MeetingActivityKind } from '@/types/meetingActivity';

/**
 * Combined transcription progress for a single meeting.
 *
 * Transcription runs as several stages. The batch pass reports its own stages
 * ("decoding", "vad", "transcribing", "saving") on `retranscription-progress`,
 * and speaker diarization reports the stages that follow on
 * `diarization-progress`. Both events carry a `meeting_id`, so a meeting's
 * workspace can show one continuous percentage across every stage.
 *
 * The same activity stream covers a first pass (`kind: 'import' | 'recording'`)
 * and a later re-run (`kind: 'retranscription'`), so this hook is the single
 * source of progress for both — callers branch on `kind` when they only care
 * about one of them.
 */
export interface TranscriptionProgress {
  /** The activity's task id, for cancelTranscription(taskId). */
  taskId: string;
  /** 'import' | 'recording' | 'retranscription' — what kind of pass this is. */
  kind: MeetingActivityKind;
  /** Overall completion across every phase, 0-100. */
  percent: number;
  /**
   * True while the backend has not reported a percentage yet. `percent` is 0
   * then, which is a placeholder rather than a measurement, so callers should
   * show an indeterminate indicator instead of "0%".
   */
  indeterminate: boolean;
  /** Short human label for the current phase, e.g. "Transcribing". */
  stageLabel: string;
  /** The backend's own description of the phase, when it sends one. */
  message: string | null;
  /** True while the run can still be cancelled (activity.controls_available). */
  cancellable: boolean;
}

const RETRANSCRIPTION_STAGES: Record<string, string> = {
  copying: 'Preparing audio',
  decoding: 'Decoding audio',
  resampling: 'Converting audio',
  vad: 'Detecting speech',
  transcribing: 'Transcribing',
  saving: 'Saving transcript',
  complete: 'Finishing up',
};

const DIARIZATION_STAGES: Record<string, string> = {
  downloading_models: 'Preparing speaker models',
  decoding_audio: 'Analyzing audio',
  segmenting: 'Detecting speaker segments',
  clustering: 'Clustering speakers',
  diarizing: 'Diarizing speakers',
  saving_speakers: 'Saving speakers',
  speakers_complete: 'Finalizing',
  complete: 'Finishing up',
};

function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, Math.round(value)));
}

function labelFor(stage: string, labels: Record<string, string>): string {
  const known = labels[stage];
  if (known) return known;
  // An unlisted stage still has to read as a label rather than as a raw key.
  const words = stage.replace(/_/g, ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

function overallPercent(stage: string, raw: number): number {
  if (stage === 'complete' || stage === 'speakers_complete') return 100;
  return raw;
}

export function useTranscriptionProgress(
  meetingId?: string | null,
): TranscriptionProgress | null {
  const { snapshot } = useMeetingActivity();
  return useMemo(() => {
    if (!meetingId) return null;
    const activity = snapshot.activities
      .filter((candidate) => candidate.meeting_id === meetingId)
      .sort((left, right) => right.revision - left.revision)
      .find((candidate) => candidate.status === 'queued' || candidate.status === 'transcribing');
    if (!activity) return null;
    const stage = activity.stage;
    const indeterminate = activity.progress_percentage === null;
    const raw = indeterminate ? 0 : clampPercent(activity.progress_percentage as number);
    const isDiarization = !!stage && (stage.includes('speaker') || stage === 'diarizing');
    const stageLabel = stage
      ? labelFor(stage, isDiarization ? DIARIZATION_STAGES : RETRANSCRIPTION_STAGES)
      : activity.status === 'queued'
        ? 'Queued'
        : 'Preparing';
    return {
      taskId: activity.task_id,
      kind: activity.kind,
      percent: stage ? overallPercent(stage, raw) : raw,
      indeterminate,
      stageLabel,
      message: activity.message,
      cancellable: activity.controls_available,
    };
  }, [meetingId, snapshot.activities]);
}
