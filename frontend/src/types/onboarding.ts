export type OnboardingStep = 1 | 2 | 3 | 4 | 5;

export type PermissionStatus = 'checking' | 'not_determined' | 'authorized' | 'denied';

export interface OnboardingPermissions {
  microphone: PermissionStatus;
  systemAudio: PermissionStatus;
  screenRecording: PermissionStatus;
}

export interface OnboardingContainerProps {
  title: string;
  description?: React.ReactNode;
  children: React.ReactNode;
  step?: number;
  totalSteps?: number;
  stepOffset?: number;
  hideProgress?: boolean;
  className?: string;
  showNavigation?: boolean;
  onNext?: () => void;
  onPrevious?: () => void;
  canGoNext?: boolean;
  canGoPrevious?: boolean;
}

export interface PermissionRowProps {
  icon: React.ReactNode;
  title: string;
  description: string;
  status: PermissionStatus;
  isPending?: boolean;
  onAction: () => void;
}

export interface StatusIndicatorProps {
  status: 'idle' | 'checking' | 'success' | 'error';
  size?: 'sm' | 'md' | 'lg';
}

// The Rust side of this feature lives in frontend/src-tauri/src/audio/mic_check.rs (the model load
// and per-channel capture/classify logic) and frontend/src-tauri/src/onboarding.rs (persistence).
// SetupCheckResult, ModelReport and ChannelReport mirror #[serde(rename_all = "camelCase")] structs,
// so their fields are camelCase here. ChannelOutcome and ModelCheckOutcome mirror
// #[serde(rename_all = "snake_case")] enums. SetupCheckRecord is different: it hangs off
// OnboardingStatus, which carries no serde rename on the Rust side, so its keys stay snake_case on
// the wire. That split is deliberate, not an accident — match it exactly when reading from either.

export type SetupCheckChannel = 'microphone' | 'system_audio';

// 'command_failed' has no Rust counterpart: lib/micCheck.ts synthesizes it when invoke() itself
// throws, so the UI still has an outcome to branch on if the Tauri call never reaches the
// backend. Keep that comment — it is the same reason the old MicCheckOutcome carried one.
export type ChannelOutcome =
  | 'transcribed'
  | 'no_speech_detected'
  | 'no_audio_detected'
  | 'permission_denied'
  | 'device_unavailable'
  | 'transcription_failed'
  | 'cancelled'
  | 'unsupported'
  | 'not_run'
  | 'command_failed';

export type ModelCheckOutcome = 'loaded' | 'unavailable' | 'failed' | 'command_failed';

export interface ModelReport {
  outcome: ModelCheckOutcome;
  modelName: string | null;
  detail: string | null;
}

export interface ChannelReport {
  channel: SetupCheckChannel;
  outcome: ChannelOutcome;
  transcript: string | null;
  deviceName: string;
  peakLevel: number;
  durationMs: number;
  detail: string | null;
}

export interface SetupCheckResult {
  model: ModelReport;
  microphone: ChannelReport;
  systemAudio: ChannelReport;
  durationMs: number;
  cancelled: boolean;
}

export interface SetupCheckLevelEvent {
  channel: SetupCheckChannel;
  rms: number;
  peak: number;
  isActive: boolean;
  elapsedMs: number;
  durationMs: number;
}

// Persisted in onboarding-status.json. snake_case because OnboardingStatus carries no serde
// rename on the Rust side, unlike every other shape in this feature.
export type SetupCheckStatus = 'passed' | 'issues' | 'skipped';

export interface SetupCheckRecord {
  status: SetupCheckStatus;
  model: string;
  microphone: string;
  system_audio: string;
  checked_at: string;
}
