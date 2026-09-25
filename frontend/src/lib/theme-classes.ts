/**
 * The app's theme, as the class recipes components share. Switching between
 * light and dark lives next door in lib/theme.ts.
 *
 * Colours are defined once, as CSS variables in app/globals.css (a light set on
 * :root and a dark set on .dark), and exposed as Tailwind tokens in
 * tailwind.config.js: surface-*, ink-*, hairline, brand, selected, focus, and a
 * DEFAULT + subtle pair for each tone (info, success, warning, error,
 * recording, paused). This module composes those tokens into the patterns the
 * UI repeats, so a status box or a selected card looks the same everywhere and
 * follows the theme.
 *
 * Use a recipe from here, or a token directly, instead of Tailwind's palette
 * classes (bg-red-50, text-blue-600, bg-white). Palette classes are fixed
 * colours: they don't switch with the theme and are what made settings render
 * light-blue in dark mode. Lint rejects them everywhere except this file and
 * the shadcn primitives in components/ui.
 */

export type Tone = 'neutral' | 'info' | 'success' | 'warning' | 'error';

/** A bordered box holding a message: an error, a note, a hint. */
export const panel: Record<Tone, string> = {
  neutral: 'border border-hairline bg-surface-2 text-ink',
  info: 'border border-info/30 bg-info-subtle text-info',
  success: 'border border-success/30 bg-success-subtle text-success',
  warning: 'border border-warning/30 bg-warning-subtle text-warning',
  error: 'border border-error/30 bg-error-subtle text-error',
};

/**
 * A small status pill ("Ready", "Corrupted"), or the round background behind a
 * status icon.
 */
export const badge: Record<Tone, string> = {
  neutral: 'bg-surface-2 text-ink-muted',
  info: 'bg-info-subtle text-info',
  success: 'bg-success-subtle text-success',
  warning: 'bg-warning-subtle text-warning',
  error: 'bg-error-subtle text-error',
};

/** Text or an icon that carries a status on its own, on a normal surface. */
export const toneText: Record<Tone, string> = {
  neutral: 'text-ink-muted',
  info: 'text-info',
  success: 'text-success',
  warning: 'text-warning',
  error: 'text-error',
};

/** A solid fill: status dots, level meters, confidence markers. */
export const toneFill: Record<Tone, string> = {
  neutral: 'bg-ink-subtle',
  info: 'bg-info',
  success: 'bg-success',
  warning: 'bg-warning',
  error: 'bg-error',
};

/** Something the user picks from a set. */
export const selection = {
  /** A card in a list of options, like the model cards in settings. */
  card: {
    idle: 'border-hairline',
    selected: 'border-ink ring-2 ring-ink',
  },
  /** A row or option inside a list or menu. */
  row: {
    idle: 'text-ink hover:bg-surface-2',
    selected: 'bg-selected text-selected-foreground',
  },
  /** The pill that labels the chosen card. */
  pill: 'bg-selected text-selected-foreground',
};

/** Determinate progress: download bars and the like. */
export const progress = {
  track: 'bg-hairline',
  fill: 'bg-ink',
};

/** An inline link inside running text. */
export const link = 'text-ink underline underline-offset-4 hover:text-ink-muted';

/** Keyboard focus on inputs that draw their own ring. */
export const focusRing = 'focus:outline-none focus:ring-2 focus:ring-focus';

/** The dimmed layer behind an overlay. Black in both themes on purpose. */
export const scrim = 'bg-black/60 backdrop-blur-sm';

/** Whether a recording is live or paused, for dots and indicators. */
export const recordingFill = {
  live: 'bg-recording',
  paused: 'bg-paused',
};

/**
 * Speaker labels in the transcript. These are the one place the app wants
 * more hues than the tones provide, so each entry carries its own light and
 * dark pair.
 */
export const speakerPalette = [
  'bg-blue-100 text-blue-700 dark:bg-blue-500/15 dark:text-blue-300',
  'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300',
  'bg-violet-100 text-violet-700 dark:bg-violet-500/15 dark:text-violet-300',
  'bg-amber-100 text-amber-800 dark:bg-amber-500/15 dark:text-amber-300',
  'bg-rose-100 text-rose-700 dark:bg-rose-500/15 dark:text-rose-300',
  'bg-cyan-100 text-cyan-700 dark:bg-cyan-500/15 dark:text-cyan-300',
  'bg-orange-100 text-orange-700 dark:bg-orange-500/15 dark:text-orange-300',
  'bg-pink-100 text-pink-700 dark:bg-pink-500/15 dark:text-pink-300',
] as const;

/** Highlights over transcript segments. */
export const transcriptHighlight = {
  /** The search match the user is on. */
  activeMatch: 'bg-warning/25 ring-1 ring-warning/60',
  /** A segment containing some other search match. */
  match: 'bg-warning/10',
  /** The matched text itself. */
  matchText: 'rounded bg-warning/40 px-0.5 text-inherit',
  /** The segment the audio player is on. */
  playing: 'bg-info/15 shadow-[inset_3px_0_0_0_rgb(var(--info-rgb))] ring-1 ring-inset ring-info/35',
};
