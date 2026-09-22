/**
 * One base for every download size, rate and estimate the UI prints.
 *
 * The models screens used to advertise a catalogue size in decimal MB while the
 * live counter divided the very same byte count by 1024 and still called it
 * "MB", so a 670,619,706-byte download was announced as "~670 MB" and then
 * finished at "639.6 MB" — a bar that settles thirty units short of its own
 * label, at exactly the moment a new user is deciding whether setup worked.
 * Everything here is therefore binary (1024) and every suffix says so: `MiB`
 * next to `MiB/s`, never a bare `MB`. Two call sites handed the same number
 * cannot disagree because there is nowhere left for them to disagree.
 *
 * Progress events arrive half-formed — a total of zero before the response
 * headers land, a rate of `Infinity` on the first sample, a negative remainder
 * when a counter resets — so every function here is total and answers with
 * something a person can read rather than `NaN MiB`.
 */

export const BYTES_PER_MIB = 1024 * 1024;

/** The copy shown while the rate is too unstable to name a number. */
export const ETA_INDETERMINATE = 'Estimating time remaining…';

/**
 * Suffix ladder, smallest first, each with the precision that reads best at
 * that magnitude: whole bytes, one decimal through the middle of the range, and
 * two decimals once a single unit is large enough that one decimal would hide
 * hundreds of megabytes.
 */
const UNITS: ReadonlyArray<{ suffix: string; bytes: number; decimals: number }> = [
  { suffix: 'B', bytes: 1, decimals: 0 },
  { suffix: 'KiB', bytes: 1024, decimals: 1 },
  { suffix: 'MiB', bytes: 1024 ** 2, decimals: 1 },
  { suffix: 'GiB', bytes: 1024 ** 3, decimals: 2 },
  { suffix: 'TiB', bytes: 1024 ** 4, decimals: 2 },
];

const SECONDS_PER_MINUTE = 60;
const SECONDS_PER_HOUR = 60 * SECONDS_PER_MINUTE;

/** Anything that is not a usable non-negative number counts as nothing at all. */
function nonNegative(value: number): number {
  return Number.isFinite(value) && value > 0 ? value : 0;
}

export function mibToBytes(mib: number): number {
  return nonNegative(mib) * BYTES_PER_MIB;
}

/** "639.6 MiB", "2.55 GiB" — the single formatter for every size in the UI. */
export function formatBytes(bytes: number): string {
  const safe = nonNegative(bytes);

  let unit = UNITS[0];
  for (const candidate of UNITS) {
    if (safe >= candidate.bytes) {
      unit = candidate;
    }
  }

  // Bytes stay whole; larger units keep a fixed number of decimals so a live
  // counter does not shuffle its width from one tick to the next.
  return `${(safe / unit.bytes).toFixed(unit.decimals)} ${unit.suffix}`;
}

/** "12.4 MiB/s" — same base and suffix family as {@link formatBytes}. */
export function formatBytesPerSecond(bytesPerSecond: number): string {
  return `${formatBytes(bytesPerSecond)}/s`;
}

/** "about 4 min", "about 45 sec", "about 1 hr 20 min". */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds)) {
    return ETA_INDETERMINATE;
  }

  // A remaining count of zero or below means the transfer is effectively done,
  // and "about 0 sec" reads like a stuck clock, so the floor is one second.
  const total = Math.max(1, Math.round(seconds));

  if (total < SECONDS_PER_MINUTE) {
    return `about ${total} sec`;
  }

  if (total < SECONDS_PER_HOUR) {
    const minutes = Math.max(1, Math.round(total / SECONDS_PER_MINUTE));
    return minutes >= 60 ? 'about 1 hr' : `about ${minutes} min`;
  }

  const hours = Math.floor(total / SECONDS_PER_HOUR);
  const minutes = Math.round((total % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE);
  // Rounding the remainder can land on a full hour; carry it rather than
  // printing "about 1 hr 60 min".
  const carriedHours = minutes === 60 ? hours + 1 : hours;
  const remainingMinutes = minutes === 60 ? 0 : minutes;

  return remainingMinutes === 0
    ? `about ${carriedHours} hr`
    : `about ${carriedHours} hr ${remainingMinutes} min`;
}

/** {@link formatDuration}, or {@link ETA_INDETERMINATE} when there is no estimate yet. */
export function formatEta(seconds: number | null | undefined): string {
  return seconds === null || seconds === undefined ? ETA_INDETERMINATE : formatDuration(seconds);
}
