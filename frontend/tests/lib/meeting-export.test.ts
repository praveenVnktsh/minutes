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

  test('rejects a repeated page that makes no progress', async () => {
    const repeated = transcript('1');
    await expect(fetchCompleteTranscripts('meeting', async () => ({
      transcripts: [repeated],
      total_count: 2,
      has_more: true,
    }))).rejects.toThrow('overlap');
  });

  test('rejects partially overlapping pages instead of claiming a complete count', async () => {
    const pages = [
      { transcripts: [transcript('1'), transcript('2')], total_count: 4, has_more: true },
      { transcripts: [transcript('2'), transcript('3')], total_count: 4, has_more: false },
    ];
    await expect(fetchCompleteTranscripts('meeting', async () => pages.shift()!))
      .rejects.toThrow('overlap');
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
        content: {
          type: 'tableContent',
          rows: [{
            cells: [
              { type: 'tableCell', props: { colspan: 1, rowspan: 1 }, content: [{ type: 'text', text: 'Owner', styles: {} }] },
              { type: 'tableCell', props: { colspan: 2, rowspan: 1 }, content: [{ type: 'text', text: 'Task', styles: {} }] },
            ],
          }, {
            cells: [
              { type: 'tableCell', props: { colspan: 1, rowspan: 1 }, content: [{ type: 'text', text: 'Sam', styles: {} }] },
              { type: 'tableCell', props: { colspan: 1, rowspan: 1 }, content: [{ type: 'text', text: 'Review', styles: {} }] },
            ],
          }],
        },
      }],
    });

    expect(markdown).toContain('- Read [**brief**](https://example.com)');
    expect(markdown).toContain('  - [x] Ship');
    expect(markdown).toContain('| Owner | Task |  |');
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

  test('omits empty structural blocks but preserves a visible nested child', () => {
    expect(blockNoteBlocksToMarkdown([
      { id: 'bullet', type: 'bulletListItem', content: [] },
      { id: 'heading', type: 'heading', props: { level: 2 }, content: [] },
      { id: 'code', type: 'codeBlock', content: [] },
    ])).toBe('');

    expect(blockNoteBlocksToMarkdown([{
      id: 'empty-parent',
      type: 'bulletListItem',
      content: [],
      children: [{ id: 'child', type: 'bulletListItem', content: [{ type: 'text', text: 'Visible child', styles: {} }] }],
    }])).toBe('- Visible child');
  });

  test('exports visible image and file blocks from their props', () => {
    expect(blockNoteBlocksToMarkdown([{
      id: 'image-id',
      type: 'image',
      props: { url: 'https://example.com/diagram.png', name: 'diagram.png', caption: 'System diagram' },
    }, {
      id: 'file-id',
      type: 'file',
      props: { url: 'https://example.com/brief.pdf', name: 'brief.pdf', caption: '' },
    }, {
      id: 'audio-id',
      type: 'audio',
      props: { url: '', name: 'local recording.wav', caption: '' },
    }])).toBe([
      '![System diagram](https://example.com/diagram.png)',
      '[brief.pdf](https://example.com/brief.pdf)',
      'Audio: local recording.wav',
    ].join('\n\n'));
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
