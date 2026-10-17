import { describe, expect, test } from 'bun:test';
import { LIVE_NOTES_MODEL } from '../../src/lib/builtin-ai';
import {
  liveNotesOptInDefault,
  onboardingDownloadTotal,
  shouldRequestLiveNotesDownload,
} from '../../src/lib/liveNotesOnboarding';

describe('live notes model in onboarding', () => {
  test('the checkbox follows the memory default and is off when unknown', () => {
    expect(liveNotesOptInDefault({ opt_in_by_default: true })).toBe(true);
    expect(liveNotesOptInDefault({ opt_in_by_default: false })).toBe(false);
    expect(liveNotesOptInDefault(null)).toBe(false);
  });

  test('nothing is requested without the tick', () => {
    expect(shouldRequestLiveNotesDownload(false, 'qwen3.5:4b')).toBe(false);
    expect(shouldRequestLiveNotesDownload(true, 'qwen3.5:4b')).toBe(true);
    expect(shouldRequestLiveNotesDownload(true, LIVE_NOTES_MODEL)).toBe(false);
    expect(shouldRequestLiveNotesDownload(true, '')).toBe(false);
  });

  test('the optional model counts in the total only when ticked', () => {
    expect(onboardingDownloadTotal(1, 2, 4, false)).toBe(3);
    expect(onboardingDownloadTotal(1, 2, 4, true)).toBe(7);
    expect(onboardingDownloadTotal(1, 2, null, true)).toBeNull();
    expect(onboardingDownloadTotal(1, 2, null, false)).toBe(3);
    expect(onboardingDownloadTotal(null, 2, 4, false)).toBeNull();
  });
});
