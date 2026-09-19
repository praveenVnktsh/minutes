'use client';

import { useMemo } from 'react';
import { useMeetingActivity } from '@/contexts/MeetingActivityContext';

/**
 * Combined transcription progress for a single meeting.
 *
 * Transcription runs as several stages. The batch pass reports its own stages
 * ("decoding", "vad", "transcribing", "saving") on `retranscription-progress`,
 * and speaker diarization reports the stages that follow on
 * `diarization-progress`. Both events carry a `meeting_id`, so a meeting's
 * workspace can show one continuous percentage across every stage.
 */
export interface TranscriptionProgress {
  /** Overall completion across every phase, 0-100. */
  percent: number;
  /** Short human label for the current phase, e.g. "Transcribing". */
  stageLabel: string;
  /** The backend's own description of the phase, when it sends one. */
  message: string | null;
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
  return labels[stage] ?? stage.replace(/_/g, ' ');
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
    if (!activity?.stage || activity.progress_percentage === null) return null;
    const isDiarization = activity.stage.includes('speaker') || activity.stage === 'diarizing';
    const raw = clampPercent(activity.progress_percentage);
    return {
      percent: overallPercent(activity.stage, raw),
      stageLabel: labelFor(activity.stage, isDiarization ? DIARIZATION_STAGES : RETRANSCRIPTION_STAGES),
      message: activity.message,
    };
  }, [meetingId, snapshot.activities]);
}
