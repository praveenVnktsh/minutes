import { describe, expect, test } from 'bun:test';
import { citationSecondsAt, linkSummaryCitations, unlinkSummaryCitations } from './summaryCitations';
import { blockNoteBlocksToMarkdown } from './meetingExport';

describe('summary citations', () => {
  test('stored [MM:SS] text becomes transcript links for the editor', () => {
    expect(linkSummaryCitations('- Pricing at $40 [02:05]\n- Over an hour [1:02:03]'))
      .toBe('- Pricing at $40 [[02:05]](#copilot-t=125)\n- Over an hour [[1:02:03]](#copilot-t=3723)');
  });

  test('ordinary brackets and links are left alone', () => {
    const markdown = '- See [the doc](https://example.com) [TBD] [12:75]';
    expect(linkSummaryCitations(markdown)).toBe(markdown);
  });

  test('copies turn the links back into plain [MM:SS], escaped or not', () => {
    expect(unlinkSummaryCitations('- A [[02:05]](#copilot-t=125)')).toBe('- A [02:05]');
    expect(unlinkSummaryCitations('- A [\\[02:05\\]](#copilot-t=125)')).toBe('- A [02:05]');
    expect(unlinkSummaryCitations('[site](https://example.com)')).toBe('[site](https://example.com)');
    const markdown = '## Pricing\n\n- Forty per seat [02:05]';
    expect(unlinkSummaryCitations(linkSummaryCitations(markdown))).toBe(markdown);
  });

  test('a click inside a citation link yields its seconds', () => {
    const link = { getAttribute: (name: string) => (name === 'href' ? '#copilot-t=125' : null) };
    const inner = { closest: (selector: string) => (selector === 'a' ? link : null) };
    expect(citationSecondsAt(inner as unknown as EventTarget)).toBe(125);
    const other = { closest: () => ({ getAttribute: () => 'https://example.com' }) };
    expect(citationSecondsAt(other as unknown as EventTarget)).toBeNull();
    expect(citationSecondsAt({ closest: () => null } as unknown as EventTarget)).toBeNull();
    expect(citationSecondsAt(null)).toBeNull();
  });

  test('exports of edited notes keep the citation as text', () => {
    const blocks = [{
      type: 'bulletListItem',
      content: [
        { type: 'text', text: 'Forty per seat ', styles: {} },
        { type: 'link', href: '#copilot-t=125', content: [{ type: 'text', text: '[02:05]', styles: {} }] },
      ],
      children: [],
    }];
    expect(blockNoteBlocksToMarkdown(blocks)).toBe('- Forty per seat [02:05]');
  });
});
