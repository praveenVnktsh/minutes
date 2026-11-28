import type { MeetingSummary, Transcript } from '@/types';
import type { LiveNotesDocument } from '@/lib/liveNotes';
import { formatNoteTimestamp } from '@/lib/liveNotes';
import { citationHrefSeconds } from '@/lib/copilot';
import { bracketedClock } from '@/lib/format';

export interface TranscriptPage {
  transcripts: Transcript[];
  total_count: number;
  has_more: boolean;
}

export type TranscriptPageReader = (args: {
  meetingId: string;
  limit: number;
  offset: number;
}) => Promise<TranscriptPage>;

const TRANSCRIPT_PAGE_SIZE = 250;
const MAX_TRANSCRIPT_PAGES = 10_000;

export async function fetchCompleteTranscripts(
  meetingId: string,
  readPage: TranscriptPageReader,
): Promise<Transcript[]> {
  const transcripts: Transcript[] = [];
  const seenIds = new Set<string>();
  let expectedTotal: number | undefined;
  let offset = 0;

  for (let pageNumber = 0; pageNumber < MAX_TRANSCRIPT_PAGES; pageNumber += 1) {
    const page = await readPage({ meetingId, limit: TRANSCRIPT_PAGE_SIZE, offset });
    if (!Array.isArray(page.transcripts) || !Number.isFinite(page.total_count) || page.total_count < 0) {
      throw new Error('Transcript service returned an invalid page');
    }
    if (expectedTotal === undefined) expectedTotal = page.total_count;
    if (page.total_count !== expectedTotal) {
      throw new Error('Transcript total changed during pagination');
    }

    if (page.transcripts.length === 0) {
      if (page.has_more || offset < expectedTotal) {
        throw new Error('Transcript pagination stopped before all segments were read');
      }
      return transcripts;
    }

    for (const transcript of page.transcripts) {
      if (seenIds.has(transcript.id)) throw new Error('Transcript pages overlap');
      seenIds.add(transcript.id);
      transcripts.push(transcript);
    }

    offset += page.transcripts.length;
    if (offset > expectedTotal) throw new Error('Transcript pagination exceeded the reported total');
    if (page.has_more) {
      if (offset >= expectedTotal) throw new Error('Transcript pagination has an inconsistent continuation');
      continue;
    }
    if (offset !== expectedTotal || transcripts.length !== expectedTotal) {
      throw new Error('Transcript pagination stopped before all segments were read');
    }
    return transcripts;
  }

  throw new Error('Transcript pagination exceeded its safety limit');
}

type JsonObject = Record<string, unknown>;

function objectValue(value: unknown): JsonObject | null {
  return value !== null && typeof value === 'object' ? value as JsonObject : null;
}

function inlineMarkdown(content: unknown): string {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';

  return content.map((item) => {
    if (typeof item === 'string') return item;
    const inline = objectValue(item);
    if (!inline) return '';

    if (inline.type === 'link') {
      const label = inlineMarkdown(inline.content);
      const href = typeof inline.href === 'string' ? inline.href : '';
      // A `[MM:SS]` transcript link only works inside the app: export its text.
      if (citationHrefSeconds(href) !== null) return label;
      return href ? `[${label}](${href})` : label;
    }

    let text = typeof inline.text === 'string' ? inline.text : inlineMarkdown(inline.content);
    const styles = objectValue(inline.styles);
    if (styles?.code) text = `\`${text}\``;
    if (styles?.bold) text = `**${text}**`;
    if (styles?.italic) text = `*${text}*`;
    if (styles?.strike) text = `~~${text}~~`;
    if (styles?.underline) text = `<u>${text}</u>`;
    return text;
  }).join('');
}

function tableMarkdown(content: unknown): string {
  const table = objectValue(content);
  if (!table || !Array.isArray(table.rows) || table.rows.length === 0) return '';
  const rows = table.rows.map((row) => {
    const cells = objectValue(row)?.cells;
    if (!Array.isArray(cells)) return [];
    return cells.flatMap((cell) => {
      const tableCell = objectValue(cell);
      const cellContent = tableCell?.type === 'tableCell' ? tableCell.content : cell;
      const markdown = inlineMarkdown(cellContent).replace(/\|/g, '\\|').replace(/\n/g, '<br>');
      const colspan = tableCell?.type === 'tableCell'
        ? Math.max(1, Number(objectValue(tableCell.props)?.colspan) || 1)
        : 1;
      return [markdown, ...Array.from({ length: colspan - 1 }, () => '')];
    });
  });
  const width = Math.max(...rows.map((row) => row.length));
  if (width === 0 || rows.every((row) => row.every((cell) => !cell.trim()))) return '';
  const line = (cells: string[]) => `| ${Array.from({ length: width }, (_, index) => cells[index] ?? '').join(' | ')} |`;
  return [line(rows[0]), line(Array.from({ length: width }, () => '---')), ...rows.slice(1).map(line)].join('\n');
}

function blockMarkdown(blockValue: unknown, depth = 0): string {
  const block = objectValue(blockValue);
  if (!block) return '';
  const type = typeof block.type === 'string' ? block.type : 'paragraph';
  const props = objectValue(block.props);
  const content = type === 'table' ? tableMarkdown(block.content) : inlineMarkdown(block.content);
  const indent = '  '.repeat(depth);
  let line = '';

  switch (type) {
    case 'heading': {
      const level = Math.min(6, Math.max(1, Number(props?.level) || 2));
      if (content.trim()) line = `${'#'.repeat(level)} ${content}`;
      break;
    }
    case 'bulletListItem':
      if (content.trim()) line = `${indent}- ${content}`;
      break;
    case 'numberedListItem':
      if (content.trim()) line = `${indent}1. ${content}`;
      break;
    case 'checkListItem':
      if (content.trim()) line = `${indent}- [${props?.checked ? 'x' : ' '}] ${content}`;
      break;
    case 'codeBlock': {
      const language = typeof props?.language === 'string' ? props.language : '';
      if (content.trim()) line = `\`\`\`${language}\n${content}\n\`\`\``;
      break;
    }
    case 'quote':
      if (content.trim()) line = content.split('\n').map((part) => `> ${part}`).join('\n');
      break;
    case 'image':
    case 'audio':
    case 'video':
    case 'file':
      line = fileBlockMarkdown(type, props);
      break;
    default:
      line = content;
  }

  const children = Array.isArray(block.children)
    ? block.children.map((child) => blockMarkdown(child, depth + 1)).filter(Boolean).join('\n')
    : '';
  return [line, children].filter(Boolean).join('\n');
}

function fileBlockMarkdown(type: string, props: JsonObject | null): string {
  const url = typeof props?.url === 'string' ? props.url.trim() : '';
  const name = typeof props?.name === 'string' ? props.name.trim() : '';
  const caption = typeof props?.caption === 'string' ? props.caption.trim() : '';
  const defaultLabel = type.charAt(0).toUpperCase() + type.slice(1);
  const label = caption || name || defaultLabel;
  if (!url) return name || caption ? `${defaultLabel}: ${label}` : '';
  return type === 'image' ? `![${label}](${url})` : `[${label}](${url})`;
}

export function blockNoteBlocksToMarkdown(blocks: unknown[]): string {
  return blocks.map((block) => blockMarkdown(block)).filter((block) => block.trim()).join('\n\n').trim();
}

export function originalNotesMarkdown(document: LiveNotesDocument | null): string {
  if (!document) return '';
  if (Array.isArray(document.editorBlocks)) {
    // The user's own notes: lines the AI wrote into them are left out.
    const aiBlocks = document.aiBlocks ?? {};
    const own = document.editorBlocks.filter((block) => {
      const id = (block as { id?: unknown } | null)?.id;
      return typeof id !== 'string' || !(id in aiBlocks);
    });
    return blockNoteBlocksToMarkdown(own);
  }
  if (document.rawMarkdown !== undefined) return document.rawMarkdown.trim();

  return document.notes
    .filter((note) => note.text.trim())
    .map((note) => `- [${formatNoteTimestamp(note.timestampSeconds)}]${note.important ? ' **Important**' : ''} ${note.text}`)
    .join('\n');
}

export function storedSummaryMarkdown(summary: MeetingSummary | null): string {
  if (!summary) return '';
  if ('summary_json' in summary && Array.isArray(summary.summary_json)) {
    return blockNoteBlocksToMarkdown(summary.summary_json);
  }
  if ('markdown' in summary && typeof summary.markdown === 'string') return summary.markdown.trim();

  return Object.entries(summary)
    .filter(([key]) => !['markdown', 'summary_json', '_section_order', 'MeetingName'].includes(key))
    .map(([, section]) => {
      const value = objectValue(section);
      if (!value || typeof value.title !== 'string' || !Array.isArray(value.blocks)) return '';
      const body = value.blocks.map((block) => {
        const content = objectValue(block)?.content;
        return typeof content === 'string' ? `- ${content}` : '';
      }).filter(Boolean).join('\n');
      return body ? `## ${value.title}\n\n${body}` : '';
    })
    .filter(Boolean)
    .join('\n\n');
}

export function transcriptMarkdown(transcripts: Transcript[]): string {
  return transcripts.map((segment) => {
    const timestamp = formatTranscriptTimestamp(segment.audio_start_time, segment.timestamp);
    const speaker = segment.speaker
      ? ` **${segment.speaker === 'mic' ? 'You' : segment.speaker === 'system' ? 'Others' : segment.speaker}**`
      : '';
    return `- ${timestamp}${speaker} ${segment.text}`;
  }).join('\n');
}

function formatTranscriptTimestamp(seconds: number | undefined, fallback: string): string {
  if (seconds === undefined) return fallback.startsWith('[') ? fallback : `[${fallback}]`;
  return bracketedClock(seconds, undefined, { padHours: true });
}

export interface FrontmatterMeta {
  title: string;
  meetingId: string;
  /** When the meeting started; omitted from the frontmatter when unknown. */
  createdAt?: Date;
  durationSeconds?: number;
  attendees?: string[];
  tags?: string[];
  exportedAt: Date;
}

const pad = (value: number) => String(value).padStart(2, '0');

/** `YYYY-MM-DD` in the local time zone, as Obsidian's daily notes name days. */
export function localDate(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** ISO 8601 in local time with its offset, e.g. `2026-11-27T09:30:00+01:00`. */
export function localIsoTimestamp(date: Date): string {
  const offset = -date.getTimezoneOffset();
  const sign = offset >= 0 ? '+' : '-';
  const abs = Math.abs(offset);
  return `${localDate(date)}T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
    + `${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
}

/** A JSON string is a valid YAML double-quoted scalar, so `:`, `#`, quotes
 * and newlines in a title can't break the frontmatter. */
const yamlString = (value: string) => JSON.stringify(value);

function yamlList(key: string, values: string[]): string[] {
  return [`${key}:`, ...values.map((value) => `  - ${yamlString(value)}`)];
}

/** Tags as Obsidian accepts them: no leading `#`, spaces as `-`, only
 * letters, digits, `_`, `/` and `-`, not purely numeric, each once. Matches
 * `normalise_tags` in `obsidian.rs`. */
export function normaliseTags(input: string[]): string[] {
  const seen = new Set<string>();
  const tags: string[] = [];
  for (const raw of input) {
    const tag = raw.trim().replace(/^#+/, '').split(/\s+/).filter(Boolean).join('-')
      .replace(/[^\p{L}\p{N}_/-]/gu, '')
      .replace(/^\/+|\/+$/g, '');
    if (!tag || /^[0-9]+$/.test(tag)) continue;
    const key = tag.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    tags.push(tag);
  }
  return tags;
}

/** A YAML frontmatter block for Obsidian's Properties, Dataview and Bases. */
export function buildFrontmatter(meta: FrontmatterMeta): string {
  const lines = ['---', `title: ${yamlString(meta.title || 'Untitled meeting')}`];
  if (meta.createdAt && !Number.isNaN(meta.createdAt.getTime())) {
    lines.push(`date: ${localDate(meta.createdAt)}`, `created: ${yamlString(localIsoTimestamp(meta.createdAt))}`);
  }
  if (meta.durationSeconds && meta.durationSeconds > 0) {
    lines.push(`duration_minutes: ${Math.max(1, Math.round(meta.durationSeconds / 60))}`);
  }
  const attendees = (meta.attendees ?? []).map((name) => name.trim()).filter(Boolean);
  if (attendees.length) lines.push(...yamlList('attendees', attendees));
  const tags = normaliseTags(meta.tags ?? []);
  if (tags.length) lines.push(...yamlList('tags', tags));
  lines.push(
    `minutes_id: ${yamlString(meta.meetingId)}`,
    `minutes_link: ${yamlString(`minutes://meeting/${meta.meetingId}`)}`,
    `exported_at: ${yamlString(localIsoTimestamp(meta.exportedAt))}`,
    '---',
  );
  return `${lines.join('\n')}\n\n`;
}

/** A diarised voice, as `get_speaker_identities` returns it. Its display name
 * is its id (`speaker_00`, `mic`) until diarisation or the user names it. */
export interface SpeakerName {
  speaker_id: string;
  display_name: string;
}

/** Who was in the meeting: its linked attendees, then each speaker the user
 * (or diarisation) named who isn't one of them already. */
export function meetingPeopleNames(attendees: string[], speakers: SpeakerName[]): string[] {
  const seen = new Set<string>();
  const names: string[] = [];
  const add = (raw: string) => {
    const name = raw.trim();
    const key = name.toLocaleLowerCase();
    if (!name || seen.has(key)) return;
    seen.add(key);
    names.push(name);
  };
  attendees.forEach(add);
  speakers
    .filter((speaker) => speaker.display_name.trim() && speaker.display_name.trim() !== speaker.speaker_id)
    .forEach((speaker) => add(speaker.display_name));
  return names;
}

/** The recording's length: where its last segment ends. */
export function transcriptDurationSeconds(transcripts: Transcript[]): number | undefined {
  const end = transcripts.reduce((max, segment) => Math.max(max, segment.audio_end_time ?? 0), 0);
  return end > 0 ? end : undefined;
}

export function buildMeetingExport({
  title,
  createdAt,
  notes,
  aiNotes = '',
  transcripts,
  frontmatter,
  includeTranscript = true,
}: {
  title: string;
  createdAt?: string;
  /** The notes document as shown: AI lines only when "Show AI notes" is on. */
  notes: string;
  /** An old meeting's Enhanced notes, when they are shown below its notes. */
  aiNotes?: string;
  transcripts: Transcript[];
  /** A `buildFrontmatter` block; it carries the date, so the date line goes. */
  frontmatter?: string;
  includeTranscript?: boolean;
}): string {
  const lines = [`# ${title || 'Untitled meeting'}`, ''];
  if (createdAt && !frontmatter) lines.push(`_${new Date(createdAt).toLocaleString()}_`, '');
  if (notes.trim()) lines.push('## Notes', '', notes.trim(), '');
  if (aiNotes.trim()) lines.push('## AI notes', '', aiNotes.trim(), '');
  if (includeTranscript && transcripts.length) lines.push('## Transcript', '', transcriptMarkdown(transcripts), '');
  return (frontmatter ?? '') + lines.join('\n').trimEnd() + '\n';
}

/** `<Title>-YYYY-MM-DD.md`, dated in local time. */
export function exportFileName(title: string, date = new Date()): string {
  const safeTitle = (title || 'meeting').replace(/[^\w\- ]+/g, '').trim().replace(/\s+/g, '-') || 'meeting';
  return `${safeTitle}-${localDate(date)}.md`;
}
