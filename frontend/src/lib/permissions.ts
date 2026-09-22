/**
 * The one place onboarding and the setup check both go to ask about a
 * permission, and the one place that opens a macOS privacy pane for either of
 * them.
 *
 * Before this module existed, the two onboarding call sites and the setup
 * check each built their own `invoke('open_system_settings', ...)` call, and
 * one of them left the argument off entirely (FR-04): Tauri's argument
 * deserialisation failed, the promise rejected, and the user landed on a raw
 * `window.alert()` instead of System Settings. Routing every caller through
 * `openPermissionSettings` means there is exactly one call to get right.
 *
 * The permission checks here are evidence-based on the Rust side —
 * `trigger_microphone_permission` and `trigger_system_audio_permission_command`
 * open the device and wait to actually observe an audio callback before they
 * report anything back (see FR-02/FR-03 in
 * docs/audits/2026-09-21-first-run-flow.md). This module does not add any
 * verification of its own; it only relays whatever `PermissionReport` the
 * backend resolved with, or turns a failed `invoke` into an `undetermined`
 * result so a broken Tauri call reads as "we don't know" rather than as a
 * fabricated "denied".
 */

import { invoke } from '@tauri-apps/api/core';
import type { PermissionReport, PermissionStatus, PermissionVerdict } from '@/types/onboarding';

/// The pane names open_system_settings understands, in one place.
export const PERMISSION_PANES = {
  microphone: 'Privacy_Microphone',
  systemAudio: 'Privacy_ScreenCapture',
} as const;

export type PermissionPane = (typeof PERMISSION_PANES)[keyof typeof PERMISSION_PANES];

/**
 * Mirrors `micCheck.ts`'s own `isMacOS()`: a dynamic import of
 * `@tauri-apps/plugin-os` so a non-Tauri context (tests, a stray web render)
 * does not crash, falling back to a userAgent sniff when the plugin itself is
 * unavailable. `open_system_settings` exists only on macOS, so every caller
 * that might invoke it needs this same check.
 */
export async function isMacOS(): Promise<boolean> {
  try {
    const { platform } = await import('@tauri-apps/plugin-os');
    return platform() === 'macos';
  } catch {
    return typeof navigator !== 'undefined' && navigator.userAgent.includes('Mac');
  }
}

/**
 * Opens a macOS privacy pane. Resolves false when it could not be opened —
 * off macOS, where there is nothing to open, or when the invoke itself
 * failed — so the caller can fall back to written instructions instead of
 * throwing in front of the user.
 *
 * The `{ preferencePane: pane }` argument object is the whole point of this
 * function: `open_system_settings` requires it, and a caller that builds the
 * invoke by hand is exactly how FR-04 happened the first time.
 */
export async function openPermissionSettings(pane: PermissionPane): Promise<boolean> {
  if (!(await isMacOS())) return false;

  try {
    await invoke('open_system_settings', { preferencePane: pane });
    return true;
  } catch (error) {
    console.error('Failed to open system settings:', error);
    return false;
  }
}

/**
 * Turns a failed invoke into the report shape the caller still needs. A
 * command that never reached the backend has told us nothing about the
 * permission itself, so the verdict is `undetermined`, not `denied` — the
 * same reasoning `startSetupCheck` uses for `command_failed`.
 */
function commandFailedReport(error: unknown): PermissionReport {
  console.error('Permission check could not be started:', error);
  const detail = error instanceof Error ? error.message : String(error);
  return { verdict: 'undetermined', detail };
}

/**
 * Asks the backend to verify the microphone by observing audio, and relays
 * whatever `PermissionReport` it resolves with. This function does not
 * itself verify anything — the evidence, such as it is, comes from
 * `trigger_microphone_permission` waiting on an audio callback.
 */
export async function checkMicrophonePermission(): Promise<PermissionReport> {
  try {
    return await invoke<PermissionReport>('trigger_microphone_permission');
  } catch (error) {
    return commandFailedReport(error);
  }
}

/**
 * Asks the backend to verify system audio the same way. `undetermined` here
 * is also exactly what a Mac with nothing playing produces — silence is not
 * evidence of a denial, so the backend does not report one, and neither does
 * this function.
 */
export async function checkSystemAudioPermission(): Promise<PermissionReport> {
  try {
    return await invoke<PermissionReport>('trigger_system_audio_permission_command');
  } catch (error) {
    return commandFailedReport(error);
  }
}

/**
 * The UI row state a wire verdict maps to. Pulled out to its own function —
 * rather than inlined at each of the row's call sites — so the mapping from
 * `PermissionVerdict` to `PermissionStatus` exists exactly once and cannot
 * drift between the microphone row and the system-audio row.
 */
export function verdictToStatus(verdict: PermissionVerdict): PermissionStatus {
  switch (verdict) {
    case 'authorized':
      return 'authorized';
    case 'denied':
      return 'denied';
    case 'undetermined':
      return 'undetermined';
  }
}
