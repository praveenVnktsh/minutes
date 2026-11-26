import { describe, expect, test } from 'bun:test';
import { BETA_FEATURE_DESCRIPTIONS, BETA_FEATURE_NAMES, DEFAULT_BETA_FEATURES, parseBetaFeatures } from './betaFeatures';

describe('Linear & Notion people sync flag', () => {
  test('is off by default', () => {
    expect(DEFAULT_BETA_FEATURES.peopleIntegrations).toBe(false);
    expect(BETA_FEATURE_NAMES.peopleIntegrations).toBe('Linear & Notion people sync');
    expect(BETA_FEATURE_DESCRIPTIONS.peopleIntegrations).toContain('never meeting content');
  });

  test('settings saved before the flag existed load with it off', () => {
    const features = parseBetaFeatures({ liveTranscription: true, micEchoCancellation: true });
    expect(features.peopleIntegrations).toBe(false);
    expect(features.micEchoCancellation).toBe(true);
  });

  test('a saved choice is kept, and a malformed one falls back to off', () => {
    expect(parseBetaFeatures({ peopleIntegrations: true }).peopleIntegrations).toBe(true);
    expect(parseBetaFeatures({ peopleIntegrations: 'yes' }).peopleIntegrations).toBe(false);
  });
});
