import { describe, expect, test } from 'bun:test';
import { isWholesaleReplacement, nearestSegmentIndex } from './transcriptAnchor';

describe('isWholesaleReplacement', () => {
  test('a new transcription shares no row ids with the old one', () => {
    expect(isWholesaleReplacement(['seg_1', 'seg_2'], ['transcript-a', 'transcript-b'])).toBe(true);
  });

  test('pagination, speaker updates and first loads are not replacements', () => {
    expect(isWholesaleReplacement(['a', 'b'], ['a', 'b', 'c'])).toBe(false);
    expect(isWholesaleReplacement(['a', 'b'], ['a', 'b'])).toBe(false);
    expect(isWholesaleReplacement([], ['a'])).toBe(false);
    expect(isWholesaleReplacement(['a'], [])).toBe(false);
  });
});

describe('nearestSegmentIndex', () => {
  const rows = [{ timestamp: 0 }, { timestamp: 4.2 }, { timestamp: 9.8 }, { timestamp: 15 }];

  test('anchors to the first row at or just before the reading time', () => {
    expect(nearestSegmentIndex(rows, 0)).toBe(0);
    expect(nearestSegmentIndex(rows, 9)).toBe(2);
    expect(nearestSegmentIndex(rows, 10.1)).toBe(2);
    expect(nearestSegmentIndex(rows, 11)).toBe(3);
  });

  test('past the end stays at the end, and an empty list has no anchor', () => {
    expect(nearestSegmentIndex(rows, 99)).toBe(3);
    expect(nearestSegmentIndex([], 5)).toBe(-1);
  });
});
