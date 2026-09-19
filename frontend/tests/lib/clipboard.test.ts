import { afterEach, describe, expect, mock, test } from 'bun:test';
import { copyMeetingLink, meetingDeepLink, writeClipboardText } from '../../src/lib/clipboard';

const originalNavigator = globalThis.navigator;

afterEach(() => {
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: originalNavigator });
});

describe('clipboard operations', () => {
  test('copies a canonical encoded meeting link and awaits the write', async () => {
    let resolveWrite!: () => void;
    const writeText = mock(() => new Promise<void>((resolve) => { resolveWrite = resolve; }));
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText } },
    });

    let settled = false;
    const copying = copyMeetingLink('sales / review').then(() => { settled = true; });
    await Promise.resolve();
    expect(settled).toBe(false);
    expect(writeText).toHaveBeenCalledWith('minutes://meeting/sales%20%2F%20review');
    resolveWrite();
    await copying;
    expect(settled).toBe(true);
    expect(meetingDeepLink('sales / review')).toBe('minutes://meeting/sales%20%2F%20review');
  });

  test('propagates clipboard rejection', async () => {
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText: () => Promise.reject(new Error('permission denied')) } },
    });
    await expect(writeClipboardText('private text')).rejects.toThrow('permission denied');
  });
});
