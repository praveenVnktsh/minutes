import { describe, expect, test } from 'bun:test';
import type { TimelineEntry } from '@/lib/liveTimeline';
import {
  aiBlocksCss,
  aiNoteText,
  insertPlace,
  pendingGroups,
  remainingAiBlocks,
  type AiBlocks,
} from './aiRawNotes';

function entry(id: string, startMs: number, topic: string | null, kind: TimelineEntry['kind'] = 'point'): TimelineEntry {
  return {
    id,
    start_ms: startMs,
    end_ms: startMs + 10_000,
    kind,
    text: `note ${id}`,
    speaker: kind === 'action' ? 'You' : null,
    provider: 'builtin-ai',
    model: 'gemma4:e2b',
    created_at: '2026-10-17T00:00:00Z',
    topic,
  };
}

describe('AI lines in the Raw notes', () => {
  test('pending live notes are grouped by topic in time order, skipping ones already written', () => {
    const groups = pendingGroups(
      [entry('c', 300, 'Hiring'), entry('a', 100, 'Pricing'), entry('b', 200, 'Pricing'), entry('d', 50, null)],
      new Set(['a']),
    );
    expect(groups.map((group) => [group.topic, group.entries.map((e) => e.id)])).toEqual([
      ['Notes', ['d']],
      ['Pricing', ['b']],
      ['Hiring', ['c']],
    ]);
  });

  test('lines read as notes: decisions and actions say so, actions name their owner', () => {
    expect(aiNoteText(entry('a', 0, 'X'))).toBe('note a');
    expect(aiNoteText(entry('b', 0, 'X', 'decision'))).toBe('Decision: note b');
    expect(aiNoteText(entry('c', 0, 'X', 'action'))).toBe('Action: You — note c');
  });

  const ai: AiBlocks = {
    h1: { entryId: '', text: 'Pricing', topic: 'Pricing' },
    p1: { entryId: 'a', text: 'note a', topic: 'Pricing' },
  };

  test('a topic grows under its last AI line when that is below the cursor', () => {
    // The cursor is in the first block, above the topic's lines.
    expect(insertPlace(['u1', 'h1', 'p1', 'u2'], 0, ai, 'Pricing')).toEqual({ afterBlockId: 'p1', withHeading: false });
  });

  test('never above the cursor: past it, lines go to the end with their heading again', () => {
    expect(insertPlace(['u1', 'h1', 'p1', 'u2'], 3, ai, 'Pricing')).toEqual({ afterBlockId: 'u2', withHeading: true });
    // The notes already end in that topic: no second heading.
    expect(insertPlace(['u1', 'h1', 'p1'], 2, ai, 'Pricing')).toEqual({ afterBlockId: 'p1', withHeading: false });
    // A new topic always starts with its heading at the end.
    expect(insertPlace(['u1', 'h1', 'p1'], 0, ai, 'Hiring')).toEqual({ afterBlockId: 'p1', withHeading: true });
    expect(insertPlace([], -1, ai, 'Hiring')).toBeNull();
  });

  test('an AI line the user edits or deletes becomes theirs', () => {
    const remaining = remainingAiBlocks(ai, new Map([['h1', 'Pricing'], ['p1', 'note a, and my addition']]));
    expect(Object.keys(remaining)).toEqual(['h1']);
    expect(remainingAiBlocks(ai, new Map())).toEqual({});
  });

  test('AI lines are marked with theme colours only', () => {
    const css = aiBlocksCss(ai);
    expect(css).toContain('.bn-block[data-id="p1"] > .bn-block-content');
    expect(css).toContain('var(--ink-muted)');
    expect(css).toContain('content: "AI"');
    expect(css).not.toMatch(/#[0-9a-f]{3,6}\b/i);
    expect(aiBlocksCss({ 'bad"id': { entryId: '', text: '', topic: '' } })).toBe('');
  });
});
