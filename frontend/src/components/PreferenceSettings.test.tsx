import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'

// Restore the real modules afterwards so these mocks cannot leak into a later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) }
const originalConfig = { ...(await import('@/contexts/ConfigContext')) }
const originalPlatform = { ...(await import('@/hooks/usePlatform')) }
const originalUpdateSettings = { ...(await import('@/components/UpdateSettings')) }
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@/contexts/ConfigContext', () => originalConfig)
  mock.module('@/hooks/usePlatform', () => originalPlatform)
  mock.module('@/components/UpdateSettings', () => originalUpdateSettings)
})

// The shortcuts backend this file exercises; 'pause' is left out of the initial reply so the
// component's tolerance of an older backend (no `pause` field) is actually under test.
let shortcutsBackend: { recording: string; window: string; pause?: string } = {
  recording: 'CmdOrCtrl+Shift+KeyR',
  window: 'CmdOrCtrl+Shift+KeyM',
}
let setShortcutsCalls: Array<{ recording: string; window: string; pause: string }> = []

mock.module('@tauri-apps/api/core', () => ({
  ...originalCore,
  invoke: async (command: string, args?: Record<string, unknown>) => {
    if (command === 'get_global_shortcuts') return shortcutsBackend
    if (command === 'set_global_shortcuts') {
      const call = args as { recording: string; window: string; pause: string }
      setShortcutsCalls.push(call)
      return undefined
    }
    return undefined
  },
}))
mock.module('@/contexts/ConfigContext', () => ({
  ...originalConfig,
  useConfig: () => ({
    notificationSettings: {
      recording_notifications: true,
      time_based_reminders: true,
      meeting_reminders: true,
      respect_do_not_disturb: true,
      notification_sound: true,
      system_permission_granted: true,
      consent_given: true,
      manual_dnd_mode: false,
      notification_preferences: {
        show_recording_started: true,
        show_recording_stopped: true,
        show_recording_paused: true,
        show_recording_resumed: true,
        show_transcription_complete: true,
        show_meeting_reminders: true,
        show_system_errors: true,
        meeting_reminder_minutes: [],
      },
    },
    storageLocations: { database: '/db', models: '/models', recordings: '/recordings' },
    isLoadingPreferences: false,
    loadPreferences: async () => {},
    updateNotificationSettings: async () => {},
  }),
}))
mock.module('@/hooks/usePlatform', () => ({
  ...originalPlatform,
  usePlatform: () => 'linux',
}))
// Stubbed so this file tests the shortcuts list PreferenceSettings owns, not update-check wiring,
// which has no bearing on the pause shortcut and would otherwise need its own provider mocked.
mock.module('@/components/UpdateSettings', () => ({
  UpdateSettings: () => null,
}))

const { PreferenceSettings } = await import('./PreferenceSettings')

describe('PreferenceSettings keyboard shortcuts', () => {
  beforeEach(() => {
    shortcutsBackend = { recording: 'CmdOrCtrl+Shift+KeyR', window: 'CmdOrCtrl+Shift+KeyM' }
    setShortcutsCalls = []
  })

  test('lists a pause/resume row and tolerates a backend reply with no pause field', async () => {
    let renderer!: ReturnType<typeof create>
    await act(async () => {
      renderer = create(<PreferenceSettings />)
    })

    const pauseButton = renderer.root.findByProps({ 'aria-label': 'Change shortcut for Pause / resume recording' })
    // The backend omitted `pause`; the component must treat that as disabled rather than crash
    // or show a stale/undefined label.
    expect(pauseButton.children).toEqual(['Not set'])
    expect(
      renderer.root.findAll((node) => node.type === 'span' && node.props.children === 'Pause / resume recording').length,
    ).toBe(1)
  })

  test('clearing the pause shortcut sends the other two shortcuts unchanged', async () => {
    shortcutsBackend = { recording: 'CmdOrCtrl+Shift+KeyR', window: 'CmdOrCtrl+Shift+KeyM', pause: 'CmdOrCtrl+Shift+KeyP' }
    let renderer!: ReturnType<typeof create>
    await act(async () => {
      renderer = create(<PreferenceSettings />)
    })

    const clearButton = renderer.root.findByProps({ 'aria-label': 'Disable shortcut for Pause / resume recording' })
    await act(async () => {
      clearButton.props.onClick()
    })

    expect(setShortcutsCalls).toEqual([
      { recording: 'CmdOrCtrl+Shift+KeyR', window: 'CmdOrCtrl+Shift+KeyM', pause: '' },
    ])
  })
})
