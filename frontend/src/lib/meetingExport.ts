import type { MeetingSummary, Transcript } from '@/types';
import type { LiveNotesDocument } from '@/lib/liveNotes';
import { formatNoteTimestamp } from '@/lib/liveNotes';

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
  if (Array.isArray(document.editorBlocks)) return blockNoteBlocksToMarkdown(document.editorBlocks);
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
  const totalSeconds = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const remainder = totalSeconds % 60;
  return hours > 0
    ? `[${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${remainder.toString().padStart(2, '0')}]`
    : `[${minutes.toString().padStart(2, '0')}:${remainder.toString().padStart(2, '0')}]`;
}

export function buildMeetingExport({
  title,
  createdAt,
  originalNotes,
  enhancedNotes,
  transcripts,
}: {
  title: string;
  createdAt?: string;
  originalNotes: string;
  enhancedNotes: string;
  transcripts: Transcript[];
}): string {
  const lines = [`# ${title || 'Untitled meeting'}`, ''];
  if (createdAt) lines.push(`_${new Date(createdAt).toLocaleString()}_`, '');
  if (originalNotes.trim()) lines.push('## Original notes', '', originalNotes.trim(), '');
  if (enhancedNotes.trim()) lines.push('## Enhanced notes', '', enhancedNotes.trim(), '');
  if (transcripts.length) lines.push('## Transcript', '', transcriptMarkdown(transcripts), '');
  return lines.join('\n').trimEnd() + '\n';
}

export function exportFileName(title: string, date = new Date()): string {
  const safeTitle = (title || 'meeting').replace(/[^\w\- ]+/g, '').trim().replace(/\s+/g, '-') || 'meeting';
  return `${safeTitle}-${date.toISOString().slice(0, 10)}.md`;
}
