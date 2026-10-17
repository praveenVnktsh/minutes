import { CITATION_HREF_PREFIX, citationHrefSeconds, linkifyCitations } from '@/lib/copilot';

/**
 * `[MM:SS]` links in the enhanced notes. The summary is stored with plain
 * `[MM:SS]` text; the editor shows each one as a link that scrolls the
 * transcript to that moment (the Copilot's citation links), and copies and
 * exports turn them back into plain text.
 */

/** The stored notes as the editor parses them: citations become links. */
export function linkSummaryCitations(markdown: string): string {
  return linkifyCitations(markdown);
}

const LINKED_CITATION = new RegExp(
  String.raw`\[\\?\[((?:\d{1,2}:)?\d{1,2}:\d{2})\\?\]\]\(${CITATION_HREF_PREFIX.replace(/[.*+?^${}()|[\]\\#]/g, '\\$&')}\d+(?:\.\d+)?\)`,
  'g',
);

/** Markdown from the editor with citation links back to plain `[MM:SS]`. */
export function unlinkSummaryCitations(markdown: string): string {
  return markdown.replace(LINKED_CITATION, (_whole, time: string) => `[${time}]`);
}

/** Seconds of the citation link a click landed on, or null. */
export function citationSecondsAt(target: EventTarget | null): number | null {
  if (!target || typeof (target as Element).closest !== 'function') return null;
  const link = (target as Element).closest('a');
  return citationHrefSeconds(link?.getAttribute('href') ?? undefined);
}
