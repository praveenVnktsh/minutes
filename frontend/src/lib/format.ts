/**
 * The app's number formatters in one place: sizes (re-exported from
 * download-display, binary units throughout) and clock times.
 */
export { formatBytes, formatBytesPerSecond, formatDuration, formatEta } from '@/lib/download-display';

export interface ClockOptions {
  /** Show hours as "h:mm:ss" once a time reaches an hour. Without it,
   * minutes keep counting past 59 ("75:03"). */
  hours?: boolean;
  /** Pad the hours to two digits ("01:02:03"); implies `hours`. */
  padHours?: boolean;
  /** Pad the minutes to two digits when there are no hours ("05:07").
   * Off gives "5:07". On by default. */
  padMinutes?: boolean;
}

const pad = (value: number) => String(value).padStart(2, '0');

/**
 * A recording-relative time as a clock: "05:07", or with `hours` "1:02:03".
 * Anything that is not a usable non-negative number reads as zero.
 */
export function formatClock(seconds: number, options: ClockOptions = {}): string {
  const { padHours = false, padMinutes = true } = options;
  const showHours = options.hours || padHours;
  const total = Number.isFinite(seconds) && seconds > 0 ? Math.floor(seconds) : 0;
  const hours = showHours ? Math.floor(total / 3600) : 0;
  const minutes = Math.floor((total - hours * 3600) / 60);
  const secs = total % 60;
  if (hours > 0) return `${padHours ? pad(hours) : hours}:${pad(minutes)}:${pad(secs)}`;
  return `${padMinutes ? pad(minutes) : minutes}:${pad(secs)}`;
}

/** "[05:07]" for a transcript line, or `empty` when the line has no time. */
export function bracketedClock(seconds: number | undefined, empty = '[--:--]', options?: ClockOptions): string {
  return seconds === undefined ? empty : `[${formatClock(seconds, options)}]`;
}
