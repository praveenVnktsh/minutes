import { describe, expect, test } from 'bun:test';
import { isSettingsSection, resolveSettingsSection, SETTINGS_SECTIONS, settingsHref } from './settingsSections';

describe('settings sections', () => {
  test('has six tabs, with Labs & Developer under the developer id', () => {
    expect(SETTINGS_SECTIONS).toEqual(['general', 'recording', 'transcription', 'summary', 'integrations', 'developer']);
    expect(settingsHref('developer')).toBe('/settings?section=developer');
  });

  test('old beta deep links land on Labs & Developer', () => {
    expect(isSettingsSection('beta')).toBe(true);
    expect(resolveSettingsSection('beta')).toBe('developer');
  });

  test('unknown sections fall back to General', () => {
    expect(isSettingsSection('nope')).toBe(false);
    expect(isSettingsSection(null)).toBe(false);
    expect(isSettingsSection('toString')).toBe(false);
    expect(resolveSettingsSection('toString')).toBe('general');
    expect(resolveSettingsSection(null)).toBe('general');
    expect(resolveSettingsSection('recording')).toBe('recording');
  });
});
