import { describe, expect, test } from 'bun:test';
import type { Transcript } from '../../src/types';
import {
  blockNoteBlocksToMarkdown,
  buildMeetingExport,
  fetchCompleteTranscripts,
  originalNotesMarkdown,
  storedSummaryMarkdown,
} from '../../src/lib/meetingExport';

function transcript(id: string): Transcript {
  return {
    id,
    text: `segment ${id}`,
    timestamp: `10:00:0${id}`,
    speaker: id === '1' ? 'mic' : 'Alex',
    audio_start_time: Number(id) * 10,
  };
}

describe('meeting export', () => {
  test('reads every capped transcript page and preserves timestamps and speakers', async () => {
    const requests: number[] = [];
    const rows = [transcript('1'), transcript('2'), transcript('3')];
    const result = await fetchCompleteTranscripts('meeting', async ({ offset }) => {
      requests.push(offset);
      const page = rows.slice(offset, offset + 2);
      return { transcripts: page, total_count: rows.length, has_more: offset + page.length < rows.length };
    });

    expect(requests).toEqual([0, 2]);
    const markdown = buildMeetingExport({
      title: 'Planning',
      createdAt: '2026-09-18T10:00:00Z',
      originalNotes: '',
      enhancedNotes: '',
      transcripts: result,
    });
    expect(markdown).toContain('[00:10] **You** segment 1');
    expect(markdown).toContain('[00:20] **Alex** segment 2');
    expect(markdown).toContain('[00:30] **Alex** segment 3');
  });

  test('rejects a page sequence that makes no progress', async () => {
    const repeated = transcript('1');
    await expect(fetchCompleteTranscripts('meeting', async () => ({
      transcripts: [repeated],
      total_count: 2,
      has_more: true,
    }))).rejects.toThrow('made no progress');
  });

  test('exports notes-only documents from complete editor blocks without internal ids', () => {
    const markdown = originalNotesMarkdown({
      version: 2,
      meetingStartedAtMs: 0,
      updatedAt: '2026-09-18T10:00:00Z',
      notes: [],
      rawMarkdown: 'stale markdown',
      editorBlocks: [{
        id: 'private-parent-id',
        type: 'bulletListItem',
        content: [
          { type: 'text', text: 'Read ', styles: {} },
          { type: 'link', href: 'https://example.com', content: [{ type: 'text', text: 'brief', styles: { bold: true } }] },
        ],
        children: [{ id: 'private-child-id', type: 'checkListItem', props: { checked: true }, content: [{ type: 'text', text: 'Ship', styles: {} }] }],
      }, {
        id: 'private-table-id',
        type: 'table',
        content: { rows: [{ cells: [[{ type: 'text', text: 'Owner', styles: {} }], [{ type: 'text', text: 'Task', styles: {} }]] }, { cells: [[{ type: 'text', text: 'Sam', styles: {} }], [{ type: 'text', text: 'Review', styles: {} }]] }] },
      }],
    });

    expect(markdown).toContain('- Read [**brief**](https://example.com)');
    expect(markdown).toContain('  - [x] Ship');
    expect(markdown).toContain('| Owner | Task |');
    expect(markdown).not.toContain('private-');
    expect(markdown).not.toContain('stale markdown');

    const exported = buildMeetingExport({
      title: 'Notes only',
      originalNotes: markdown,
      enhancedNotes: '',
      transcripts: [],
    });
    expect(exported).toContain('## Original notes');
    expect(exported).not.toContain('## Transcript');
  });

  test('treats a present empty editor document as genuinely empty', () => {
    expect(originalNotesMarkdown({
      version: 2,
      meetingStartedAtMs: 0,
      updatedAt: '2026-09-18T10:00:00Z',
      notes: [{ id: 'old', timestampSeconds: 0, text: 'stale', important: false }],
      rawMarkdown: 'stale',
      editorBlocks: [],
    })).toBe('');
  });

  test('converts structured enhanced notes when the editor is unmounted', () => {
    const markdown = storedSummaryMarkdown({
      summary_json: [{
        id: 'summary-id',
        type: 'heading',
        props: { level: 2 },
        content: [{ type: 'text', text: 'Decisions', styles: {} }],
        children: [{ id: 'decision-id', type: 'bulletListItem', content: [{ type: 'text', text: 'Launch Friday', styles: { bold: true } }] }],
      }],
    });

    expect(markdown).toBe('## Decisions\n  - **Launch Friday**');
    expect(blockNoteBlocksToMarkdown([])).toBe('');
  });
});
