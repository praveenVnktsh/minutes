import { LIVE_NOTES_MODEL } from '@/lib/builtin-ai';

/**
 * Onboarding's optional live notes model (Gemma 4 E2B). It is opt-in: a checkbox on the setup
 * overview, ticked by default only on Macs with 16 GB or more, and nothing downloads until
 * onboarding completes, when the Rust side waits for the required models (Parakeet and the
 * summary model) before fetching it.
 */

/** The checkbox's starting state: the backend's memory-based default, off when unknown. */
export function liveNotesOptInDefault(settings: { opt_in_by_default: boolean } | null): boolean {
  return settings?.opt_in_by_default ?? false;
}

/**
 * Whether completing onboarding asks for the live notes download: only when ticked, and not when
 * the summary model already is the live notes model (one model serves both).
 */
export function shouldRequestLiveNotesDownload(optIn: boolean, summaryModel: string): boolean {
  return optIn && !!summaryModel && summaryModel !== LIVE_NOTES_MODEL;
}

/** The total the overview shows: the optional model counts only when ticked. Null until known. */
export function onboardingDownloadTotal(
  parakeetBytes: number | null,
  summaryBytes: number | null,
  liveNotesBytes: number | null,
  optIn: boolean,
): number | null {
  if (parakeetBytes === null || summaryBytes === null) return null;
  if (!optIn) return parakeetBytes + summaryBytes;
  return liveNotesBytes === null ? null : parakeetBytes + summaryBytes + liveNotesBytes;
}
