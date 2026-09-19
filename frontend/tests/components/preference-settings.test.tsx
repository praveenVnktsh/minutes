import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { NotificationSettings } from '../../src/contexts/ConfigContext';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
const originalPlatform = { ...await import('../../src/hooks/usePlatform') };
const originalAnalytics = { ...await import('../../src/lib/analytics') };
const originalUpdate = { ...await import('../../src/components/UpdateSettings') };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
  mock.module('../../src/hooks/usePlatform', () => originalPlatform);
  mock.module('../../src/lib/analytics', () => originalAnalytics);
  mock.module('../../src/components/UpdateSettings', () => originalUpdate);
});

const notifications: NotificationSettings = {
  recording_notifications: true,
  time_based_reminders: true,
  meeting_reminders: true,
  respect_do_not_disturb: true,
  notification_sound: true,
  system_permission_granted: true,
  consent_given: true,
  manual_dnd_mode: false,
  notification_preferences: {
    show_recording_started: true, show_recording_stopped: true, show_recording_paused: true,
    show_recording_resumed: true, show_transcription_complete: true, show_meeting_reminders: true,
    show_system_errors: true, meeting_reminder_minutes: [5],
  },
};
const updateNotificationSettings = mock(async (_settings: NotificationSettings) => {});
const invoke = mock(async (command: string) => {
  if (command === 'get_global_shortcuts') return { recording: 'CmdOrCtrl+Shift+KeyR', window: 'CmdOrCtrl+Shift+KeyM' };
  if (command === 'set_global_shortcuts') throw new Error('shortcut registration failed');
  return null;
});
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('../../src/contexts/ConfigContext', () => ({
  useConfig: () => ({
    notificationSettings: notifications,
    storageLocations: { database: '/db', models: '/models', recordings: '/recordings' },
    isLoadingPreferences: false,
    loadPreferences: async () => {},
    updateNotificationSettings,
  }),
}));
mock.module('../../src/hooks/usePlatform', () => ({ usePlatform: () => 'macos' }));
mock.module('../../src/lib/analytics', () => ({ default: { track: async () => {} } }));
mock.module('../../src/components/UpdateSettings', () => ({ UpdateSettings: () => null }));

const { PreferenceSettings } = await import('../../src/components/PreferenceSettings');
let renderer: ReactTestRenderer;
const text = () => JSON.stringify(renderer.toJSON());

beforeEach(() => {
  updateNotificationSettings.mockReset();
  invoke.mockClear();
});
afterEach(() => renderer?.unmount());

async function renderSettings() {
  await act(async () => { renderer = create(<PreferenceSettings />); });
  await act(async () => { await Promise.resolve(); });
}

describe('preference autosave recovery', () => {
  test('rolls back recording OS notifications when persistence fails', async () => {
    updateNotificationSettings.mockImplementationOnce(async () => { throw new Error('database unavailable'); });
    await renderSettings();
    const toggle = renderer.root.findByProps({ 'aria-label': 'Show operating system notifications when recording starts and stops' });
    await act(async () => { await toggle.props.onCheckedChange(false); });

    expect(toggle.props.checked).toBe(true);
    expect(text()).toContain('previous setting restored');
  });

  test('restores a shortcut when native registration rejects the change', async () => {
    await renderSettings();
    const clear = renderer.root.findAllByProps({ title: 'Disable this shortcut' })[0];
    await act(async () => { clear.props.onClick(); await Promise.resolve(); });

    expect(text()).toContain('⌘⇧R');
    expect(text()).toContain('Could not save shortcut; previous shortcut restored');
  });
});
