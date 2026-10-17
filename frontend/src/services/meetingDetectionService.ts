import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

/** An app capturing the microphone, as the Rust detector reports it. */
export interface AudioApp {
  name: string;
  bundle_id: string | null;
}

/** Mirrors MeetingDetectionSettings in src-tauri/src/meeting_detection.rs. */
export interface MeetingDetectionSettings {
  ignored_apps: AudioApp[];
  only_known_apps: boolean;
}

export { normalizeAppName } from '@/lib/meetingDetection';

export const meetingDetectionService = {
  getSettings: () => invoke<MeetingDetectionSettings>('get_meeting_detection_settings'),
  /** Changes only the "only known meeting apps" flag; the ignore list is left as Rust has it. */
  setOnlyKnownApps: (enabled: boolean) =>
    invoke<MeetingDetectionSettings>('set_only_known_meeting_apps', { enabled }),
  /** `source: 'prompt'` keeps the floating prompt on its own Undo view. */
  ignoreApp: (target: AudioApp, source: 'prompt' | 'settings' = 'settings') =>
    invoke<MeetingDetectionSettings>('ignore_meeting_app', { target, source }),
  unignoreApp: (target: AudioApp, source: 'prompt' | 'settings' = 'settings') =>
    invoke<MeetingDetectionSettings>('unignore_meeting_app', { target, source }),
  /** Fires whenever the ignore list, the toggle or the snooze changes anywhere. */
  onChanged: (handler: () => void) => listen('meeting-detection-changed', handler),
  recentApps: () => invoke<AudioApp[]>('get_recent_audio_apps'),
  snooze: (minutes: number) => invoke<number>('snooze_meeting_detection', { minutes }),
  snoozedUntil: () => invoke<number | null>('get_meeting_detection_snooze'),
  resume: () => invoke<void>('resume_meeting_detection'),
};
