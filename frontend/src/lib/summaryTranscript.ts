import type { Transcript } from '@/types';

/**
 * The speaker label the summary prompts explain: `You` is the person recording
 * (the microphone), `Others` is everyone heard through system audio.
 */
export function summarySpeakerLabel(speaker: string | undefined): string | null {
  const trimmed = speaker?.trim();
  if (!trimmed) return null;
  if (trimmed === 'mic') return 'You';
  if (trimmed === 'system') return 'Others';
  return trimmed;
}

function formatAudioTime(seconds: number | undefined): string | null {
  if (seconds === undefined || !Number.isFinite(seconds) || seconds < 0) return null;
  const total = Math.floor(seconds);
  const minutes = Math.floor(total / 60).toString().padStart(2, '0');
  const secs = (total % 60).toString().padStart(2, '0');
  return `[${minutes}:${secs}]`;
}

/**
 * The transcript text sent for summarisation, one `[mm:ss] [Speaker] text`
 * line per segment. Both the manual and the automatic summary use this so the
 * model always sees the same format. A segment without a recording-relative
 * time gets no timestamp rather than its stored wall-clock string.
 */
export function formatSummaryTranscript(transcripts: Transcript[]): string {
  return transcripts
    .map((transcript) => {
      const text = transcript.text.trim();
      if (!text) return null;
      const time = formatAudioTime(transcript.audio_start_time);
      const speaker = summarySpeakerLabel(transcript.speaker);
      return [time, speaker && `[${speaker}]`, text].filter(Boolean).join(' ');
    })
    .filter((line): line is string => line !== null)
    .join('\n');
}
