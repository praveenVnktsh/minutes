import { describe, expect, test } from 'bun:test';
import type { Transcript } from '@/types';
import { formatSummaryTranscript, summarySpeakerLabel } from './summaryTranscript';

function segment(overrides: Partial<Transcript>): Transcript {
  return { id: 'id', text: 'hello', timestamp: '2026-10-16T10:00:00Z', ...overrides };
}

describe('formatSummaryTranscript', () => {
  test('writes [mm:ss] [Speaker] text lines', () => {
    expect(formatSummaryTranscript([
      segment({ audio_start_time: 5.9, speaker: 'mic', text: 'Ship it on Friday.' }),
      segment({ audio_start_time: 125, speaker: 'system', text: 'Agreed.' }),
      segment({ audio_start_time: 3725, speaker: 'Priya', text: 'One hour in.' }),
    ])).toBe([
      '[00:05] [You] Ship it on Friday.',
      '[02:05] [Others] Agreed.',
      '[62:05] [Priya] One hour in.',
    ].join('\n'));
  });

  test('never pastes the stored wall-clock or ISO timestamp', () => {
    const text = formatSummaryTranscript([
      segment({ speaker: 'mic', text: 'No audio time.', timestamp: '2026-10-16T10:00:00.000Z' }),
      segment({ text: 'No speaker either.', timestamp: '14:30:05' }),
    ]);
    expect(text).toBe('[You] No audio time.\nNo speaker either.');
    expect(text).not.toContain('2026');
    expect(text).not.toContain('14:30');
  });

  test('skips blank segments and trims text', () => {
    expect(formatSummaryTranscript([
      segment({ audio_start_time: 0, text: '   ' }),
      segment({ audio_start_time: 1, speaker: 'mic', text: '  kept  ' }),
    ])).toBe('[00:01] [You] kept');
  });

  test('ignores invalid audio times', () => {
    expect(formatSummaryTranscript([
      segment({ audio_start_time: Number.NaN, text: 'a' }),
      segment({ audio_start_time: -3, text: 'b' }),
    ])).toBe('a\nb');
  });
});

describe('summarySpeakerLabel', () => {
  test('maps capture sources to You and Others', () => {
    expect(summarySpeakerLabel('mic')).toBe('You');
    expect(summarySpeakerLabel('system')).toBe('Others');
    expect(summarySpeakerLabel('  Priya ')).toBe('Priya');
    expect(summarySpeakerLabel(undefined)).toBeNull();
    expect(summarySpeakerLabel(' ')).toBeNull();
  });
});
