'use client';

import { Info } from 'lucide-react';
import { useModelProvenance } from '@/hooks/useModelProvenance';

/** Maps a provider/engine id to a human-friendly label; unknown ids pass through unchanged. */
const PROVIDER_LABELS: Record<string, string> = {
  whisper: 'Whisper',
  parakeet: 'Parakeet',
  ollama: 'Ollama',
  claude: 'Claude',
  groq: 'Groq',
  openrouter: 'OpenRouter',
  openai: 'OpenAI',
  'builtin-ai': 'Built-in AI',
};

function label(id: string): string {
  return PROVIDER_LABELS[id] ?? id;
}

interface ModelProvenanceNoteProps {
  meetingId: string;
}

/**
 * A small info icon whose tooltip reports which models transcribed, diarized
 * and wrote AI notes for a meeting. Renders nothing while none of that is known.
 */
export function ModelProvenanceNote({ meetingId }: ModelProvenanceNoteProps) {
  const provenance = useModelProvenance(meetingId);
  if (!provenance) return null;

  const { transcription, diarization, summary } = provenance;
  const parts: string[] = [];

  if (transcription) {
    parts.push(`Transcribed by ${label(transcription.provider)} ${transcription.model}`);
  }
  if (diarization) {
    const detail = diarization.segmentation_model ? ` (${diarization.segmentation_model})` : '';
    parts.push(`Speakers by ${label(diarization.engine)}${detail}`);
  }
  if (summary) {
    parts.push(`AI notes by ${label(summary.provider)} ${summary.model}`);
  }

  if (parts.length === 0) return null;

  const text = parts.join(' · ');
  return (
    <span
      role="img"
      tabIndex={0}
      title={text}
      aria-label={`Models: ${text}`}
      className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-[var(--ink-subtle)] hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus"
    >
      <Info className="h-4 w-4" />
    </span>
  );
}
