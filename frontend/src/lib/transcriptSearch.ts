import { invoke } from '@tauri-apps/api/core';

/** A transcript row of the open meeting that matches the in-meeting search. */
export interface TranscriptMatch {
  id: string;
  audio_start_time: number | null;
}

/**
 * Searches the meeting's whole transcript in the database, so matches in rows
 * the panel has not loaded yet are found too.
 */
export function searchMeetingTranscript(meetingId: string, query: string): Promise<TranscriptMatch[]> {
  return invoke<TranscriptMatch[]>('api_search_meeting_transcript', { meetingId, query });
}

/**
 * The ids of the matching rows, in transcript order. The database's answer
 * covers the whole transcript; until it arrives (or if it fails) the rows on
 * screen are searched instead.
 */
export function transcriptMatchIds(
  segments: ReadonlyArray<{ id: string; text: string }>,
  query: string,
  remote: readonly TranscriptMatch[] | null,
): string[] {
  const normalized = query.trim().toLowerCase();
  if (!normalized) return [];
  if (remote) return remote.map((match) => match.id);
  return segments.filter((segment) => segment.text.toLowerCase().includes(normalized)).map((segment) => segment.id);
}

/** Where to go for a match: its loaded row, or a load of the rest first. */
export function resolveMatchTarget(
  segments: ReadonlyArray<{ id: string }>,
  matchId: string,
  canLoadMore: boolean,
): { kind: 'jump'; index: number } | { kind: 'load' } | { kind: 'missing' } {
  const index = segments.findIndex((segment) => segment.id === matchId);
  if (index >= 0) return { kind: 'jump', index };
  return canLoadMore ? { kind: 'load' } : { kind: 'missing' };
}
