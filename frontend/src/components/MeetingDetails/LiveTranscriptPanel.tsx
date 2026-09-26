'use client';

import { useCallback, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { useTranscripts } from '@/contexts/TranscriptContext';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useConfig } from '@/contexts/ConfigContext';
import { Switch } from '@/components/ui/switch';
import {
  LIVE_TRANSCRIPTION_STORAGE_KEY,
  isLiveTranscriptionEnabled,
} from '@/lib/liveTranscription';

/**
 * Live transcript for an in-progress recording, rendered inside the meeting
 * workspace dock so recording looks the same as the rest of the app. Includes a
 * live-transcription toggle so it can be flipped during an active meeting.
 */
export function LiveTranscriptPanel() {
  const { transcripts } = useTranscripts();
  const { isRecording, isPaused, isProcessing, isStopping } = useRecordingState();
  const { betaFeatures } = useConfig();
  const [liveEnabled, setLiveEnabled] = useState(() =>
    typeof window === 'undefined'
      ? true
      : isLiveTranscriptionEnabled(
          localStorage.getItem(LIVE_TRANSCRIPTION_STORAGE_KEY),
          betaFeatures.liveTranscription,
        ),
  );

  const handleToggle = useCallback((enabled: boolean) => {
    setLiveEnabled(enabled);
    localStorage.setItem(LIVE_TRANSCRIPTION_STORAGE_KEY, String(enabled));
    invoke('set_live_transcription_enabled', { enabled }).catch((error) =>
      console.error('Failed to toggle live transcription:', error),
    );
  }, []);

  const toggleAvailable = betaFeatures.liveTranscription;

  const segments = useMemo(
    () =>
      transcripts.map((t) => ({
        id: t.id,
        timestamp: t.audio_start_time ?? 0,
        endTime: t.audio_end_time,
        text: t.text,
        confidence: t.confidence,
        speaker: t.speaker,
        speakerId: t.speaker_id,
      })),
    [transcripts]
  );

  return (
    <div className="flex h-full min-w-0 flex-col bg-[var(--surface-0)] text-ink">
      <div className="mx-auto w-full max-w-[900px] px-8 pb-2 pt-4">
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-1.5 text-[11px] font-bold uppercase tracking-[0.08em] text-recording">
            <span className={`h-1.5 w-1.5 rounded-full bg-recording ${isPaused ? '' : 'animate-pulse'}`} />
            {isPaused ? 'Paused' : 'Live'}
          </div>
          {toggleAvailable && (
            <label
              className="flex items-center gap-2 text-[11px] text-[var(--ink-muted)]"
              title="Live transcription is optional; audio is always saved"
            >
              <Switch checked={liveEnabled} onCheckedChange={handleToggle} />
              Live transcript
            </label>
          )}
        </div>
      </div>
      {toggleAvailable && !liveEnabled ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-2 px-8 pb-16 text-center">
          <p className="text-sm font-medium text-[var(--ink-muted)]">Live transcription is off</p>
          <p className="text-xs text-[var(--ink-subtle)]">
            Audio is still being recorded. Turn it on to transcribe as people speak, or leave it off
            and it will be transcribed after the meeting.
          </p>
        </div>
      ) : segments.length === 0 ? (
        <div className="flex flex-1 items-center justify-center px-8 pb-16 text-center text-sm text-[var(--ink-subtle)]">
          Listening… live transcript will appear here as people speak.
        </div>
      ) : (
        <div className="mx-auto w-full max-w-[900px] flex-1 overflow-hidden pb-4">
          <VirtualizedTranscriptView
            segments={segments}
            isRecording={isRecording}
            isPaused={isPaused}
            isProcessing={isProcessing}
            isStopping={isStopping}
            enableStreaming={isRecording}
            showConfidence
          />
        </div>
      )}
    </div>
  );
}
