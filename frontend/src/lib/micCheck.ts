/**
 * The microphone check that closes onboarding.
 *
 * This is the seam between the mic-check UI and the Rust commands in
 * `audio/mic_check.rs`: it starts a capture, subscribes to the level meter,
 * and — the part that matters — turns an outcome into wording that names what
 * actually went wrong and what to do about it. The whole point of the step is
 * that a new user finds out their microphone is muted or blocked here, in an
 * app that can explain it, rather than in their first real meeting, so a
 * generic "something went wrong" would be worse than showing nothing at all.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { MicCheckLevelEvent, MicCheckOutcome, MicCheckResult } from '@/types/onboarding';

/** How long the step records for. The Rust side clamps this to 3–15 seconds. */
export const MIC_CHECK_DURATION_SECONDS = 8;

const LEVEL_EVENT = 'mic-check-level';

/**
 * Listed rather than written as a bare union so the tests can walk every
 * outcome and prove each one has its own message. `satisfies` keeps a typo
 * here from quietly becoming an outcome nothing in the switch below answers.
 */
export const MIC_CHECK_OUTCOMES = [
  'transcribed',
  'no_speech_detected',
  'no_audio_detected',
  'permission_denied',
  'device_unavailable',
  'model_unavailable',
  'model_failed',
  'transcription_failed',
  'cancelled',
  'command_failed',
] as const satisfies readonly MicCheckOutcome[];

/**
 * The wire shapes live in `@/types/onboarding` beside the rest of onboarding's
 * types, and are re-exported here so the step can take the whole microphone
 * check from one module. `MicCheckLevelEvent` reads as `MicCheckLevel` at the
 * call site, where it is a level rather than an event.
 */
export type { MicCheckOutcome, MicCheckResult };
export type MicCheckLevel = MicCheckLevelEvent;

export interface StartMicCheckOptions {
  deviceName?: string | null;
  durationSecs?: number | null;
}

/**
 * What the user can do about an outcome. `open-mic-settings` is the only one
 * this module can carry out itself; the rest are the step's own buttons.
 */
export type MicCheckFixKind = 'retry' | 'choose-device' | 'open-mic-settings' | 'retry-download';

export interface MicCheckFix {
  label: string;
  kind: MicCheckFixKind;
}

export interface MicCheckDescription {
  title: string;
  message: string;
  fix?: MicCheckFix;
}

/** Stands in for a device we never got a name for. */
const UNNAMED_DEVICE = 'your microphone';

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function deviceLabel(result: MicCheckResult): string {
  return result.deviceName?.trim() || UNNAMED_DEVICE;
}

/**
 * Records from one input device and transcribes it, resolving with the verdict.
 *
 * This never rejects. A failed invoke — no Tauri runtime, a check already in
 * flight, a panic on the way — becomes a `command_failed` result, because the
 * step has a message and a retry for that too, and an unhandled rejection
 * would leave the user staring at a spinner instead.
 */
export async function startMicCheck({
  deviceName = null,
  durationSecs = MIC_CHECK_DURATION_SECONDS,
}: StartMicCheckOptions = {}): Promise<MicCheckResult> {
  try {
    return await invoke<MicCheckResult>('mic_check_start', { deviceName, durationSecs });
  } catch (error) {
    console.error('Microphone check could not be started:', error);
    return {
      outcome: 'command_failed',
      transcript: null,
      deviceName: deviceName ?? '',
      peakLevel: 0,
      durationMs: 0,
      detail: errorText(error),
    };
  }
}

/**
 * Stops a capture that is still running; the pending `startMicCheck` then
 * resolves as `cancelled`. Cancelling a check that already finished is not a
 * failure worth putting in front of anyone.
 */
export async function cancelMicCheck(): Promise<void> {
  try {
    await invoke('mic_check_cancel');
  } catch (error) {
    console.warn('Microphone check could not be cancelled:', error);
  }
}

/** Subscribes to the level meter for the duration of a capture. */
export function onMicCheckLevel(handler: (level: MicCheckLevel) => void): Promise<UnlistenFn> {
  return listen<MicCheckLevel>(LEVEL_EVENT, ({ payload }) => handler(payload));
}

/**
 * The wording for an outcome: what happened, in terms of the device the user
 * picked, and the one thing to try next.
 *
 * `result.detail` is deliberately left out — it carries raw error text for the
 * log and the details line, and the message has to stand on its own.
 */
export function describeMicCheckOutcome(result: MicCheckResult): MicCheckDescription {
  const device = deviceLabel(result);

  switch (result.outcome) {
    case 'transcribed':
      return {
        title: 'Your microphone works',
        message: `Minutes heard you through ${device} and turned it into text. That is everything a meeting needs.`,
      };

    case 'no_speech_detected':
      return {
        title: 'Sound, but no words',
        message: `Minutes heard sound from ${device} but could not make out any speech in it. Speak a little louder, or move closer to the microphone, and run the check again.`,
        fix: { label: 'Try again', kind: 'retry' },
      };

    case 'no_audio_detected':
      return {
        title: 'That microphone sent silence',
        message: `${device} is connected but sent nothing but silence. It is probably muted, or it is not the input you are actually speaking into.`,
        fix: { label: 'Choose a different microphone', kind: 'choose-device' },
      };

    case 'permission_denied':
      return {
        title: 'Minutes is blocked from the microphone',
        message: `Your operating system is withholding audio from ${device}, so Minutes recorded nothing at all. Switch Minutes on under System Settings › Privacy & Security › Microphone on macOS, or Settings › Privacy & security › Microphone on Windows, then run the check again.`,
        fix: { label: 'Open microphone settings', kind: 'open-mic-settings' },
      };

    case 'device_unavailable':
      return {
        title: 'That microphone could not be opened',
        message: `Minutes could not open ${device}. Another app may be holding it, or it may have been unplugged since you chose it.`,
        fix: { label: 'Choose a different microphone', kind: 'choose-device' },
      };

    case 'model_unavailable':
      return {
        title: 'The transcription model is missing',
        message:
          'Your recording was fine, but there is no transcription model on disk to turn it into text. Download the model again and the check has something to run.',
        fix: { label: 'Download the model again', kind: 'retry-download' },
      };

    case 'model_failed':
      return {
        title: 'The transcription model would not load',
        message:
          'The model is on disk but Minutes could not load it. The download may have been damaged, or this machine may have run short of memory.',
        fix: { label: 'Try again', kind: 'retry' },
      };

    case 'transcription_failed':
      return {
        title: 'Transcription failed partway through',
        message: `The model loaded and then errored while transcribing. Nothing is wrong with ${device} — the recording itself came through.`,
        fix: { label: 'Try again', kind: 'retry' },
      };

    case 'cancelled':
      return {
        title: 'Microphone check stopped',
        message:
          'You stopped the check before it finished. Nothing is wrong — run it again whenever you are ready to say a few words.',
        fix: { label: 'Start the check again', kind: 'retry' },
      };

    case 'command_failed':
      return {
        title: 'The check could not run',
        message:
          'Minutes could not start the microphone check at all, so neither your microphone nor the model has been tested yet.',
        fix: { label: 'Try again', kind: 'retry' },
      };
  }
}

async function isMacOS(): Promise<boolean> {
  try {
    const { platform } = await import('@tauri-apps/plugin-os');
    return platform() === 'macos';
  } catch {
    return typeof navigator !== 'undefined' && navigator.userAgent.includes('Mac');
  }
}

/**
 * Carries out the fixes this module can carry out itself, which today is
 * opening the microphone privacy pane on macOS. Returns false when the fix
 * could not be performed, so the caller can fall back to the written
 * instructions in the message; `open_system_settings` exists only on macOS.
 *
 * `choose-device`, `retry` and `retry-download` belong to the step's own UI.
 */
export async function applyMicCheckFix(kind: MicCheckFixKind): Promise<boolean> {
  if (kind !== 'open-mic-settings') return false;
  if (!(await isMacOS())) return false;

  try {
    await invoke('open_system_settings', { preferencePane: 'Privacy_Microphone' });
    return true;
  } catch (error) {
    console.error('Failed to open microphone settings:', error);
    return false;
  }
}
