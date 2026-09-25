/**
 * Beta Features Type System
 *
 * This file defines the scalable architecture for managing beta features.
 *
 * ## Adding a New Beta Feature
 * 1. Add property to BetaFeatures interface
 * 2. Add default value in DEFAULT_BETA_FEATURES
 * 3. Add analytics mapping in BETA_FEATURE_ANALYTICS_MAP
 * 4. Add UI strings in BETA_FEATURE_NAMES and BETA_FEATURE_DESCRIPTIONS
 * 5. Use in components: `betaFeatures.yourFeatureName`
 *
 * ## Graduating a Feature to Stable
 * 1. Remove property from BetaFeatures interface
 * 2. TypeScript will error at all usage sites
 * 3. Remove conditional checks - feature is now always-on
 */

export interface BetaFeatures {
  /**
   * Import audio files and retranscribe existing meetings with different language settings
   * @since v0.3.0
   */
  importAndRetranscribe: boolean;
  /**
   * Toggle live transcription on/off during recording to save CPU/GPU resources
   * @since v0.3.0
   */
  liveTranscription: boolean;
  /**
   * Remove laptop-speaker echo from the microphone before live transcription,
   * so the remote side is not transcribed a second time as "mic"
   * @since v1.5
   */
  micEchoCancellation: boolean;
}

export const DEFAULT_BETA_FEATURES: BetaFeatures = {
  importAndRetranscribe: true, // Default: enabled
  liveTranscription: true, // Deferred transcription mode is available by default
  micEchoCancellation: false, // Opt-in until it has been proven on real calls
};


/**
 * Human-readable feature names for UI display
 */
export const BETA_FEATURE_NAMES: Record<keyof BetaFeatures, string> = {
  importAndRetranscribe: 'Import Audio & Retranscribe',
  liveTranscription: 'Live Transcription Toggle',
  micEchoCancellation: 'Mic Echo Cancellation',
};

/**
 * Feature descriptions for UI tooltips/help text
 */
export const BETA_FEATURE_DESCRIPTIONS: Record<keyof BetaFeatures, string> = {
  importAndRetranscribe: 'Import audio files to transcribe or retranscribe existing meetings with different language settings.',
  liveTranscription: 'Show a toggle to enable/disable live transcription during recording. When off, audio is still recorded but transcription is skipped to save resources.',
  micEchoCancellation: 'When the other side plays through your speakers, remove their voice from the microphone before transcribing, so it is not transcribed twice. Takes effect from the next recording.',
};

/**
 * Type-safe feature key union
 * This ensures only valid feature keys can be used
 */
export type BetaFeatureKey = keyof BetaFeatures;

/**
 * Load beta features from localStorage
 *
 * @returns BetaFeatures object with values from localStorage or defaults
 */
export function loadBetaFeatures(): BetaFeatures {
  if (typeof window === 'undefined') {
    return { ...DEFAULT_BETA_FEATURES };
  }

  try {
    const saved = localStorage.getItem('betaFeatures');
    if (saved) {
      const parsed = JSON.parse(saved) as Partial<BetaFeatures>;
      // Merge with defaults to handle missing keys (graceful degradation)
      return { ...DEFAULT_BETA_FEATURES, ...parsed };
    }
  } catch (error) {
    console.error('[BetaFeatures] Failed to load from localStorage:', error);
  }

  return { ...DEFAULT_BETA_FEATURES };
}

/**
 * Save beta features to localStorage
 *
 * @param features - BetaFeatures object to save
 */
export function saveBetaFeatures(features: BetaFeatures): void {
  if (typeof window === 'undefined') return;

  try {
    localStorage.setItem('betaFeatures', JSON.stringify(features));
  } catch (error) {
    console.error('[BetaFeatures] Failed to save to localStorage:', error);
  }
}
