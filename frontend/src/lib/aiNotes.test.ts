import { describe, expect, test } from 'bun:test';
import type { TimelineEntry } from '@/lib/liveTimeline';
import type { LiveNotesDocument } from '@/lib/liveNotes';
import { aiBlocksCss } from '@/lib/aiRawNotes';
import { buildMeetingExport } from '@/lib/meetingExport';
import { parseBetaFeatures, DEFAULT_BETA_FEATURES } from '@/types/betaFeatures';
import {
  actionItemText,
  completeAiNotes,
  hasAiNotes,
  legacyAiNotes,
  notesMarkdown,
  WRAP_UP_ENTRY,
} from './aiNotes';

function entry(id: string, startMs: number, kind: TimelineEntry['kind'], text: string, topic: string, speaker: string | null = null): TimelineEntry {
  return {
    id,
    start_ms: startMs,
    end_ms: startMs + 5_000,
    kind,
    text,
    speaker,
    provider: 'builtin-ai',
    model: 'gemma4:e2b',
    created_at: '2026-10-17T00:00:00Z',
    topic,
  };
}

const content = (text: string) => [{ type: 'text', text, styles: {} }];

/** The user's line, an AI topic with one line already written, and the
 * user's trailing empty bullet. */
function meetingDocument(): LiveNotesDocument {
  return {
    version: 2,
    meetingStartedAtMs: 0,
    updatedAt: '2026-10-17T10:00:00Z',
    notes: [
      { id: 'mine', timestampSeconds: 1, text: 'Ask about budget', important: false },
      { id: 'h1', timestampSeconds: 5, text: 'Pricing', important: false },
      { id: 'a1', timestampSeconds: 5, text: 'Free tier held back', important: false },
      { id: 'empty', timestampSeconds: 9, text: '', important: false },
    ],
    editorBlocks: [
      { id: 'mine', type: 'bulletListItem', props: {}, content: content('Ask about budget'), children: [] },
      { id: 'h1', type: 'heading', props: { level: 3 }, content: content('Pricing'), children: [] },
      { id: 'a1', type: 'bulletListItem', props: {}, content: content('Free tier held back'), children: [] },
      { id: 'empty', type: 'bulletListItem', props: {}, content: [], children: [] },
    ],
    aiBlocks: {
      h1: { entryId: '', text: 'Pricing', topic: 'Pricing' },
      a1: { entryId: 'e1', text: 'Free tier held back', topic: 'Pricing' },
    },
    aiEntries: ['e1'],
  };
}

const entries = [
  entry('e1', 5_000, 'point', 'Free tier held back', 'Pricing'),
  entry('e2', 60_000, 'decision', 'Launch on Nov 12', 'Launch'),
  entry('e3', 70_000, 'action', 'Draft the launch email', 'Launch', 'Priya'),
  entry('e4', 80_000, 'action', 'Book the room', 'Launch'),
];

function ids() {
  let next = 0;
  return () => `new-${++next}`;
}

function blockTexts(document: LiveNotesDocument): string[] {
  return (document.editorBlocks as { content: { text: string }[] }[]).map((block) => block.content.map((part) => part.text).join(''));
}

describe('Decisions and Action items when the meeting ends', () => {
  test('adds the live notes not written yet, then Decisions and Action items, as AI lines above the trailing empty line', () => {
    const before = meetingDocument();
    const after = completeAiNotes(before, entries, ids())!;
    expect(blockTexts(after)).toEqual([
      'Ask about budget',
      'Pricing',
      'Free tier held back',
      'Launch',
      'Decision: Launch on Nov 12',
      'Action: Priya — Draft the launch email',
      'Action: Others — Book the room',
      'Decisions',
      'Launch on Nov 12',
      'Action items',
      'Priya — Draft the launch email',
      'Book the room',
      '',
    ]);
    // Every added line is the AI's; the user's lines are untouched and not AI.
    const added = (after.editorBlocks as { id: string }[]).map((block) => block.id).filter((id) => id.startsWith('new-'));
    expect(added).toHaveLength(9);
    for (const id of added) expect(after.aiBlocks?.[id]).toBeDefined();
    expect(after.aiBlocks?.mine).toBeUndefined();
    expect(after.aiEntries).toContain(WRAP_UP_ENTRY);
    expect(after.aiEntries).toEqual(expect.arrayContaining(['e1', 'e2', 'e3', 'e4']));
    // The notes list follows the blocks, so the user's notes stay theirs.
    expect(after.notes.map((note) => note.id)).toEqual((after.editorBlocks as { id: string }[]).map((block) => block.id));
    expect(after.rawMarkdown).toContain('### Action items');
    // The stored document passed in is not changed.
    expect(blockTexts(before)).toHaveLength(4);
  });

  test('runs once: a completed document is left alone', () => {
    const after = completeAiNotes(meetingDocument(), entries, ids())!;
    expect(completeAiNotes(after, entries, ids())).toBeNull();
  });

  test('a meeting without decisions or actions only gets the marker', () => {
    const after = completeAiNotes(meetingDocument(), [entries[0]], ids())!;
    expect(blockTexts(after)).toEqual(blockTexts(meetingDocument()));
    expect(after.aiEntries).toEqual(['e1', WRAP_UP_ENTRY]);
  });

  test('never rewrites a document the AI did not write into (an old meeting)', () => {
    const old: LiveNotesDocument = { ...meetingDocument(), aiBlocks: undefined, aiEntries: undefined };
    expect(completeAiNotes(old, entries, ids())).toBeNull();
  });

  test('a deleted live note is not added back', () => {
    const document = { ...meetingDocument(), aiEntries: ['e1', 'e2'] };
    const after = completeAiNotes(document, [entries[0], entries[1]], ids())!;
    expect(blockTexts(after)).not.toContain('Decision: Launch on Nov 12');
    // Decisions still lists it: the wrap-up reads every decision.
    expect(blockTexts(after)).toContain('Launch on Nov 12');
  });

  test('action items read "Owner — task", or just the task without an owner', () => {
    expect(actionItemText({ text: 'Ship it', speaker: 'You' })).toBe('You — Ship it');
    expect(actionItemText({ text: 'Ship it', speaker: null })).toBe('Ship it');
  });
});

describe('Show AI notes', () => {
  test('off hides AI lines (never deletes them); on marks them muted with an AI tag', () => {
    const aiBlocks = meetingDocument().aiBlocks!;
    const hidden = aiBlocksCss(aiBlocks, false);
    expect(hidden).toContain('.bn-block-outer[data-id="a1"]');
    expect(hidden).toContain('display: none');
    expect(hidden).not.toContain('"AI"');
    const shown = aiBlocksCss(aiBlocks, true);
    expect(shown).toContain('content: "AI"');
    expect(shown).not.toContain('display: none');
  });

  test('notes Markdown keeps AI lines only when shown', () => {
    const document = meetingDocument();
    expect(notesMarkdown(document, true)).toBe('- Ask about budget\n\n### Pricing\n\n- Free tier held back');
    expect(notesMarkdown(document, false)).toBe('- Ask about budget');
  });

  test('an old meeting shows its stored Enhanced notes below its notes, only while shown', () => {
    const old: LiveNotesDocument = { ...meetingDocument(), aiBlocks: undefined, aiEntries: undefined };
    expect(hasAiNotes(old)).toBe(false);
    expect(legacyAiNotes(old, '## Decisions\n- Ship', true)).toBe('## Decisions\n- Ship');
    expect(legacyAiNotes(old, '## Decisions\n- Ship', false)).toBe('');
    expect(legacyAiNotes(null, 'Summary', true)).toBe('Summary');
    // A meeting whose doc has AI lines never shows the stored summary too.
    expect(legacyAiNotes(meetingDocument(), 'Summary', true)).toBe('');
  });

  test('the export has one Notes section, and AI notes only for an old meeting', () => {
    const contents = buildMeetingExport({ title: 'Planning', notes: '- Mine', aiNotes: '- Old summary', transcripts: [] });
    expect(contents).toBe('# Planning\n\n## Notes\n\n- Mine\n\n## AI notes\n\n- Old summary\n');
    expect(buildMeetingExport({ title: 'Planning', notes: '- Mine', transcripts: [] })).not.toContain('AI notes');
  });
});

describe('retired setting', () => {
  test('the old "Write live notes into my notes" flag is ignored and dropped', () => {
    const features = parseBetaFeatures({ liveNotesInRawNotes: false, liveTranscription: false, bogus: 'x' });
    expect(features).toEqual({ ...DEFAULT_BETA_FEATURES, liveTranscription: false });
    expect('liveNotesInRawNotes' in features).toBe(false);
    expect(parseBetaFeatures(null)).toEqual(DEFAULT_BETA_FEATURES);
    expect(parseBetaFeatures({ micEchoCancellation: 'yes' })).toEqual(DEFAULT_BETA_FEATURES);
  });
});
