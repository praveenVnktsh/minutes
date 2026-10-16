import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create, type ReactTestRenderer } from 'react-test-renderer'

const originalCore = { ...(await import('@tauri-apps/api/core')) }
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
})

type App = { name: string; bundle_id: string | null }
let settings: { ignored_apps: App[]; only_known_apps: boolean }
let recent: App[]
const calls: Array<{ command: string; args: unknown }> = []

mock.module('@tauri-apps/api/core', () => ({
  ...originalCore,
  invoke: async (command: string, args?: Record<string, unknown>) => {
    calls.push({ command, args })
    switch (command) {
      case 'get_meeting_detection_settings':
        return settings
      case 'get_recent_audio_apps':
        return recent
      case 'get_meeting_detection_snooze':
        return null
      case 'ignore_meeting_app':
        settings = { ...settings, ignored_apps: [...settings.ignored_apps, args!.target as App] }
        return settings
      case 'unignore_meeting_app': {
        const target = args!.target as App
        settings = { ...settings, ignored_apps: settings.ignored_apps.filter((app) => app.name !== target.name) }
        return settings
      }
      case 'set_meeting_detection_settings':
        settings = args!.settings as typeof settings
        return settings
      default:
        return undefined
    }
  },
}))

const { MeetingDetectionIgnoreList } = await import('./MeetingDetectionIgnoreList')

async function render() {
  let renderer!: ReactTestRenderer
  await act(async () => {
    renderer = create(<MeetingDetectionIgnoreList />)
  })
  return renderer
}

const text = (renderer: ReactTestRenderer) => JSON.stringify(renderer.toJSON())
type TextNode = { children: Array<TextNode | string> }
const textOf = (node: TextNode | string): string =>
  typeof node === 'string' ? node : node.children.map(textOf).join('')

describe('MeetingDetectionIgnoreList', () => {
  beforeEach(() => {
    settings = { ignored_apps: [{ name: 'Superwhisper', bundle_id: 'com.superduper.superwhisper' }], only_known_apps: false }
    recent = [
      { name: 'Superwhisper', bundle_id: 'com.superduper.superwhisper' },
      { name: 'Raycast', bundle_id: 'com.raycast.macos' },
    ]
    calls.length = 0
  })

  test('lists ignored apps and removes one', async () => {
    const renderer = await render()
    expect(text(renderer)).toContain('Superwhisper')
    const remove = renderer.root.findByProps({ 'aria-label': 'Remove Superwhisper' })
    await act(async () => { remove.props.onClick() })
    expect(calls.some((call) => call.command === 'unignore_meeting_app')).toBe(true)
    expect(renderer.root.findAllByProps({ 'aria-label': 'Remove Superwhisper' })).toHaveLength(0)
  })

  test('suggests recently seen apps that are not already ignored and adds with the bundle id', async () => {
    const renderer = await render()
    const chips = renderer.root.findAll((node) => node.type === 'button' && textOf(node).includes('Raycast'))
    expect(chips).toHaveLength(1)
    // Superwhisper is ignored already, so it is not suggested again.
    expect(renderer.root.findAll((node) => node.type === 'button' && textOf(node) === 'Superwhisper')).toHaveLength(0)
    await act(async () => { chips[0].props.onClick() })
    const added = calls.find((call) => call.command === 'ignore_meeting_app')
    expect(added?.args).toEqual({ target: { name: 'Raycast', bundle_id: 'com.raycast.macos' } })
  })

  test('adds a typed app name', async () => {
    const renderer = await render()
    const input = renderer.root.findByProps({ 'aria-label': 'App name to ignore' })
    await act(async () => { input.props.onChange({ target: { value: '  Voice Tool ' } }) })
    const form = renderer.root.findByType('form')
    await act(async () => { form.props.onSubmit({ preventDefault: () => {} }) })
    const added = calls.find((call) => call.command === 'ignore_meeting_app')
    expect(added?.args).toEqual({ target: { name: 'Voice Tool', bundle_id: null } })
    expect(text(renderer)).toContain('Voice Tool')
  })

  test('saves the only-known-apps toggle', async () => {
    const renderer = await render()
    const toggle = renderer.root.findByProps({ 'aria-label': 'Only prompt for known meeting apps' })
    await act(async () => { toggle.props.onCheckedChange(true) })
    const saved = calls.find((call) => call.command === 'set_meeting_detection_settings')
    expect((saved?.args as { settings: typeof settings }).settings.only_known_apps).toBe(true)
  })
})
