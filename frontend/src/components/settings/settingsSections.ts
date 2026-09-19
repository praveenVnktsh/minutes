export const SETTINGS_SECTIONS = [
  'general',
  'recording',
  'transcription',
  'summary',
  'integrations',
  'beta',
] as const;

export type SettingsSection = typeof SETTINGS_SECTIONS[number];

export function isSettingsSection(value: string | null): value is SettingsSection {
  return SETTINGS_SECTIONS.includes(value as SettingsSection);
}

/** Canonical deep link for setup and error recovery surfaces. */
export function settingsHref(section: SettingsSection): string {
  return `/settings?section=${section}`;
}

export function resolveSettingsSection(value: string | null): SettingsSection {
  return isSettingsSection(value) ? value : 'general';
}
