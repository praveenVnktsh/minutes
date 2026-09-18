import { describe, expect, test } from 'bun:test';
import { findSegmentIdAtTime } from '../../src/components/MeetingDetails/TranscriptPanel';

const segments = [
  { id: 'a', timestamp: 0, endTime: 5 },
  { id: 'b', timestamp: 5, endTime: 12 },
  { id: 'c', timestamp: 12 },
];

describe('findSegmentIdAtTime', () => {
  test('returns the segment covering the playback time', () => {
    expect(findSegmentIdAtTime(segments, 0)).toBe('a');
    expect(findSegmentIdAtTime(segments, 4.9)).toBe('a');
    expect(findSegmentIdAtTime(segments, 5)).toBe('b');
    expect(findSegmentIdAtTime(segments, 12)).toBe('c');
    expect(findSegmentIdAtTime(segments, 40)).toBe('c');
  });

  test('falls back to the next start time when endTime is missing', () => {
    const open = [
      { id: 'a', timestamp: 0 },
      { id: 'b', timestamp: 8 },
    ];
    expect(findSegmentIdAtTime(open, 7.9)).toBe('a');
    expect(findSegmentIdAtTime(open, 8)).toBe('b');
  });

  test('returns nothing before the first spoken segment', () => {
    expect(findSegmentIdAtTime([{ id: 'a', timestamp: 3, endTime: 6 }], 0)).toBeUndefined();
    expect(findSegmentIdAtTime([], 4)).toBeUndefined();
  });
});
