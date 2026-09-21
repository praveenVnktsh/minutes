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

// 'command_failed' has no Rust counterpart: micCheck.ts synthesizes it when invoke() itself throws,
// so the UI still has an outcome to branch on even if the Tauri call never reaches the backend.
export type MicCheckOutcome =
  | 'transcribed'
  | 'no_speech_detected'
  | 'no_audio_detected'
  | 'permission_denied'
  | 'device_unavailable'
  | 'model_unavailable'
  | 'model_failed'
  | 'transcription_failed'
  | 'cancelled'
  | 'command_failed';

export interface MicCheckResult {
  outcome: MicCheckOutcome;
  transcript: string | null;
  deviceName: string;
  peakLevel: number;
  durationMs: number;
  detail: string | null;
}

export interface MicCheckLevelEvent {
  rms: number;
  peak: number;
  isActive: boolean;
  elapsedMs: number;
  durationMs: number;
}
