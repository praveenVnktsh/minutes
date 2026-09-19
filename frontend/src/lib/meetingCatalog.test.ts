import { describe, expect, test } from 'bun:test';
import {
  meetingUrl,
  mergeMeetingCatalog,
  selectMeetings,
  selectSearchResults,
  type MeetingCatalogEntry,
} from './meetingCatalog';

const meetings: MeetingCatalogEntry[] = [
  { id: 'regular', title: 'Regular', created_at: '2026-09-18', pinned: false, archived: false, debug: false },
  { id: 'pinned', title: 'Pinned', pinned: true, archived: false, debug: false },
  { id: 'archive', title: 'Archive', pinned: false, archived: true, debug: false },
  { id: 'debug', title: 'Debug', pinned: false, archived: false, debug: true },
];

describe('meeting catalog selectors', () => {
  test('preserves catalog metadata when a partial identity is merged', () => {
    expect(mergeMeetingCatalog(meetings, [{ id: 'pinned', title: 'Renamed' }])).toEqual([
      { id: 'pinned', title: 'Renamed', pinned: true, archived: false, debug: false },
    ]);
  });

  test('applies archived, debug, and pinned visibility consistently', () => {
    expect(selectMeetings(meetings).map(({ id }) => id)).toEqual(['regular', 'pinned']);
    expect(selectMeetings(meetings, { pinned: true }).map(({ id }) => id)).toEqual(['pinned']);
    expect(selectMeetings(meetings, { includeArchived: true }).map(({ id }) => id)).toEqual([
      'regular', 'pinned', 'archive',
    ]);
    expect(selectMeetings(meetings, { debugMode: true }).map(({ id }) => id)).toEqual([
      'regular', 'pinned', 'debug',
    ]);
  });

  test('resolves search identities from catalog metadata and omits unknown rows', () => {
    const results = [
      { id: 'archive', title: 'Wrong title', matchContext: 'hit', timestamp: '0' },
      { id: 'debug', title: 'Wrong title', matchContext: 'hit', timestamp: '0' },
      { id: 'regular', title: 'Wrong title', matchContext: 'hit', timestamp: '0' },
      { id: 'unknown', title: 'Title-only fallback', matchContext: 'hit', timestamp: '0' },
    ];

    expect(selectSearchResults(results, meetings)).toEqual([meetings[0]]);
    expect(selectSearchResults(results, meetings, { includeArchived: true, debugMode: true })).toEqual([
      meetings[2], meetings[3], meetings[0],
    ]);
  });

  test('encodes meeting identity in the canonical URL', () => {
    expect(meetingUrl('team / review?#')).toBe('/meeting-details?id=team%20%2F%20review%3F%23');
  });
});
