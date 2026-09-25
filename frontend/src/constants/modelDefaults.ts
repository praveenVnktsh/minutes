/**
 * Default model names for transcription engines.
 * IMPORTANT: Keep in sync with Rust constants in src-tauri/src/config.rs
 */

/**
 * Default Whisper model for transcription when no preference is configured.
 * Turbo keeps nearly all of large-v3's accuracy at a fraction of the size and
 * decode time.
 */
export const DEFAULT_WHISPER_MODEL = 'large-v3-turbo-q5_0';

/**
 * Default Parakeet model for transcription when no preference is configured.
 * The app targets English, and v2 is the English-only Parakeet, which scores
 * better on English than the multilingual v3.
 */
export const DEFAULT_PARAKEET_MODEL = 'parakeet-tdt-0.6b-v2-int8';

/**
 * Model defaults by provider type
 */
export const MODEL_DEFAULTS = {
  whisper: DEFAULT_WHISPER_MODEL,
  localWhisper: DEFAULT_WHISPER_MODEL,
  parakeet: DEFAULT_PARAKEET_MODEL,
} as const;
