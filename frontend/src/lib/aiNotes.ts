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
/** Marks a wrap-up written from the live notes (no model), with how many
 * lines it wrote (`wrap-up:live:3`): the tidy pass's wrap-up replaces it
 * when it arrives, if all of them are still the AI's. */
export const LIVE_WRAP_UP_ENTRY = 'wrap-up:live';
/** Marks a wrap-up from the tidy pass (or a live one it could not replace). */
export const TIDY_WRAP_UP_ENTRY = 'wrap-up:tidy';

/** The end-of-meeting tidy pass's Summary, Decisions, Action items
 * ("Owner — task") and Open questions, stored with the meeting's summary
 * (`tidy_wrap_up`). */
export interface TidyWrapUp {
  summary: string;
  decisions: string[];
  action_items: string[];
  open_questions: string[];
}

const isStringList = (value: unknown): value is string[] =>
  Array.isArray(value) && value.every((item) => typeof item === 'string');

/** The tidy wrap-up stored with a summary result, if it has one. */
export function tidyWrapUpOf(summaryData: unknown): TidyWrapUp | null {
  const value = (summaryData as { tidy_wrap_up?: unknown } | null)?.tidy_wrap_up as Partial<TidyWrapUp> | undefined;
  if (!value || typeof value.summary !== 'string') return null;
  if (!isStringList(value.decisions) || !isStringList(value.action_items) || !isStringList(value.open_questions)) return null;
  return {
    summary: value.summary,
    decisions: value.decisions,
    action_items: value.action_items,
    open_questions: value.open_questions,
  };
}

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

type BlockType = 'heading' | 'bulletListItem' | 'paragraph';

function block(id: string, type: BlockType, text: string): StoredBlock {
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

/** One part of the wrap-up: a heading and its lines. */
interface WrapUpGroup {
  topic: string;
  type: 'bulletListItem' | 'paragraph';
  lines: { entryId: string; text: string; ms: number }[];
}

/** Decisions and Action items from the live notes' decisions and actions. */
function liveWrapUp(entries: TimelineEntry[]): WrapUpGroup[] {
  const ordered = [...entries].sort((left, right) => left.start_ms - right.start_ms);
  const line = (entry: TimelineEntry, text: string) => ({ entryId: entry.id, text, ms: entry.start_ms });
  return [
    {
      topic: 'Decisions',
      type: 'bulletListItem' as const,
      lines: ordered.filter((entry) => entry.kind === 'decision').map((entry) => line(entry, entry.text)),
    },
    {
      topic: 'Action items',
      type: 'bulletListItem' as const,
      lines: ordered.filter((entry) => entry.kind === 'action').map((entry) => line(entry, actionItemText(entry))),
    },
  ].filter((group) => group.lines.length > 0);
}

/** Summary, Decisions, Action items and Open questions from the tidy pass. */
function tidyWrapUp(tidy: TidyWrapUp, ms: number): WrapUpGroup[] {
  const lines = (texts: string[]) => texts.map((text) => text.trim()).filter(Boolean).map((text) => ({ entryId: '', text, ms }));
  return [
    { topic: 'Summary', type: 'paragraph' as const, lines: lines([tidy.summary]) },
    { topic: 'Decisions', type: 'bulletListItem' as const, lines: lines(tidy.decisions) },
    { topic: 'Action items', type: 'bulletListItem' as const, lines: lines(tidy.action_items) },
    { topic: 'Open questions', type: 'bulletListItem' as const, lines: lines(tidy.open_questions) },
  ].filter((group) => group.lines.length > 0);
}

/** Writes AI lines at the end of the notes (above trailing empty lines of
 * the user's) and returns the completed document. */
class AiWriter {
  readonly blocks: unknown[];
  readonly aiBlocks: AiBlocks;
  private readonly added: { id: string; ms: number; text: string }[] = [];

  constructor(private readonly document: LiveNotesDocument, private readonly makeId: () => string) {
    this.blocks = [...(document.editorBlocks as unknown[])];
    this.aiBlocks = { ...(document.aiBlocks ?? {}) };
  }

  /** Removes blocks (the AI's own) from the notes. */
  remove(ids: ReadonlySet<string>) {
    for (let index = this.blocks.length - 1; index >= 0; index -= 1) {
      const id = blockId(this.blocks[index]);
      if (id && ids.has(id)) {
        this.blocks.splice(index, 1);
        delete this.aiBlocks[id];
      }
    }
  }

  write(topic: string, lines: { entryId: string; text: string; ms: number }[], type: BlockType = 'bulletListItem', wrapUp = false) {
    const ids = this.blocks.map((candidate) => blockId(candidate) ?? '');
    const emptyIds = new Set(this.blocks.filter(isEmptyStoredBlock).map((candidate) => blockId(candidate) ?? ''));
    const place = insertPlace(ids, ids.length - 1, this.aiBlocks, topic, emptyIds);
    const mark = wrapUp ? { wrapUp: true as const } : {};
    const newBlocks: StoredBlock[] = [];
    if (!place || place.withHeading) {
      const id = this.makeId();
      this.aiBlocks[id] = { entryId: '', text: topic, topic, ...mark };
      newBlocks.push(block(id, 'heading', topic));
      this.added.push({ id, ms: lines[0]?.ms ?? 0, text: topic });
    }
    for (const line of lines) {
      const id = this.makeId();
      this.aiBlocks[id] = { entryId: line.entryId, text: line.text, topic, ...mark };
      newBlocks.push(block(id, type, line.text));
      this.added.push({ id, ms: line.ms, text: line.text });
    }
    if (place) {
      const index = this.blocks.findIndex((candidate) => blockId(candidate) === place.afterBlockId);
      this.blocks.splice(place.before ? index : index + 1, 0, ...newBlocks);
    } else {
      this.blocks.push(...newBlocks);
    }
  }

  /** The wrap-up's groups, each line marked; how many lines were written. */
  writeWrapUp(groups: WrapUpGroup[]): number {
    const before = this.added.length;
    for (const group of groups) this.write(group.topic, group.lines, group.type, true);
    return this.added.length - before;
  }

  done(aiEntries: Iterable<string>): LiveNotesDocument {
    const newNotes: LiveNote[] = this.added.map((line) => ({
      id: line.id,
      timestampSeconds: line.ms / 1000,
      text: line.text,
      important: false,
    }));
    const notesById = new Map([...this.document.notes, ...newNotes].map((note) => [note.id, note]));
    const notes = this.blocks
      .map((candidate) => notesById.get(blockId(candidate) ?? ''))
      .filter((note): note is LiveNote => Boolean(note));
    return {
      ...this.document,
      version: 2,
      updatedAt: new Date().toISOString(),
      notes,
      editorBlocks: this.blocks,
      rawMarkdown: blockNoteBlocksToMarkdown(this.blocks),
      aiBlocks: this.aiBlocks,
      aiEntries: [...aiEntries],
    };
  }
}

/**
 * When the meeting ends: the live notes the editor had not written yet,
 * then the wrap-up, all as AI lines at the end of the notes (above trailing
 * empty lines of the user's). The wrap-up is the tidy pass's Summary,
 * Decisions, Action items and Open questions when it is already stored
 * (`tidy`), else "Decisions" and "Action items" from the live notes'
 * decisions and actions, without a model; that one is replaced by the tidy
 * pass's when it arrives ({@link applyTidyWrapUp}).
 *
 * Only a document the AI already wrote into is completed, once (an old
 * meeting is never rewritten). Returns null when there is nothing to do.
 */
export function completeAiNotes(
  document: LiveNotesDocument,
  entries: TimelineEntry[],
  makeId: () => string = defaultId,
  tidy: TidyWrapUp | null = null,
): LiveNotesDocument | null {
  if (!Array.isArray(document.editorBlocks) || !hasAiNotes(document)) return null;
  const inserted = new Set(document.aiEntries ?? []);
  if (inserted.has(WRAP_UP_ENTRY)) return null;

  const writer = new AiWriter(document, makeId);
  for (const group of pendingGroups(entries, inserted)) {
    writer.write(group.topic, group.entries.map((entry) => {
      inserted.add(entry.id);
      return { entryId: entry.id, text: aiNoteText(entry), ms: entry.start_ms };
    }));
  }
  const endMs = Math.max(0, ...entries.map((entry) => entry.start_ms));
  const written = writer.writeWrapUp(tidy ? tidyWrapUp(tidy, endMs) : liveWrapUp(entries));
  inserted.add(WRAP_UP_ENTRY);
  inserted.add(tidy ? TIDY_WRAP_UP_ENTRY : `${LIVE_WRAP_UP_ENTRY}:${written}`);
  return writer.done(inserted);
}

/**
 * The tidy pass's wrap-up in place of the one written from the live notes,
 * when every line of that one is still the AI's (none edited or deleted);
 * otherwise the document is only marked, and keeps its wrap-up. Returns
 * null when there is nothing to do (no live wrap-up, or already replaced).
 */
export function applyTidyWrapUp(
  document: LiveNotesDocument,
  tidy: TidyWrapUp,
  makeId: () => string = defaultId,
): LiveNotesDocument | null {
  if (!Array.isArray(document.editorBlocks)) return null;
  const inserted = new Set(document.aiEntries ?? []);
  if (inserted.has(TIDY_WRAP_UP_ENTRY)) return null;
  const live = [...inserted].find((entry) => entry.startsWith(`${LIVE_WRAP_UP_ENTRY}:`));
  if (!live) return null;
  inserted.add(TIDY_WRAP_UP_ENTRY);

  const aiBlocks = document.aiBlocks ?? {};
  const wrapUpIds = new Set(
    (document.editorBlocks as unknown[])
      .map((candidate) => blockId(candidate) ?? '')
      .filter((id) => aiBlocks[id]?.wrapUp),
  );
  const writer = new AiWriter(document, makeId);
  if (wrapUpIds.size === Number(live.slice(LIVE_WRAP_UP_ENTRY.length + 1))) {
    const endSeconds = Math.max(0, ...document.notes.map((note) => note.timestampSeconds));
    writer.remove(wrapUpIds);
    writer.writeWrapUp(tidyWrapUp(tidy, endSeconds * 1000));
  }
  return writer.done(inserted);
}
