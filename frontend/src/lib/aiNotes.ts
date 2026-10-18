import { useSyncExternalStore } from 'react';
import type { TimelineEntry } from '@/lib/liveTimeline';
import type { LiveNote, LiveNotesDocument } from '@/lib/liveNotes';
import { blockNoteBlocksToMarkdown, originalNotesMarkdown } from '@/lib/meetingExport';
import { aiNoteText, insertPlace, pendingGroups, type AiBlocks } from '@/lib/aiRawNotes';

/**
 * One notes document: the user's lines and the AI's lines (the live notes,
 * then Decisions and Action items when the meeting ends) in the same doc.
 * "Show AI notes" only changes what is shown, copied and exported; nothing
 * stored changes when it is flipped.
 */

// --- Show AI notes (a per-user preference, on by default) --------------------

export const SHOW_AI_NOTES_KEY = 'meetily:show-ai-notes';
const listeners = new Set<() => void>();
let cached: boolean | null = null;

export function readShowAiNotes(): boolean {
  if (cached !== null) return cached;
  let stored: string | null = null;
  try {
    stored = typeof localStorage === 'undefined' ? null : localStorage.getItem(SHOW_AI_NOTES_KEY);
  } catch {
    stored = null;
  }
  cached = stored !== 'false';
  return cached;
}

export function setShowAiNotes(show: boolean): void {
  cached = show;
  try {
    localStorage.setItem(SHOW_AI_NOTES_KEY, String(show));
  } catch {
    // Not remembered, but still applied for this session.
  }
  listeners.forEach((listener) => listener());
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Whether AI notes are shown, and a setter shared by every view. */
export function useShowAiNotes(): [boolean, (show: boolean) => void] {
  const show = useSyncExternalStore(subscribe, readShowAiNotes, () => true);
  return [show, setShowAiNotes];
}

/** For tests: forget the cached value so the next read uses storage. */
export function resetShowAiNotesCache(): void {
  cached = null;
}

// --- The document ---------------------------------------------------------------

/** Marks a document whose Decisions and Action items were added. */
export const WRAP_UP_ENTRY = 'wrap-up';

/** Whether the AI wrote into this document (a meeting from before one
 * notes document has none). */
export function hasAiNotes(document: LiveNotesDocument | null | undefined): boolean {
  if (!document) return false;
  return (document.aiEntries?.length ?? 0) > 0 || Object.keys(document.aiBlocks ?? {}).length > 0;
}

/** The notes as Markdown: every line, or only the user's when AI notes are
 * hidden. */
export function notesMarkdown(document: LiveNotesDocument | null, includeAi: boolean): string {
  if (!document) return '';
  if (!includeAi || !Array.isArray(document.editorBlocks)) return originalNotesMarkdown(document);
  return blockNoteBlocksToMarkdown(document.editorBlocks);
}

/** The old Enhanced notes, shown read-only below the notes of a meeting
 * whose document has no AI lines. Empty when hidden or not needed. */
export function legacyAiNotes(
  document: LiveNotesDocument | null | undefined,
  storedSummaryMarkdown: string,
  showAi: boolean,
): string {
  if (!showAi || hasAiNotes(document)) return '';
  return storedSummaryMarkdown.trim();
}

type StoredBlock = {
  id: string;
  type: string;
  props: Record<string, unknown>;
  content: { type: 'text'; text: string; styles: Record<string, never> }[];
  children: unknown[];
};

function block(id: string, type: 'heading' | 'bulletListItem', text: string): StoredBlock {
  return {
    id,
    type,
    props: type === 'heading' ? { level: 3 } : {},
    content: [{ type: 'text', text, styles: {} }],
    children: [],
  };
}

function blockId(value: unknown): string | null {
  const id = (value as { id?: unknown } | null)?.id;
  return typeof id === 'string' ? id : null;
}

function isEmptyStoredBlock(value: unknown): boolean {
  const stored = value as { content?: unknown; children?: unknown[] } | null;
  if (!stored || (Array.isArray(stored.children) && stored.children.length > 0)) return false;
  const content = stored.content;
  if (content === undefined || content === '') return true;
  if (typeof content === 'string') return !content.trim();
  if (!Array.isArray(content)) return false;
  return content.every((item) => {
    const text = typeof item === 'string' ? item : (item as { text?: unknown } | null)?.text;
    return typeof text === 'string' && !text.trim();
  });
}

function defaultId(): string {
  return typeof crypto !== 'undefined' && crypto.randomUUID
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

/** An action as the Action items list shows it: "Owner — task". */
export function actionItemText(entry: Pick<TimelineEntry, 'text' | 'speaker'>): string {
  const owner = entry.speaker?.trim();
  return owner ? `${owner} — ${entry.text}` : entry.text;
}

/**
 * When the meeting ends: the live notes the editor had not written yet,
 * then "Decisions" and "Action items" from the live notes' decisions and
 * actions, all as AI lines at the end of the notes (above trailing empty
 * lines of the user's). No model is involved.
 *
 * Only a document the AI already wrote into is completed, once (an old
 * meeting is never rewritten). Returns null when there is nothing to do.
 */
export function completeAiNotes(
  document: LiveNotesDocument,
  entries: TimelineEntry[],
  makeId: () => string = defaultId,
): LiveNotesDocument | null {
  if (!Array.isArray(document.editorBlocks) || !hasAiNotes(document)) return null;
  const inserted = new Set(document.aiEntries ?? []);
  if (inserted.has(WRAP_UP_ENTRY)) return null;

  const blocks: unknown[] = [...document.editorBlocks];
  const aiBlocks: AiBlocks = { ...(document.aiBlocks ?? {}) };
  const added: { id: string; ms: number; text: string }[] = [];

  const insertAt = (afterId: string, before: boolean, newBlocks: StoredBlock[]) => {
    const index = blocks.findIndex((candidate) => blockId(candidate) === afterId);
    blocks.splice(before ? index : index + 1, 0, ...newBlocks);
  };
  const placeAtEnd = (topic: string) => {
    const ids = blocks.map((candidate) => blockId(candidate) ?? '');
    const emptyIds = new Set(blocks.filter(isEmptyStoredBlock).map((candidate) => blockId(candidate) ?? ''));
    return insertPlace(ids, ids.length - 1, aiBlocks, topic, emptyIds);
  };

  const write = (topic: string, lines: { entryId: string; text: string; ms: number }[]) => {
    const place = placeAtEnd(topic);
    const newBlocks: StoredBlock[] = [];
    if (!place || place.withHeading) {
      const id = makeId();
      aiBlocks[id] = { entryId: '', text: topic, topic };
      newBlocks.push(block(id, 'heading', topic));
      added.push({ id, ms: lines[0]?.ms ?? 0, text: topic });
    }
    for (const line of lines) {
      const id = makeId();
      aiBlocks[id] = { entryId: line.entryId, text: line.text, topic };
      newBlocks.push(block(id, 'bulletListItem', line.text));
      added.push({ id, ms: line.ms, text: line.text });
    }
    if (place) insertAt(place.afterBlockId, Boolean(place.before), newBlocks);
    else blocks.push(...newBlocks);
  };

  for (const group of pendingGroups(entries, inserted)) {
    write(group.topic, group.entries.map((entry) => {
      inserted.add(entry.id);
      return { entryId: entry.id, text: aiNoteText(entry), ms: entry.start_ms };
    }));
  }

  const ordered = [...entries].sort((left, right) => left.start_ms - right.start_ms);
  const decisions = ordered.filter((entry) => entry.kind === 'decision');
  const actions = ordered.filter((entry) => entry.kind === 'action');
  if (decisions.length > 0) {
    write('Decisions', decisions.map((entry) => ({ entryId: entry.id, text: entry.text, ms: entry.start_ms })));
  }
  if (actions.length > 0) {
    write('Action items', actions.map((entry) => ({ entryId: entry.id, text: actionItemText(entry), ms: entry.start_ms })));
  }
  inserted.add(WRAP_UP_ENTRY);

  const newNotes: LiveNote[] = added.map((line) => ({
    id: line.id,
    timestampSeconds: line.ms / 1000,
    text: line.text,
    important: false,
  }));
  const notesById = new Map([...document.notes, ...newNotes].map((note) => [note.id, note]));
  const notes = blocks
    .map((candidate) => notesById.get(blockId(candidate) ?? ''))
    .filter((note): note is LiveNote => Boolean(note));

  return {
    ...document,
    version: 2,
    updatedAt: new Date().toISOString(),
    notes,
    editorBlocks: blocks,
    rawMarkdown: blockNoteBlocksToMarkdown(blocks),
    aiBlocks,
    aiEntries: [...inserted],
  };
}
