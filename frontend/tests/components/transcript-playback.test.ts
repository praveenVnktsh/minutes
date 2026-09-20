import { describe, expect, test } from 'bun:test';
import {
  findSegmentIdAtTime,
  resolveActiveSegmentId,
} from '../../src/components/MeetingDetails/TranscriptPanel';

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

const idle = {
  hasAudio: true,
  scrubTime: null,
  seekTarget: null,
  isPlaying: false,
  currentTime: 0,
};

describe('resolveActiveSegmentId', () => {
  test('highlights nothing when the meeting has no audio', () => {
    expect(resolveActiveSegmentId(segments, {
      ...idle,
      hasAudio: false,
      seekTarget: 6,
      isPlaying: true,
      currentTime: 13,
    })).toBeUndefined();
  });

  test('a scrub in progress wins over the playback position', () => {
    expect(resolveActiveSegmentId(segments, {
      ...idle,
      scrubTime: 13,
      seekTarget: 6,
      isPlaying: true,
      currentTime: 1,
    })).toBe('c');
  });

  test('a live playback position wins over the remembered seek target', () => {
    expect(resolveActiveSegmentId(segments, {
      ...idle,
      seekTarget: 13,
      isPlaying: true,
      currentTime: 6,
    })).toBe('b');
  });

  test('falls back to the seek target while playback is still at zero', () => {
    expect(resolveActiveSegmentId(segments, { ...idle, seekTarget: 13 })).toBe('c');
  });

  test('a seek to the very start highlights the first line', () => {
    expect(resolveActiveSegmentId(segments, { ...idle, seekTarget: 0 })).toBe('a');
  });

  test('highlights nothing when nothing is playing and nothing was asked for', () => {
    expect(resolveActiveSegmentId(segments, idle)).toBeUndefined();
  });
});
