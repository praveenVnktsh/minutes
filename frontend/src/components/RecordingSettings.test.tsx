import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'

// Restore the real modules afterwards so these mocks cannot leak into a later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) }
const originalConfig = { ...(await import('@/contexts/ConfigContext')) }
const originalRecordingState = { ...(await import('@/contexts/RecordingStateContext')) }
const originalPanel = { ...(await import('@/components/SetupCheckPanel')) }
const originalDeviceSelection = { ...(await import('@/components/DeviceSelection')) }
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@/contexts/ConfigContext', () => originalConfig)
  mock.module('@/contexts/RecordingStateContext', () => originalRecordingState)
  mock.module('@/components/SetupCheckPanel', () => originalPanel)
  mock.module('@/components/DeviceSelection', () => originalDeviceSelection)
})

let selectedDeviceWrites: Array<{ micDevice: string | null; systemDevice: string | null }> = []
let savedPreferences: Array<Record<string, unknown>> = []

mock.module('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args?: { preferences?: Record<string, unknown> }) => {
    if (command === 'set_recording_preferences' && args?.preferences) {
      savedPreferences.push(args.preferences)
      return undefined
    }
    if (command === 'get_recording_preferences') {
      return {
        save_folder: '/recordings',
        auto_save: true,
        file_format: 'mp4',
        automatic_record_prompt: true,
        min_meeting_duration_seconds: 10,
        preferred_mic_device: null,
        preferred_system_device: null,
      }
    }
    if (command === 'get_onboarding_status') return null
    return undefined
  },
}))
mock.module('@/contexts/ConfigContext', () => ({
  ...originalConfig,
  useConfig: () => ({
    setSelectedDevices: (devices: never) => selectedDeviceWrites.push(devices),
    betaFeatures: { liveTranscription: true },
    toggleBetaFeature: () => {},
  }),
}))
mock.module('@/contexts/RecordingStateContext', () => ({
  ...originalRecordingState,
  useRecordingState: () => ({ isRecording: false }),
}))
mock.module('@tauri-apps/plugin-store', () => ({
  Store: { load: async () => ({ get: async () => true, set: async () => {}, save: async () => {} }) },
}))
mock.module('@/components/DeviceSelection', () => ({
  ...originalDeviceSelection,
  DeviceSelection: () => null,
}))

// Stubbed so this file tests the wiring RecordingSettings owns — what it hands the panel — rather
// than re-testing the panel, which has its own file.
let panelProps: Record<string, unknown> = {}
mock.module('@/components/SetupCheckPanel', () => ({
  SetupCheckPanel: (props: Record<string, unknown>) => {
    panelProps = props
    return null
  },
}))

const { RecordingSettings } = await import('./RecordingSettings')

describe('RecordingSettings and the setup check', () => {
  beforeEach(() => {
    selectedDeviceWrites = []
    savedPreferences = []
    panelProps = {}
  })

  test('the menu bar recording indicator is off unless switched on, and the switch saves it', async () => {
    let renderer!: ReturnType<typeof create>
    await act(async () => {
      renderer = create(<RecordingSettings />)
    })
    const toggle = renderer.root.find(
      (node) => node.props['aria-label'] === 'Show the recording indicator in the menu bar' && typeof node.props.onCheckedChange === 'function',
    )
    expect(Boolean(toggle.props.checked)).toBe(false)
    await act(async () => {
      await toggle.props.onCheckedChange(true)
    })
    expect(savedPreferences.at(-1)?.show_menu_bar_indicator).toBe(true)
    await act(async () => renderer.unmount())
  })

  test('a device chosen inside the check reaches the in-memory selection the start path reads', async () => {
    let renderer!: ReturnType<typeof create>
    await act(async () => {
      renderer = create(<RecordingSettings />)
    })

    // Reveal the panel; it is collapsed until asked for, so settings never grabs the microphone.
    const toggle = renderer.root.findAll(
      (node) =>
        node.type === 'button' &&
        typeof node.props.children === 'string' &&
        node.props.children.includes('the check'),
    )[0]
    await act(async () => {
      toggle.props.onClick()
    })

    expect(typeof panelProps.onDevicesChanged).toBe('function')

    await act(async () => {
      ;(panelProps.onDevicesChanged as (devices: unknown) => void)({
        micDevice: 'Scarlett Solo (input)',
        systemDevice: 'BlackHole 2ch (output)',
      })
    })

    // Writing the preference to disk is not enough: ConfigContext is loaded once at app mount and
    // the start path sends what it holds, so without this the next meeting opens a device the
    // check never tested.
    expect(selectedDeviceWrites).toEqual([
      { micDevice: 'Scarlett Solo (input)', systemDevice: 'BlackHole 2ch (output)' },
    ])
  })

  test('groups the tab into Audio, Meeting detection and Devices, with the capture backend under Advanced', async () => {
    let renderer!: ReturnType<typeof create>
    await act(async () => {
      renderer = create(<RecordingSettings />)
    })
    const sections = renderer.root.findAll((node) => node.type === 'section').map((node) => node.props['aria-label'])
    expect(sections).toEqual(['Audio', 'Meeting detection', 'Devices'])
    const advanced = renderer.root.findByType('details')
    expect(JSON.stringify(renderer.toJSON())).toContain('Advanced')
    // Folded away, and not even mounted, until opened.
    expect(advanced.props.open).toBeUndefined()
    expect(renderer.root.findAll((node) => (node.type as { name?: string }).name === 'AudioBackendSelector')).toHaveLength(0)
    await act(async () => renderer.unmount())
  })
})
