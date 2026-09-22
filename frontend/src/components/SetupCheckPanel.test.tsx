import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'

// Restore the real modules afterwards so these mocks cannot leak into a later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) }
const originalSelect = { ...(await import('@/components/ui/select')) }
const originalMicCheck = { ...(await import('@/lib/micCheck')) }
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@/components/ui/select', () => originalSelect)
  mock.module('@/lib/micCheck', () => originalMicCheck)
})

let preferences: Record<string, unknown> = {}
let savedPreferences: Array<Record<string, unknown>> = []
let setPreferencesFails = false

mock.module('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args?: Record<string, unknown>) => {
    if (command === 'get_audio_devices') {
      return [
        { name: 'Scarlett Solo', device_type: 'Input' },
        { name: 'MacBook Pro Microphone', device_type: 'Input' },
        { name: 'BlackHole 2ch', device_type: 'Output' },
      ]
    }
    if (command === 'get_recording_preferences') return preferences
    if (command === 'set_recording_preferences') {
      if (setPreferencesFails) throw new Error('disk is full')
      savedPreferences.push(args?.preferences as Record<string, unknown>)
      return undefined
    }
    return undefined
  },
}))

// forwardRef because the panel holds a ref on the trigger to focus it for "choose a different
// device"; a plain function component would warn on every render.
const SelectTriggerStub = React.forwardRef<unknown, { children?: React.ReactNode }>(
  ({ children }, _ref) => <>{children}</>,
)
SelectTriggerStub.displayName = 'SelectTriggerStub'

// Radix's Select reaches for the DOM, which react-test-renderer does not provide. The stub keeps
// `onValueChange` reachable, which is the seam the picker is driven through here.
mock.module('@/components/ui/select', () => ({
  Select: ({ children, ...props }: { children?: React.ReactNode }) =>
    React.createElement('div', { 'data-picker': 'true', ...props }, children),
  SelectContent: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  SelectItem: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  SelectTrigger: SelectTriggerStub,
  SelectValue: () => null,
}))

// The check itself is another node's business; this file is about what the pickers announce.
mock.module('@/lib/micCheck', () => ({
  ...originalMicCheck,
  startSetupCheck: async () => {
    throw new Error('the check must not run in this test')
  },
  cancelSetupCheck: async () => {},
  onSetupCheckLevel: async () => () => {},
}))

const { SetupCheckPanel } = await import('./SetupCheckPanel')

/** The two pickers in document order: microphone first, then system audio. */
function pickers(root: ReturnType<typeof create>) {
  return root.root.findAll((node) => node.type === 'div' && node.props['data-picker'] === 'true', {
    deep: true,
  })
}

async function mountPanel(props: Record<string, unknown> = {}) {
  let renderer!: ReturnType<typeof create>
  await act(async () => {
    renderer = create(<SetupCheckPanel {...props} />)
  })
  return renderer
}

describe('SetupCheckPanel device pickers', () => {
  beforeEach(() => {
    preferences = { preferred_mic_device: null, preferred_system_device: null, auto_save: true }
    savedPreferences = []
    setPreferencesFails = false
  })

  test('choosing a microphone announces both channels, so the other is not cleared', async () => {
    const changes: Array<{ micDevice: string | null; systemDevice: string | null }> = []
    const renderer = await mountPanel({ onDevicesChanged: (devices: never) => changes.push(devices) })

    await act(async () => {
      pickers(renderer)[1].props.onValueChange('BlackHole 2ch (output)')
    })
    await act(async () => {
      pickers(renderer)[0].props.onValueChange('Scarlett Solo (input)')
    })

    // The in-memory selection this feeds replaces the pair, so the microphone change has to carry
    // the system device chosen a moment earlier rather than blanking it.
    expect(changes).toEqual([
      { micDevice: null, systemDevice: 'BlackHole 2ch (output)' },
      { micDevice: 'Scarlett Solo (input)', systemDevice: 'BlackHole 2ch (output)' },
    ])
  })

  test('picking the default entry announces null rather than the literal value', async () => {
    preferences = { preferred_mic_device: 'Scarlett Solo (input)', preferred_system_device: null }
    const changes: Array<{ micDevice: string | null; systemDevice: string | null }> = []
    const renderer = await mountPanel({ onDevicesChanged: (devices: never) => changes.push(devices) })

    await act(async () => {
      pickers(renderer)[0].props.onValueChange('default')
    })

    expect(changes).toEqual([{ micDevice: null, systemDevice: null }])
  })

  test('a preference that will not save still announces the device the check will test', async () => {
    setPreferencesFails = true
    const changes: Array<{ micDevice: string | null; systemDevice: string | null }> = []
    const renderer = await mountPanel({ onDevicesChanged: (devices: never) => changes.push(devices) })

    await act(async () => {
      pickers(renderer)[0].props.onValueChange('Scarlett Solo (input)')
    })

    // The recording this session opens comes from the in-memory selection, not from disk, so it
    // must name the device the check is about to validate even when the write failed.
    expect(changes).toEqual([{ micDevice: 'Scarlett Solo (input)', systemDevice: null }])
  })

  test('the panel still works for a surface that passes no handler', async () => {
    const renderer = await mountPanel()

    await act(async () => {
      pickers(renderer)[0].props.onValueChange('Scarlett Solo (input)')
    })

    expect(savedPreferences.at(-1)?.preferred_mic_device).toBe('Scarlett Solo (input)')
  })
})
