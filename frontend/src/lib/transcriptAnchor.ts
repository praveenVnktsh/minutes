/**
 * Keeping the reader's place when a transcript is swapped in place, e.g. when
 * the final pass replaces the live transcript. The new rows have new ids and
 * different boundaries, so the place is kept by time, not by row.
 */

/**
 * Whether `next` replaced `previous` wholesale (a new transcription), as
 * opposed to growing (pagination) or updating rows in place (speakers).
 */
export function isWholesaleReplacement(previous: readonly string[], next: readonly string[]): boolean {
  if (previous.length === 0 || next.length === 0) return false;
  const kept = new Set(next);
  return !previous.some((id) => kept.has(id));
}

/** Rows within this many seconds before the anchor still count as at it. */
const ANCHOR_TOLERANCE_SECONDS = 0.5;

/**
 * Index of the row to show at the top after a swap: the first row starting at
 * or just before `time`, else the last row (the reader was past the end).
 */
export function nearestSegmentIndex(
  segments: ReadonlyArray<{ timestamp: number }>,
  time: number,
): number {
  if (segments.length === 0) return -1;
  const index = segments.findIndex((segment) => segment.timestamp >= time - ANCHOR_TOLERANCE_SECONDS);
  return index === -1 ? segments.length - 1 : index;
}
