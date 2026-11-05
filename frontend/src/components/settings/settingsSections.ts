export const SETTINGS_SECTIONS = [
  'general',
  'recording',
  'transcription',
  'summary',
  'integrations',
  'developer',
] as const;

export type SettingsSection = typeof SETTINGS_SECTIONS[number];

/** Old section ids that still open a tab: Beta was merged into Labs & Developer. */
const SETTINGS_SECTION_ALIASES: Readonly<Record<string, SettingsSection>> = {
  beta: 'developer',
};

/** Whether a `?section=` value opens a tab (a current id or an old alias). */
export function isSettingsSection(value: string | null): boolean {
  if (value === null) return false;
  return SETTINGS_SECTIONS.includes(value as SettingsSection) || Object.hasOwn(SETTINGS_SECTION_ALIASES, value);
}

/** Canonical deep link for setup and error recovery surfaces. */
export function settingsHref(section: SettingsSection): string {
  return `/settings?section=${section}`;
}

/** The tab a `?section=` value opens: aliases map to their tab, anything else to General. */
export function resolveSettingsSection(value: string | null): SettingsSection {
  if (value === null) return 'general';
  if (SETTINGS_SECTIONS.includes(value as SettingsSection)) return value as SettingsSection;
  return Object.hasOwn(SETTINGS_SECTION_ALIASES, value) ? SETTINGS_SECTION_ALIASES[value] : 'general';
}
