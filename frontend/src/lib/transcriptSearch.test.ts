import { describe, expect, test } from 'bun:test';
import { resolveMatchTarget, transcriptMatchIds } from './transcriptSearch';

const loaded = [
  { id: 'a', text: 'Budget review' },
  { id: 'b', text: 'Filler' },
];

describe('in-meeting transcript search', () => {
  test('counts matches in rows that are not loaded yet once the database answers', () => {
    const remote = [
      { id: 'a', audio_start_time: 1 },
      { id: 'z', audio_start_time: 900 },
    ];
    expect(transcriptMatchIds(loaded, 'budget', remote)).toEqual(['a', 'z']);
  });

  test('searches the loaded rows until then, case-insensitively', () => {
    expect(transcriptMatchIds(loaded, '  BUDGET ', null)).toEqual(['a']);
    expect(transcriptMatchIds(loaded, '   ', null)).toEqual([]);
    expect(transcriptMatchIds(loaded, 'budget', [])).toEqual([]);
  });

  test('jumps to a loaded match and loads the rest for one that is not', () => {
    expect(resolveMatchTarget(loaded, 'b', true)).toEqual({ kind: 'jump', index: 1 });
    expect(resolveMatchTarget(loaded, 'z', true)).toEqual({ kind: 'load' });
    expect(resolveMatchTarget(loaded, 'z', false)).toEqual({ kind: 'missing' });
  });
});
