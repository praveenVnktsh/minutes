import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'
import type { RecordingRequestResult } from '@/types/meetingActivity'

type EventHandler = (event: { payload: unknown }) => void
const handlers = new Map<string, EventHandler>()
const hide = mock(async () => {})
let requestNumber = 0
let emitResultBeforeResolve = false

const originalCore = { ...await import('@tauri-apps/api/core') }
const originalEvent = { ...await import('@tauri-apps/api/event') }
const originalWindowApi = { ...await import('@tauri-apps/api/window') }

const invoke = mock(async (command: string) => {
  if (command !== 'start_recording_from_prompt') return null
  const requestId = `request-${++requestNumber}`
  if (emitResultBeforeResolve) {
    handlers.get('meeting-prompt-start-result')?.({ payload: { request_id: requestId, accepted: false, error: 'Microphone denied', session_id: null } })
  }
  return { request_id: requestId, source: 'meeting_prompt', status: 'pending', claimed_by: null, started_session_id: null }
})
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }))
mock.module('@tauri-apps/api/event', () => ({
  ...originalEvent,
  listen: async (event: string, handler: EventHandler) => {
    handlers.set(event, handler)
    return () => handlers.delete(event)
  },
}))
mock.module('@tauri-apps/api/window', () => ({ ...originalWindowApi, getCurrentWindow: () => ({ hide }) }))
mock.module('@/lib/theme', () => ({ readTheme: () => 'dark', applyTheme: () => {}, listenForThemeChanges: async () => () => {} }))

const originalWindow = globalThis.window
const { default: MeetingPromptPage } = await import('./page')

beforeEach(() => {
  handlers.clear()
  hide.mockClear()
  invoke.mockClear()
  requestNumber = 0
  emitResultBeforeResolve = false
  Object.defineProperty(globalThis, 'window', { configurable: true, value: new EventTarget() })
})

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@tauri-apps/api/event', () => originalEvent)
  mock.module('@tauri-apps/api/window', () => originalWindowApi)
  if (originalWindow) Object.defineProperty(globalThis, 'window', { configurable: true, value: originalWindow })
  else Reflect.deleteProperty(globalThis, 'window')
})

describe('meeting prompt acknowledgement', () => {
  test('stays visible and offers retry after a correlated failure', async () => {
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<MeetingPromptPage />) })
    const start = renderer!.root.findAllByType('button').find((button) => button.children.some((child) => child === 'Start'))!
    await act(async () => { await start.props.onClick() })
    const result: RecordingRequestResult = { request_id: 'request-1', accepted: false, error: 'Device unavailable', session_id: null }
    await act(async () => { handlers.get('meeting-prompt-start-result')?.({ payload: result }) })
    expect(hide).not.toHaveBeenCalled()
    expect(JSON.stringify(renderer!.toJSON())).toContain('Device unavailable')
    expect(JSON.stringify(renderer!.toJSON())).toContain('Retry')
    await act(async () => renderer!.unmount())
  })

  test('handles a result that arrives before the start command resolves', async () => {
    emitResultBeforeResolve = true
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<MeetingPromptPage />) })
    const start = renderer!.root.findAllByType('button').find((button) => button.children.some((child) => child === 'Start'))!
    await act(async () => { await start.props.onClick() })
    expect(hide).not.toHaveBeenCalled()
    expect(JSON.stringify(renderer!.toJSON())).toContain('Microphone denied')
    await act(async () => renderer!.unmount())
  })

  test('hides only after the matching native success acknowledgement', async () => {
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<MeetingPromptPage />) })
    const start = renderer!.root.findAllByType('button').find((button) => button.children.some((child) => child === 'Start'))!
    await act(async () => { await start.props.onClick() })
    await act(async () => {
      handlers.get('meeting-prompt-start-result')?.({ payload: { request_id: 'other', accepted: true, error: null, session_id: 'wrong' } })
    })
    expect(hide).not.toHaveBeenCalled()
    await act(async () => {
      handlers.get('meeting-prompt-start-result')?.({ payload: { request_id: 'request-1', accepted: true, error: null, session_id: 'session-1' } })
    })
    expect(hide).toHaveBeenCalledTimes(1)
    await act(async () => renderer!.unmount())
  })
})
