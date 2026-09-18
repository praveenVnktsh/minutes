'use client';

import { useEffect, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

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

interface StageProgressEvent {
  meeting_id?: string | null;
  stage?: string | null;
  progress_percentage?: number | null;
  message?: string | null;
}

// Retranscription is split decode -> vad -> transcribe -> save and diarization
// is the short tail that runs once the transcript is written. Weighting the
// stages keeps one percentage moving forward instead of resetting when
// diarization restarts its own count at zero.
const RETRANSCRIPTION_SHARE = 85;
const DIARIZATION_SHARE = 100 - RETRANSCRIPTION_SHARE;

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

function overallPercent(stage: string, raw: number, isDiarization: boolean): number {
  if (stage === 'complete' || stage === 'speakers_complete') return 100;
  return isDiarization
    ? RETRANSCRIPTION_SHARE + Math.round((raw / 100) * DIARIZATION_SHARE)
    : Math.round((raw / 100) * RETRANSCRIPTION_SHARE);
}

export function useTranscriptionProgress(
  meetingId?: string | null,
): TranscriptionProgress | null {
  const [progress, setProgress] = useState<TranscriptionProgress | null>(null);

  useEffect(() => {
    setProgress(null);
    if (!meetingId) return;

    let disposed = false;
    const unlisteners: UnlistenFn[] = [];

    const apply = (event: StageProgressEvent, isDiarization: boolean) => {
      // Every emitter tags its events with the meeting. Events without a tag
      // are accepted because they can only come from a single active task.
      if (event.meeting_id && event.meeting_id !== meetingId) return;
      const stage = event.stage;
      if (!stage) return;
      const raw = clampPercent(event.progress_percentage ?? 0);
      setProgress({
        percent: overallPercent(stage, raw, isDiarization),
        stageLabel: labelFor(stage, isDiarization ? DIARIZATION_STAGES : RETRANSCRIPTION_STAGES),
        message: event.message ?? null,
      });
    };

    const subscribe = async () => {
      const retranscription = await listen<StageProgressEvent>(
        'retranscription-progress',
        (event) => apply(event.payload, false),
      );
      if (disposed) {
        retranscription();
        return;
      }
      unlisteners.push(retranscription);

      const diarization = await listen<StageProgressEvent>(
        'diarization-progress',
        (event) => apply(event.payload, true),
      );
      if (disposed) {
        unlisteners.forEach((unlisten) => unlisten());
        return;
      }
      unlisteners.push(diarization);
    };

    void subscribe();

    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [meetingId]);

  return progress;
}
