export interface LiveNote {
  id: string;
  timestampSeconds: number;
  text: string;
  important: boolean;
}

export interface LiveNotesDocument {
  version: number;
  meetingStartedAtMs: number;
  updatedAt: string;
  notes: LiveNote[];
  rawMarkdown?: string;
  editorBlocks?: unknown[];
}

export const LIVE_NOTES_FALLBACK_KEY = 'meetily.liveNotes.current';
export const LIVE_NOTES_FALLBACK_FOLDER_KEY = 'meetily.liveNotes.currentFolder';

export function createEmptyLiveNotesDocument(meetingStartedAtMs = Date.now()): LiveNotesDocument {
  return {
    version: 1,
    meetingStartedAtMs,
    updatedAt: new Date().toISOString(),
    notes: [createLiveNote(0)],
  };
}

export function formatNoteTimestamp(seconds: number): string {
  const wholeSeconds = Math.max(0, Math.floor(seconds));
  return `${Math.floor(wholeSeconds / 60).toString().padStart(2, '0')}:${(wholeSeconds % 60).toString().padStart(2, '0')}`;
}

export function createLiveNote(timestampSeconds: number): LiveNote {
  const randomId = typeof crypto !== 'undefined' && crypto.randomUUID
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
  return { id: randomId, timestampSeconds, text: '', important: false };
}

/**
 * The user's own notes, in markdown, passed to the summarizer as the anchor
 * that the enhanced bullet notes build on. The backend wraps this in
 * `<my_notes>` and merges it with the transcript.
 */
export function buildLiveNotesSummaryContext(document: LiveNotesDocument | null): string {
  if (!document) return '';
  const rawMarkdown = document.rawMarkdown?.trim();
  if (!rawMarkdown && !document.notes.some((note) => note.text.length > 0)) return '';
  const notes = document.notes
    .filter((note) => note.text.length > 0)
    .map((note) => `[${formatNoteTimestamp(note.timestampSeconds)}]${note.important ? ' IMPORTANT' : ''}\n${note.text}`)
    .join('\n\n');
  return rawMarkdown || notes;
}
