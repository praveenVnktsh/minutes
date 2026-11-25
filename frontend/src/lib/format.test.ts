import { describe, expect, test } from 'bun:test';
import { bracketedClock, formatBytes, formatClock } from './format';

describe('formatClock', () => {
  test('counts minutes past an hour unless hours are asked for', () => {
    expect(formatClock(0)).toBe('00:00');
    expect(formatClock(307.9)).toBe('05:07');
    expect(formatClock(4503)).toBe('75:03');
    expect(formatClock(4503, { hours: true })).toBe('1:15:03');
    expect(formatClock(307, { hours: true })).toBe('05:07');
  });

  test('pads hours or drops minute padding on request', () => {
    expect(formatClock(3723, { padHours: true })).toBe('01:02:03');
    expect(formatClock(307, { padHours: true })).toBe('05:07');
    expect(formatClock(307, { hours: true, padMinutes: false })).toBe('5:07');
    expect(formatClock(3723, { hours: true, padMinutes: false })).toBe('1:02:03');
  });

  test('reads unusable values as zero', () => {
    expect(formatClock(-4)).toBe('00:00');
    expect(formatClock(Number.NaN)).toBe('00:00');
    expect(formatClock(Number.POSITIVE_INFINITY)).toBe('00:00');
  });

  test('brackets a transcript time, or shows the placeholder', () => {
    expect(bracketedClock(65)).toBe('[01:05]');
    expect(bracketedClock(undefined)).toBe('[--:--]');
    expect(bracketedClock(undefined, '[10:00]')).toBe('[10:00]');
  });

  test('sizes come from the one binary formatter', () => {
    expect(formatBytes(1536)).toBe('1.5 KiB');
  });
});
