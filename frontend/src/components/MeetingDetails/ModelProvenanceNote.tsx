'use client';

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
 * A small, unobtrusive note reporting which models transcribed, diarized and
 * enhanced (summarized) a meeting. Renders nothing while none of that is known.
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
    parts.push(`Enhanced by ${label(summary.provider)} ${summary.model}`);
  }

  if (parts.length === 0) return null;

  return (
    <p className="mt-2 text-[11px] text-[var(--ink-subtle)]">
      {parts.join(' · ')}
    </p>
  );
}
