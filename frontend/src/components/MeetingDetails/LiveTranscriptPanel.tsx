'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { useTranscripts } from '@/contexts/TranscriptContext';
import { useRecordingState } from '@/contexts/RecordingStateContext';
import { useConfig } from '@/contexts/ConfigContext';
import { Switch } from '@/components/ui/switch';
import {
  LIVE_TRANSCRIPTION_STORAGE_KEY,
  isLiveTranscriptionEnabled,
} from '@/lib/liveTranscription';
import {
  TRANSCRIPT_PARTIAL_EVENT,
  applyPartial,
  orderedPartials,
  type LivePartials,
  type TranscriptPartialEvent,
} from '@/lib/liveTranscriptionModels';

function sourceLabel(source: string): string {
  return source === 'system' ? 'Others' : 'You';
}

/** The streaming model's in-progress utterances, per source. Display only:
 * finished text arrives as regular transcript segments. */
function useLivePartials(active: boolean): LivePartials {
  const [partials, setPartials] = useState<LivePartials>({});
  useEffect(() => {
    if (!active) {
      setPartials({});
      return undefined;
    }
    const unlisten = listen<TranscriptPartialEvent>(TRANSCRIPT_PARTIAL_EVENT, (event) => {
      setPartials((current) => applyPartial(current, event.payload));
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [active]);
  return partials;
}

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
  const partials = orderedPartials(useLivePartials(isRecording || isStopping));
  const partialLines = partials.length > 0 && (
    <div aria-live="polite" className="mx-auto w-full max-w-[900px] space-y-1 px-8 pb-4">
      {partials.map((partial) => (
        <p key={partial.source} className="text-sm italic text-[var(--ink-muted)]">
          <span className="mr-2 text-[11px] font-semibold not-italic uppercase tracking-[0.06em] text-[var(--ink-subtle)]">
            {sourceLabel(partial.source)}
          </span>
          {partial.text}
        </p>
      ))}
    </div>
  );

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
        partialLines ? (
          <div className="flex flex-1 flex-col justify-end">{partialLines}</div>
        ) : (
          <div className="flex flex-1 items-center justify-center px-8 pb-16 text-center text-sm text-[var(--ink-subtle)]">
            Listening… live transcript will appear here as people speak.
          </div>
        )
      ) : (
        <>
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
        {partialLines}
        </>
      )}
    </div>
  );
}
