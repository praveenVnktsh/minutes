import { describe, expect, test } from 'bun:test';
import {
  BYTES_PER_MIB,
  ETA_INDETERMINATE,
  formatBytes,
  formatBytesPerSecond,
  formatDuration,
  formatEta,
  mibToBytes,
} from './download-display';

// The Parakeet model and the built-in summary model: the two real downloads
// whose advertised size and live counter used to disagree.
const PARAKEET_BYTES = 670_619_706;
const SUMMARY_MODEL_BYTES = 2_740_977_664;

describe('formatBytes', () => {
  test('renders the real model sizes', () => {
    expect(formatBytes(PARAKEET_BYTES)).toBe('639.6 MiB');
    expect(formatBytes(SUMMARY_MODEL_BYTES)).toBe('2.55 GiB');
  });

  test('picks its unit from magnitude', () => {
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(1024)).toBe('1.0 KiB');
    expect(formatBytes(BYTES_PER_MIB)).toBe('1.0 MiB');
    expect(formatBytes(1024 ** 3)).toBe('1.00 GiB');
    expect(formatBytes(1024 ** 4)).toBe('1.00 TiB');
  });

  test('never labels a binary quantity with a decimal suffix', () => {
    const rendered = [0, 999, 1024, BYTES_PER_MIB, PARAKEET_BYTES, SUMMARY_MODEL_BYTES].map(formatBytes);
    for (const size of rendered) {
      expect(size).toMatch(/^[\d.]+ (B|KiB|MiB|GiB|TiB)$/);
    }
  });

  test('answers half-formed progress events with a readable zero', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(-1)).toBe('0 B');
    expect(formatBytes(-PARAKEET_BYTES)).toBe('0 B');
    expect(formatBytes(Number.NaN)).toBe('0 B');
    expect(formatBytes(Number.POSITIVE_INFINITY)).toBe('0 B');
    expect(formatBytes(Number.NEGATIVE_INFINITY)).toBe('0 B');
  });
});

describe('mibToBytes', () => {
  test('converts in the same base the formatter reads', () => {
    expect(mibToBytes(1)).toBe(BYTES_PER_MIB);
    expect(BYTES_PER_MIB).toBe(1_048_576);
  });

  test('a size routed through MiB prints the same as the raw byte count', () => {
    expect(formatBytes(mibToBytes(PARAKEET_BYTES / BYTES_PER_MIB))).toBe(formatBytes(PARAKEET_BYTES));
  });

  test('is total', () => {
    expect(mibToBytes(0)).toBe(0);
    expect(mibToBytes(-3)).toBe(0);
    expect(mibToBytes(Number.NaN)).toBe(0);
    expect(mibToBytes(Number.POSITIVE_INFINITY)).toBe(0);
  });
});

describe('formatBytesPerSecond', () => {
  test('shares the suffix family with formatBytes', () => {
    expect(formatBytesPerSecond(13_002_342)).toBe('12.4 MiB/s');
    expect(formatBytesPerSecond(1024)).toBe('1.0 KiB/s');
  });

  test('is total', () => {
    expect(formatBytesPerSecond(0)).toBe('0 B/s');
    expect(formatBytesPerSecond(-5)).toBe('0 B/s');
    expect(formatBytesPerSecond(Number.NaN)).toBe('0 B/s');
    expect(formatBytesPerSecond(Number.POSITIVE_INFINITY)).toBe('0 B/s');
  });
});

describe('formatDuration', () => {
  test('reads as human time at each scale', () => {
    expect(formatDuration(45)).toBe('about 45 sec');
    expect(formatDuration(240)).toBe('about 4 min');
    expect(formatDuration(3600)).toBe('about 1 hr');
    expect(formatDuration(4800)).toBe('about 1 hr 20 min');
  });

  test('rounds across the unit boundaries instead of overflowing them', () => {
    expect(formatDuration(59.6)).toBe('about 1 min');
    expect(formatDuration(90)).toBe('about 2 min');
    expect(formatDuration(3599)).toBe('about 1 hr');
    expect(formatDuration(7199)).toBe('about 2 hr');
  });

  test('never prints about 0 sec', () => {
    expect(formatDuration(0)).toBe('about 1 sec');
    expect(formatDuration(0.2)).toBe('about 1 sec');
    expect(formatDuration(-30)).toBe('about 1 sec');
    for (let seconds = 0; seconds < 90; seconds += 0.5) {
      expect(formatDuration(seconds)).not.toBe('about 0 sec');
    }
  });

  test('falls back to the indeterminate copy when the number is not a number', () => {
    expect(formatDuration(Number.NaN)).toBe(ETA_INDETERMINATE);
    expect(formatDuration(Number.POSITIVE_INFINITY)).toBe(ETA_INDETERMINATE);
    expect(formatDuration(Number.NEGATIVE_INFINITY)).toBe(ETA_INDETERMINATE);
  });
});

describe('formatEta', () => {
  test('shows the shared indeterminate copy while the rate is unstable', () => {
    expect(ETA_INDETERMINATE).toBe('Estimating time remaining…');
    expect(formatEta(null)).toBe(ETA_INDETERMINATE);
    expect(formatEta(undefined)).toBe(ETA_INDETERMINATE);
  });

  test('otherwise defers to formatDuration', () => {
    expect(formatEta(240)).toBe('about 4 min');
    expect(formatEta(0)).toBe('about 1 sec');
  });
});
