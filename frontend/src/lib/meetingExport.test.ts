import { describe, expect, test } from 'bun:test';
import type { Transcript } from '@/types';
import {
  buildFrontmatter,
  buildMeetingExport,
  exportFileName,
  localDate,
  localIsoTimestamp,
  meetingPeopleNames,
  normaliseTags,
  transcriptDurationSeconds,
} from '@/lib/meetingExport';

const exportedAt = new Date(2026, 10, 27, 10, 5, 12);
const createdAt = new Date(2026, 10, 27, 9, 30, 0);

/** The frontmatter's lines, without the `---` fences. */
function fields(block: string): string[] {
  expect(block.startsWith('---\n')).toBe(true);
  expect(block.endsWith('\n---\n\n')).toBe(true);
  return block.slice(4, -6).split('\n');
}

/** The value of a `key: <json>` line, parsed back as YAML would read it. */
function scalar(block: string, key: string): unknown {
  const line = fields(block).find((candidate) => candidate.startsWith(`${key}: `));
  return line === undefined ? undefined : JSON.parse(line.slice(key.length + 2));
}

const segment = (id: string, start: number, end: number, text = 'hi'): Transcript => ({
  id, text, timestamp: '00:00', audio_start_time: start, audio_end_time: end,
});

describe('buildFrontmatter', () => {
  test('writes every field, dated in local time', () => {
    const block = buildFrontmatter({
      title: 'Team standup',
      meetingId: 'm-1',
      createdAt,
      durationSeconds: 32 * 60 + 10,
      attendees: ['Alice', ' Bob ', ''],
      tags: ['minutes-app', 'work'],
      exportedAt,
    });
    expect(fields(block)).toEqual([
      'title: "Team standup"',
      'date: 2026-11-27',
      `created: "${localIsoTimestamp(createdAt)}"`,
      'duration_minutes: 32',
      'attendees:',
      '  - "Alice"',
      '  - "Bob"',
      'tags:',
      '  - "minutes-app"',
      '  - "work"',
      'minutes_id: "m-1"',
      'minutes_link: "minutes://meeting/m-1"',
      `exported_at: "${localIsoTimestamp(exportedAt)}"`,
    ]);
  });

  test('quotes titles that would otherwise break YAML', () => {
    for (const title of ['Plan: Q4', 'He said "ship it"', '#1 priority', 'Line one\nline two', "it's - [x]"]) {
      const block = buildFrontmatter({ title, meetingId: 'm', exportedAt });
      expect(scalar(block, 'title')).toBe(title);
      // One line per field: a newline in the title stays escaped.
      expect(fields(block).filter((line) => line.startsWith('title:'))).toHaveLength(1);
    }
  });

  test('leaves out what it does not know', () => {
    const block = buildFrontmatter({ title: '', meetingId: 'm', exportedAt, attendees: [], tags: ['###'] });
    const keys = fields(block).map((line) => line.split(':')[0]);
    expect(keys).toEqual(['title', 'minutes_id', 'minutes_link', 'exported_at']);
    expect(scalar(block, 'title')).toBe('Untitled meeting');
  });

  test('timestamps carry their offset', () => {
    expect(localIsoTimestamp(exportedAt)).toMatch(/^2026-11-27T10:05:12[+-]\d{2}:\d{2}$/);
    expect(localDate(new Date(2026, 0, 5))).toBe('2026-01-05');
  });
});

describe('normaliseTags', () => {
  test('makes tags Obsidian accepts, each once', () => {
    expect(normaliseTags(['#minutes-app', ' team standup ', '2026', '', 'ok/nested', 'Ünïcode!', 'MINUTES-APP', '#', '/x/']))
      .toEqual(['minutes-app', 'team-standup', 'ok/nested', 'Ünïcode', 'x']);
  });
});

describe('buildMeetingExport', () => {
  test('is unchanged without frontmatter', () => {
    expect(buildMeetingExport({ title: 'Planning', notes: '- Mine', transcripts: [segment('a', 0, 2)] }))
      .toBe('# Planning\n\n## Notes\n\n- Mine\n\n## Transcript\n\n- [00:00] hi\n');
  });

  test('puts the frontmatter first and drops the date line it replaces', () => {
    const frontmatter = buildFrontmatter({ title: 'Planning', meetingId: 'm', createdAt, exportedAt });
    const contents = buildMeetingExport({
      title: 'Planning',
      createdAt: createdAt.toISOString(),
      notes: '- Mine',
      transcripts: [],
      frontmatter,
    });
    expect(contents).toBe(`${frontmatter}# Planning\n\n## Notes\n\n- Mine\n`);
  });

  test('leaves the transcript out when the setting is off', () => {
    const contents = buildMeetingExport({
      title: 'Planning',
      notes: '- Mine',
      transcripts: [segment('a', 0, 2)],
      includeTranscript: false,
    });
    expect(contents).not.toContain('Transcript');
  });
});

test('duration is where the last segment ends', () => {
  expect(transcriptDurationSeconds([segment('a', 0, 4), segment('b', 4, 125.5), segment('c', 2, 3)])).toBe(125.5);
  expect(transcriptDurationSeconds([])).toBeUndefined();
});

test('file name uses the local date', () => {
  expect(exportFileName('Team: standup!', new Date(2026, 10, 27, 23, 59))).toBe('Team-standup-2026-11-27.md');
});

test('people are the attendees, then speakers the user named', () => {
  expect(meetingPeopleNames(['Dale Lewis', ' '], [
    { speaker_id: 'speaker_00', display_name: 'dale lewis' },
    { speaker_id: 'speaker_01', display_name: 'Howard Frederick' },
    { speaker_id: 'speaker_02', display_name: 'speaker_02' },
    { speaker_id: 'mic', display_name: 'mic' },
    { speaker_id: 'system', display_name: '' },
  ])).toEqual(['Dale Lewis', 'Howard Frederick']);
  expect(meetingPeopleNames([], [])).toEqual([]);
});
