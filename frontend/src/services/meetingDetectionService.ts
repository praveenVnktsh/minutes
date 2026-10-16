import { invoke } from '@tauri-apps/api/core';

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
  setSettings: (settings: MeetingDetectionSettings) =>
    invoke<MeetingDetectionSettings>('set_meeting_detection_settings', { settings }),
  ignoreApp: (target: AudioApp) => invoke<MeetingDetectionSettings>('ignore_meeting_app', { target }),
  unignoreApp: (target: AudioApp) => invoke<MeetingDetectionSettings>('unignore_meeting_app', { target }),
  recentApps: () => invoke<AudioApp[]>('get_recent_audio_apps'),
  snooze: (minutes: number) => invoke<number>('snooze_meeting_detection', { minutes }),
  snoozedUntil: () => invoke<number | null>('get_meeting_detection_snooze'),
  resume: () => invoke<void>('resume_meeting_detection'),
};
