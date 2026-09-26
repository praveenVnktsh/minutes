'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

/** Which model transcribed a meeting's audio. Mirrors the Rust `TranscriptionModel`. */
export interface TranscriptionModel {
  provider: string;
  model: string;
}

/** Which models diarized (speaker-labeled) a meeting. Mirrors the Rust `DiarizationModel`. */
export interface DiarizationModel {
  engine: string;
  segmentation_model: string | null;
  embedding_model: string | null;
}

/** Which model summarized a meeting. Mirrors the Rust `SummaryModel`. */
export interface SummaryModel {
  provider: string;
  model: string;
}

/**
 * Which models touched a meeting, as reported by `get_meeting_model_provenance`.
 * Mirrors the Rust `MeetingModelProvenance`. Any leg may be `null` when that
 * stage hasn't run yet (or ran before provenance was recorded).
 */
export interface MeetingModelProvenance {
  transcription: TranscriptionModel | null;
  diarization: DiarizationModel | null;
  summary: SummaryModel | null;
}

/** Events that can change which models touched a meeting, each meeting-scoped by `meeting_id`. */
const PROVENANCE_EVENTS = [
  'diarization-status-changed',
  'diarization-complete',
  'retranscription-complete',
  'import-complete',
] as const;

/**
 * Tracks a meeting's model provenance (transcription, diarization and
 * summary models) and refreshes it whenever the Rust core emits an event
 * that could change which models touched that meeting.
 */
export function useModelProvenance(meetingId: string | undefined): MeetingModelProvenance | null {
  const [provenance, setProvenance] = useState<MeetingModelProvenance | null>(null);

  const refresh = useCallback(async () => {
    if (!meetingId) {
      setProvenance(null);
      return;
    }
    try {
      setProvenance(await invoke<MeetingModelProvenance>('get_meeting_model_provenance', { meetingId }));
    } catch (error) {
      console.warn('Could not load model provenance:', error);
      setProvenance(null);
    }
  }, [meetingId]);

  useEffect(() => {
    setProvenance(null);
    void refresh();
    if (!meetingId) return;
    let disposed = false;
    const unlistens: Array<() => void> = [];

    for (const event of PROVENANCE_EVENTS) {
      void listen<{ meeting_id?: string }>(event, ({ payload }) => {
        if (payload.meeting_id === undefined || payload.meeting_id === meetingId) void refresh();
      }).then((fn) => {
        if (disposed) fn();
        else unlistens.push(fn);
      }).catch((error) => console.warn(`Could not listen for ${event}:`, error));
    }

    return () => {
      disposed = true;
      unlistens.forEach((fn) => fn());
    };
  }, [meetingId, refresh]);

  return provenance;
}
