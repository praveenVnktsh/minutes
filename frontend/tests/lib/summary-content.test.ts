import { describe, expect, test } from 'bun:test';
import { hasVisibleSummaryContent, isManuallyClearedSummary, parseSummaryContent } from '../../src/lib/summary-content';

describe('summary content validation', () => {
  test('accepts visible markdown and rejects whitespace or reasoning markers', () => {
    expect(hasVisibleSummaryContent({ markdown: '# Summary\nVisible' })).toBe(true);
    expect(hasVisibleSummaryContent({ markdown: '   ' })).toBe(false);
    expect(hasVisibleSummaryContent({ markdown: '<think>private</think>' })).toBe(false);
    expect(hasVisibleSummaryContent({
      markdown: '<thinking class="internal">private</thinking>',
    })).toBe(false);
    expect(hasVisibleSummaryContent({ markdown: '<thinker>Visible</thinker>' })).toBe(true);
  });

  test('validates BlockNote and legacy summary content', () => {
    expect(hasVisibleSummaryContent({ summary_json: [] })).toBe(false);
    expect(hasVisibleSummaryContent({
      summary_json: [{ content: [{ type: 'text', text: 'Visible' }] }],
    })).toBe(true);
    expect(hasVisibleSummaryContent({
      Decisions: { blocks: [{ content: '   ' }] },
    })).toBe(false);
    expect(hasVisibleSummaryContent({
      Decisions: { blocks: [{ content: 'Ship it' }] },
    })).toBe(true);
  });

  test('parses one historical double-encoded payload', () => {
    expect(parseSummaryContent('{"markdown":"Visible"}')).toEqual({ markdown: 'Visible' });
    expect(parseSummaryContent('{not json')).toBeNull();
  });

  test('recognizes only the explicit empty manual document representation', () => {
    const cleared = { markdown: '', summary_json: [], manually_cleared: true };
    expect(isManuallyClearedSummary(cleared)).toBe(true);
    expect(parseSummaryContent(cleared)).toEqual(cleared);
    expect(isManuallyClearedSummary({ markdown: '', summary_json: [] })).toBe(false);
    expect(isManuallyClearedSummary({ markdown: '', summary_json: [], manually_cleared: true, Decisions: {} })).toBe(false);
    expect(isManuallyClearedSummary({ markdown: '<think>hidden</think>', summary_json: [], manually_cleared: true })).toBe(false);
  });
});
