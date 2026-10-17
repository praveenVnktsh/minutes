import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create, type ReactTestRenderer } from 'react-test-renderer'

const originalCore = { ...(await import('@tauri-apps/api/core')) }
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
})

let stored: boolean | undefined
let failSave = false
const calls: Array<{ command: string; args: unknown }> = []

mock.module('@tauri-apps/api/core', () => ({
  ...originalCore,
  invoke: async (command: string, args?: Record<string, unknown>) => {
    calls.push({ command, args })
    switch (command) {
      case 'api_get_summary_from_live_notes':
        return { enabled: stored ?? true }
      case 'api_set_summary_from_live_notes':
        if (failSave) throw new Error('disk full')
        stored = args!.enabled as boolean
        return { enabled: stored }
      default:
        return undefined
    }
  },
}))

const { SummaryFromLiveNotesSettings } = await import('./SummaryFromLiveNotesSettings')

async function render() {
  let renderer!: ReactTestRenderer
  await act(async () => {
    renderer = create(<SummaryFromLiveNotesSettings />)
  })
  return renderer
}

const toggle = (renderer: ReactTestRenderer) =>
  renderer.root.findByProps({ 'aria-label': 'Build notes from live notes' })

describe('SummaryFromLiveNotesSettings', () => {
  beforeEach(() => {
    stored = undefined
    failSave = false
    calls.length = 0
  })

  test('is on by default', async () => {
    const renderer = await render()
    expect(toggle(renderer).props.checked).toBe(true)
    expect(toggle(renderer).props.disabled).toBe(false)
  })

  test('turning it off saves the flag', async () => {
    const renderer = await render()
    await act(async () => { toggle(renderer).props.onCheckedChange(false) })
    expect(calls.find((call) => call.command === 'api_set_summary_from_live_notes')?.args).toEqual({ enabled: false })
    expect(toggle(renderer).props.checked).toBe(false)
  })

  test('shows the saved value and reverts when saving fails', async () => {
    stored = false
    const renderer = await render()
    expect(toggle(renderer).props.checked).toBe(false)
    failSave = true
    await act(async () => { toggle(renderer).props.onCheckedChange(true) })
    expect(toggle(renderer).props.checked).toBe(false)
  })
})
