import type { TimelineEntry } from '@/lib/liveTimeline';

/**
 * Live notes written into the notes document (one doc: the user's lines and
 * the AI's). AI lines sit under their topic and look different. The
 * rules, kept here so they can be tested without an editor:
 *
 * - nothing is added until the user has paused typing for `AI_NOTES_PAUSE_MS`;
 * - lines are only ever added below the cursor: under their topic's last AI
 *   line when that is below the cursor, else at the end of the notes (with
 *   the topic's heading when the notes do not already end in that topic);
 * - user lines are never edited; editing an AI line makes it the user's;
 * - a live note is added once, even if the user deletes its line.
 */

export const AI_NOTES_PAUSE_MS = 2000;

/** A block the AI wrote, by block id. */
export interface AiBlockInfo {
  entryId: string;
  /** The text as written: a block whose text changed is the user's. */
  text: string;
  topic: string;
  /** A line of the meeting-end wrap-up. */
  wrapUp?: true;
}

export type AiBlocks = Record<string, AiBlockInfo>;

const FALLBACK_TOPIC = 'Notes';

export function entryTopic(entry: Pick<TimelineEntry, 'topic'>): string {
  return entry.topic?.trim() || FALLBACK_TOPIC;
}

/** The line an entry becomes in the notes. */
export function aiNoteText(entry: Pick<TimelineEntry, 'kind' | 'text' | 'speaker'>): string {
  switch (entry.kind) {
    case 'decision':
      return `Decision: ${entry.text}`;
    case 'action':
      return `Action: ${entry.speaker?.trim() || 'Others'} — ${entry.text}`;
    default:
      return entry.text;
  }
}

/** Entries not written into the notes yet, oldest first, grouped by topic
 * in the order the topics come. */
export function pendingGroups(
  entries: TimelineEntry[],
  inserted: ReadonlySet<string>,
): { topic: string; entries: TimelineEntry[] }[] {
  const groups: { topic: string; entries: TimelineEntry[] }[] = [];
  const pending = entries
    .filter((entry) => !inserted.has(entry.id))
    .sort((left, right) => left.start_ms - right.start_ms);
  for (const entry of pending) {
    const topic = entryTopic(entry);
    const group = groups.find((candidate) => candidate.topic === topic);
    if (group) group.entries.push(entry);
    else groups.push({ topic, entries: [entry] });
  }
  return groups;
}

/** Where a topic's new lines go: after `afterBlockId`, with the topic's
 * heading first when `withHeading`. */
export interface InsertPlace {
  afterBlockId: string;
  withHeading: boolean;
  /** Insert before `afterBlockId` instead (it is an empty line of the
   * user's that stays below the AI lines, where they type next). */
  before?: boolean;
}

/**
 * The place for a topic's new lines in the top-level blocks `blockIds`,
 * the cursor being in block `cursorIndex` (-1: none). Never above the cursor.
 */
export function insertPlace(
  blockIds: string[],
  cursorIndex: number,
  aiBlocks: AiBlocks,
  topic: string,
  emptyIds: ReadonlySet<string> = new Set(),
): InsertPlace | null {
  if (blockIds.length === 0) return null;
  // Empty lines of the user's at the end (the starting bullet, or one just
  // opened) stay below the AI lines: place among the blocks before them.
  let end = blockIds.length;
  while (end > 0 && emptyIds.has(blockIds[end - 1]) && !aiBlocks[blockIds[end - 1]]) end -= 1;
  if (end < blockIds.length) {
    if (end === 0) return { afterBlockId: blockIds[0], withHeading: true, before: true };
    const place = insertPlace(blockIds.slice(0, end), Math.min(cursorIndex, end - 1), aiBlocks, topic);
    return place;
  }
  let anchor = -1;
  blockIds.forEach((id, index) => {
    if (aiBlocks[id]?.topic === topic) anchor = index;
  });
  if (anchor >= 0 && anchor >= cursorIndex) {
    return { afterBlockId: blockIds[anchor], withHeading: false };
  }
  const last = blockIds.length - 1;
  const lastInfo = aiBlocks[blockIds[last]];
  return { afterBlockId: blockIds[last], withHeading: !(lastInfo && lastInfo.topic === topic) };
}

/**
 * The AI blocks still the AI's: a block the user deleted or edited is
 * dropped (an edited line is now the user's).
 */
export function remainingAiBlocks(aiBlocks: AiBlocks, textById: ReadonlyMap<string, string>): AiBlocks {
  const next: AiBlocks = {};
  for (const [id, info] of Object.entries(aiBlocks)) {
    if (textById.get(id) === info.text) next[id] = info;
  }
  return next;
}

/** CSS that marks AI lines: muted, with a small "AI" tag, or hidden when
 * "Show AI notes" is off (hidden, never deleted). Colours are theme
 * variables, so the marks follow light and dark mode. */
export function aiBlocksCss(aiBlocks: AiBlocks, visible = true): string {
  const ids = Object.keys(aiBlocks).filter((id) => /^[\w-]+$/.test(id));
  if (ids.length === 0) return '';
  if (!visible) {
    return `${ids.map((id) => `.bn-block-outer[data-id="${id}"]`).join(',\n')} { display: none; }`;
  }
  const content = ids.map((id) => `.bn-block[data-id="${id}"] > .bn-block-content`).join(',\n');
  const tag = ids.map((id) => `.bn-block[data-id="${id}"] > .bn-block-content::after`).join(',\n');
  return `${content} { color: var(--ink-muted); }\n${tag} { content: "AI"; margin-left: 6px; font-size: 10px; line-height: 1; color: var(--ink-subtle); align-self: center; }`;
}
