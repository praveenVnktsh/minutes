/**
 * The setup check that closes onboarding.
 *
 * This is the seam between the setup-check UI and the Rust commands in
 * `audio/mic_check.rs`: it loads the model, starts one capture window across
 * both channels, subscribes to the level meters, and — the part that matters —
 * turns an outcome into wording that names what actually went wrong and what to
 * do about it. The whole point of the step is that a new user finds out their
 * microphone is muted or blocked here, in an app that can explain it, rather
 * than in their first real meeting, so a generic "something went wrong" would be
 * worse than showing nothing at all.
 *
 * The check reports three things separately — the model, the microphone, and
 * the system audio — and never rolls them into one verdict. A Mac with no
 * meeting running is supposed to end with a working microphone and a silent
 * system-audio channel, and that pair has to read as one good result and one
 * neutral one, so every message below is written for a single channel and says
 * nothing about the other.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { openPermissionSettings, PERMISSION_PANES, type PermissionPane } from '@/lib/permissions';
import type {
  ChannelOutcome,
  ChannelReport,
  ModelCheckOutcome,
  ModelReport,
  SetupCheckChannel,
  SetupCheckLevelEvent,
  SetupCheckRecord,
  SetupCheckResult,
} from '@/types/onboarding';

/** How long the step records for. The Rust side clamps this to 3–15 seconds. */
export const SETUP_CHECK_DURATION_SECONDS = 8;

const LEVEL_EVENT = 'setup-check-level';

/**
 * Listed rather than written as bare unions so the tests can walk every outcome
 * and prove each one has its own message, on each channel. `satisfies` keeps a
 * typo here from quietly becoming an outcome nothing in the switches below
 * answers.
 */
export const CHANNEL_OUTCOMES = [
  'transcribed',
  'no_speech_detected',
  'no_audio_detected',
  'permission_denied',
  'device_unavailable',
  'transcription_failed',
  'cancelled',
  'unsupported',
  'not_run',
  'command_failed',
] as const satisfies readonly ChannelOutcome[];

export const MODEL_CHECK_OUTCOMES = [
  'loaded',
  'unavailable',
  'failed',
  'command_failed',
] as const satisfies readonly ModelCheckOutcome[];

export const SETUP_CHECK_CHANNELS = [
  'microphone',
  'system_audio',
] as const satisfies readonly SetupCheckChannel[];

/**
 * The wire shapes live in `@/types/onboarding` beside the rest of onboarding's
 * types, and are re-exported here so the step can take the whole setup check
 * from one module. `SetupCheckLevelEvent` reads as `SetupCheckLevel` at the call
 * site, where it is a level rather than an event.
 */
export type {
  ChannelOutcome,
  ChannelReport,
  ModelCheckOutcome,
  ModelReport,
  SetupCheckChannel,
  SetupCheckRecord,
  SetupCheckResult,
};
export type SetupCheckLevel = SetupCheckLevelEvent;

export interface StartSetupCheckOptions {
  micDeviceName?: string | null;
  systemDeviceName?: string | null;
  durationSecs?: number | null;
}

/**
 * What the user can do about an outcome. The two settings fixes are the only
 * ones this module can carry out itself; the rest are the step's own buttons.
 */
export type SetupCheckFixKind =
  | 'retry'
  | 'choose-device'
  | 'open-mic-settings'
  | 'open-screen-recording-settings'
  | 'retry-download';

export interface SetupCheckFix {
  label: string;
  kind: SetupCheckFixKind;
}

export interface SetupCheckDescription {
  title: string;
  message: string;
  fix?: SetupCheckFix;
}

/** Stand-ins for a channel we never got a device name for. */
const UNNAMED_MICROPHONE = 'your microphone';
const UNNAMED_SYSTEM_DEVICE = 'your system audio';
const UNNAMED_MODEL = 'the transcription model';

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function deviceLabel(report: ChannelReport): string {
  const fallback = report.channel === 'microphone' ? UNNAMED_MICROPHONE : UNNAMED_SYSTEM_DEVICE;
  return report.deviceName?.trim() || fallback;
}

function failedChannel(
  channel: SetupCheckChannel,
  deviceName: string,
  detail: string,
): ChannelReport {
  return {
    channel,
    outcome: 'command_failed',
    transcript: null,
    deviceName,
    peakLevel: 0,
    durationMs: 0,
    detail,
  };
}

/**
 * Runs the whole check — model first, then both channels over one capture
 * window — and resolves with the per-channel verdicts.
 *
 * This never rejects. A failed invoke — no Tauri runtime, a check already in
 * flight, a panic on the way — becomes a `command_failed` result on the model
 * and on both channels, because the step has a message and a retry for that
 * too, and an unhandled rejection would leave the user staring at a spinner
 * instead.
 */
export async function startSetupCheck({
  micDeviceName = null,
  systemDeviceName = null,
  durationSecs = SETUP_CHECK_DURATION_SECONDS,
}: StartSetupCheckOptions = {}): Promise<SetupCheckResult> {
  try {
    return await invoke<SetupCheckResult>('mic_check_start', {
      micDeviceName,
      systemDeviceName,
      durationSecs,
    });
  } catch (error) {
    console.error('Setup check could not be started:', error);
    const detail = errorText(error);
    return {
      model: { outcome: 'command_failed', modelName: null, detail },
      microphone: failedChannel('microphone', micDeviceName ?? '', detail),
      systemAudio: failedChannel('system_audio', systemDeviceName ?? '', detail),
      durationMs: 0,
      cancelled: false,
    };
  }
}

/**
 * Stops a capture that is still running; the pending `startSetupCheck` then
 * resolves with both channels `cancelled`. Cancelling a check that already
 * finished is not a failure worth putting in front of anyone.
 */
export async function cancelSetupCheck(): Promise<void> {
  try {
    await invoke('mic_check_cancel');
  } catch (error) {
    console.warn('Setup check could not be cancelled:', error);
  }
}

/**
 * Subscribes to the level meters for the duration of a capture. Both channels
 * arrive on the one event, each payload naming its own `channel`, so the caller
 * decides which meter a level belongs to.
 */
export function onSetupCheckLevel(handler: (level: SetupCheckLevel) => void): Promise<UnlistenFn> {
  return listen<SetupCheckLevel>(LEVEL_EVENT, ({ payload }) => handler(payload));
}

/**
 * The wording for the model step: whether Minutes has something to turn speech
 * into text with, and the one thing to try next when it does not.
 *
 * A model that did not load is the reason both channels come back untested, so
 * it gets its own card and its own fix rather than a footnote on a microphone
 * result. `report.detail` is deliberately left out — it carries raw error text
 * for the log and the details line, and the message has to stand on its own.
 */
export function describeModelOutcome(report: ModelReport): SetupCheckDescription {
  const model = report.modelName?.trim() || UNNAMED_MODEL;

  switch (report.outcome) {
    case 'loaded':
      return {
        title: 'The transcription model is ready',
        message: `Minutes loaded ${model} and it is ready to turn speech into text.`,
      };

    case 'unavailable':
      return {
        title: 'The transcription model is missing',
        message:
          'There is no transcription model on disk, so Minutes has nothing to turn speech into text with. Download the model again and the check has something to run.',
        fix: { label: 'Download the model again', kind: 'retry-download' },
      };

    case 'failed':
      return {
        title: 'The transcription model would not load',
        message:
          'The model is on disk but Minutes could not load it. The download may have been damaged, or this machine may have run short of memory.',
        fix: { label: 'Try again', kind: 'retry' },
      };

    case 'command_failed':
      return {
        title: 'The check could not run',
        message:
          'Minutes could not start the setup check at all, so the transcription model has not been tested yet.',
        fix: { label: 'Try again', kind: 'retry' },
      };
  }
}

/**
 * The wording for one channel's outcome: what happened, in terms of the device
 * that channel actually opened, and the one thing to try next.
 *
 * The same outcome means different things on the two channels, and that is what
 * this function exists for. Silence from a microphone is a muted device the user
 * should fix; silence from the speakers is a computer with nothing playing,
 * which is the normal result of running this check on a quiet machine and must
 * never be dressed up as a failure.
 */
export function describeChannelOutcome(report: ChannelReport): SetupCheckDescription {
  return report.channel === 'microphone'
    ? describeMicrophoneOutcome(report)
    : describeSystemAudioOutcome(report);
}

function describeMicrophoneOutcome(report: ChannelReport): SetupCheckDescription {
  const device = deviceLabel(report);

  switch (report.outcome) {
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

    case 'unsupported':
      return {
        title: 'Minutes cannot record a microphone here',
        message:
          'This computer offers Minutes no way to record a microphone, so there was nothing to test. Minutes will still take a meeting you have a recording of.',
      };

    case 'not_run':
      return {
        title: 'Your microphone was not tested',
        message:
          'The transcription model never loaded, so Minutes stopped before recording anything. Sort the model out above and the check will run from the top.',
      };

    case 'command_failed':
      return {
        title: 'The check could not run',
        message:
          'Minutes could not start the setup check at all, so your microphone has not been tested yet.',
        fix: { label: 'Try again', kind: 'retry' },
      };
  }
}

function describeSystemAudioOutcome(report: ChannelReport): SetupCheckDescription {
  const device = deviceLabel(report);

  switch (report.outcome) {
    case 'transcribed':
      return {
        title: 'Minutes can hear your meetings',
        message: `Minutes captured what was playing through ${device} and turned it into text, so everyone else on a call gets transcribed too.`,
      };

    case 'no_speech_detected':
      return {
        title: 'Audio, but no words',
        message: `Minutes captured sound from ${device} but could not make out any speech in it. Music and background noise read like this; play something with talking in it and run the check again.`,
        fix: { label: 'Try again', kind: 'retry' },
      };

    // Not a failure. A computer with nothing playing is supposed to sound like
    // this, and the check runs before the user has a meeting open, so this is
    // the ordinary result. It says what was heard, then — only for the user who
    // did expect audio — where system audio actually comes from.
    case 'no_audio_detected':
      return {
        title: 'Nothing was playing',
        message: `Minutes listened to ${device} and your computer was quiet, which is exactly what a machine with nothing playing sounds like. If you did expect to hear something, check how system audio reaches Minutes: screen recording permission on macOS, or the loopback device on Windows and Linux.`,
      };

    case 'permission_denied':
      return {
        title: 'Minutes is blocked from system audio',
        message: `macOS withholds the audio your computer is playing until Minutes is allowed to record the screen, so ${device} captured nothing. Switch Minutes on under System Settings › Privacy & Security › Screen Recording, then run the check again.`,
        fix: { label: 'Open screen recording settings', kind: 'open-screen-recording-settings' },
      };

    case 'device_unavailable':
      return {
        title: 'System audio could not be captured',
        message: `Minutes could not open ${device}. The loopback or virtual audio device that system audio travels through may be switched off, missing, or held by another app.`,
        fix: { label: 'Choose a different system audio device', kind: 'choose-device' },
      };

    case 'transcription_failed':
      return {
        title: 'System audio failed to transcribe',
        message: `The model loaded and then errored while transcribing what ${device} captured. The capture itself came through.`,
        fix: { label: 'Try again', kind: 'retry' },
      };

    case 'cancelled':
      return {
        title: 'System audio check stopped',
        message:
          'You stopped the check before Minutes had listened to what your computer is playing. Nothing is wrong — run it again with something playing whenever you are ready.',
        fix: { label: 'Start the check again', kind: 'retry' },
      };

    // No fix button: there is nothing here the user can go and switch on.
    case 'unsupported':
      return {
        title: 'Minutes cannot capture the other side of a call here',
        message:
          'This computer gives Minutes no route to what your speakers are playing yet, so only what your microphone picks up will be transcribed. Your microphone is unaffected.',
      };

    case 'not_run':
      return {
        title: 'System audio was not tested',
        message:
          'The transcription model never loaded, so Minutes stopped before listening to your computer. Sort the model out above and the check will run from the top.',
      };

    case 'command_failed':
      return {
        title: 'The check never started',
        message:
          'Minutes could not start the setup check at all, so what your computer plays has not been tested yet.',
        fix: { label: 'Try again', kind: 'retry' },
      };
  }
}

/**
 * The record kept in onboarding-status.json, so a later support conversation
 * can see how the machine looked on the first run.
 *
 * A run passes when the model loaded and the microphone came back as words.
 * Silent system audio deliberately does not spoil it: on a quiet Mac that is the
 * expected outcome, and a status that went yellow every time would teach the
 * user to ignore the status.
 */
export function summariseSetupCheck(result: SetupCheckResult): SetupCheckRecord {
  const passed = result.model.outcome === 'loaded' && result.microphone.outcome === 'transcribed';

  return {
    status: passed ? 'passed' : 'issues',
    model: result.model.outcome,
    microphone: result.microphone.outcome,
    system_audio: result.systemAudio.outcome,
    checked_at: new Date().toISOString(),
  };
}

/**
 * The record kept when the user walks past the check. Whatever the run did
 * manage to find out is still worth keeping — a skipped check that had already
 * seen a denied microphone explains a later silent recording.
 */
export function skippedSetupCheckRecord(result?: SetupCheckResult | null): SetupCheckRecord {
  return {
    status: 'skipped',
    model: result?.model.outcome ?? 'not_run',
    microphone: result?.microphone.outcome ?? 'not_run',
    system_audio: result?.systemAudio.outcome ?? 'not_run',
    checked_at: new Date().toISOString(),
  };
}

/**
 * Which fix opens which pane. The pane names themselves live in
 * `@/lib/permissions` (`PERMISSION_PANES`) — the first-run audit (FR-04) found
 * that the onboarding copies of this call left the argument off entirely and
 * so could never have opened anything, and having two files each spell out
 * `'Privacy_Microphone'` is exactly how that kind of drift happens again. This
 * mapping from `SetupCheckFixKind` to a pane is still the setup check's own
 * business, so it stays here.
 */
const SETTINGS_PANES: Partial<Record<SetupCheckFixKind, PermissionPane>> = {
  'open-mic-settings': PERMISSION_PANES.microphone,
  'open-screen-recording-settings': PERMISSION_PANES.systemAudio,
};

/**
 * Carries out the fixes this module can carry out itself, which are the two
 * privacy panes on macOS. Returns false when the fix could not be performed, so
 * the caller can fall back to the written instructions in the message.
 *
 * The actual opening — the macOS check, the `invoke`, and turning a rejection
 * into `false` — is `openPermissionSettings` from `@/lib/permissions`, the one
 * place that call is made (FR-04). This function's own job is just picking the
 * pane for a fix.
 *
 * `choose-device`, `retry` and `retry-download` belong to the step's own UI.
 */
export async function applySetupCheckFix(kind: SetupCheckFixKind): Promise<boolean> {
  const pane = SETTINGS_PANES[kind];
  if (!pane) return false;
  return openPermissionSettings(pane);
}
