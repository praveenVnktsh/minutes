import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create, type ReactTestRenderer } from 'react-test-renderer'
import { renderToString } from 'react-dom/server'

type EventHandler = (event: { payload: unknown }) => void

let onboardingRead: Promise<{ completed: boolean } | null>
let activitySnapshot = { revision: 1, recording: null as null | Record<string, unknown>, activities: [] as Array<Record<string, unknown>> }
let storedTheme: string | null = null
const invoked: string[] = []
const eventHandlers = new Map<string, Set<EventHandler>>()

const originalCore = { ...await import('@tauri-apps/api/core') }
const originalEvent = { ...await import('@tauri-apps/api/event') }
const originalWindowApi = { ...await import('@tauri-apps/api/window') }
const originalNavigation = { ...await import('next/navigation') }

const invoke = mock(async (command: string) => {
  invoked.push(command)
  if (command === 'get_onboarding_status') return onboardingRead
  if (command === 'get_meeting_activity_snapshot') return activitySnapshot
  if (command === 'get_pending_recording_request') return null
  if (command === 'api_get_meetings') return []
  if (command === 'api_get_model_config') return null
  if (command === 'api_get_api_key') return ''
  if (command === 'api_get_transcript_config') return null
  if (command === 'get_ollama_models') return []
  if (command === 'get_recording_preferences') return null
  if (command === 'get_notification_settings') return null
  if (command.includes('directory') || command.includes('folder')) return ''
  return null
})
const setNativeTheme = mock(async () => {})
const listen = mock(async (event: string, handler: EventHandler) => {
  const handlers = eventHandlers.get(event) ?? new Set<EventHandler>()
  handlers.add(handler)
  eventHandlers.set(event, handlers)
  return () => handlers.delete(handler)
})

mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }))
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen, emit: mock(async () => {}) }))
mock.module('@tauri-apps/api/window', () => ({
  ...originalWindowApi,
  getAllWindows: async () => [{ setTheme: setNativeTheme }],
  getCurrentWindow: () => ({ hide: async () => {}, setTheme: async () => {} }),
}))
mock.module('next/navigation', () => ({
  ...originalNavigation,
  usePathname: () => '/',
  useRouter: () => ({ push: mock(() => {}), replace: mock(() => {}), back: mock(() => {}) }),
}))
mock.module('next/font/local', () => ({
  default: ({ src }: { src: string }) => ({ variable: src.includes('serif') ? 'font-serif-test' : 'font-sans-test' }),
}))
mock.module('@/components/AnalyticsProvider', () => ({ default: ({ children }: { children: React.ReactNode }) => <>{children}</> }))
mock.module('@/contexts/OllamaDownloadContext', () => ({ OllamaDownloadProvider: ({ children }: { children: React.ReactNode }) => <>{children}</> }))
mock.module('@/contexts/OnboardingContext', () => ({ OnboardingProvider: ({ children }: { children: React.ReactNode }) => <>{children}</> }))
mock.module('@/components/UpdateCheckProvider', () => ({ UpdateCheckProvider: ({ children }: { children: React.ReactNode }) => <>{children}</> }))
mock.module('@/components/ui/tooltip', () => ({ TooltipProvider: ({ children }: { children: React.ReactNode }) => <>{children}</> }))
mock.module('@/components/onboarding', () => ({ OnboardingFlow: () => <div>SETUP FLOW</div> }))
mock.module('@/components/shared/DownloadProgressToast', () => ({ DownloadProgressToastProvider: () => null }))
mock.module('@/components/ImportAudio', () => ({ ImportAudioDialog: () => null, ImportDropOverlay: () => null }))
mock.module('@/components/MeetingDetectedPrompt', () => ({ MeetingDetectedPrompt: () => null }))
const toastMock = Object.assign(mock(() => {}), {
  success: mock(() => {}), error: mock(() => {}), info: mock(() => {}), warning: mock(() => {}),
})
mock.module('sonner', () => ({ Toaster: () => <div>TOASTER</div>, toast: toastMock }))
mock.module('@/services/indexedDBService', () => ({
  indexedDBService: new Proxy({}, {
    get: (_target, property) => property === 'getCurrentMeetingId' ? () => null : async () => [],
  }),
}))

const originalGlobalWindow = globalThis.window
const originalDocument = globalThis.document
const originalLocalStorage = globalThis.localStorage
const originalSessionStorage = globalThis.sessionStorage
const { MainAppLayout } = await import('./layout')

let renderer: ReactTestRenderer | undefined
const flush = async () => act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)) })

beforeEach(() => {
  invoked.length = 0
  setNativeTheme.mockClear()
  eventHandlers.clear()
  onboardingRead = new Promise(() => {})
  activitySnapshot = { revision: 1, recording: null, activities: [] }
  storedTheme = null
  const browserWindow = new EventTarget() as Window & typeof globalThis
  Object.assign(browserWindow, {
    innerWidth: 1440,
    location: { search: '' },
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
  })
  const values = new Map<string, string>()
  Object.defineProperty(globalThis, 'window', { configurable: true, value: browserWindow })
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: { getItem: (key: string) => key === 'meetily:theme' ? storedTheme : values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) },
  })
  Object.defineProperty(globalThis, 'sessionStorage', {
    configurable: true,
    value: { getItem: (key: string) => values.get(`session:${key}`) ?? null, setItem: (key: string, value: string) => values.set(`session:${key}`, value), removeItem: (key: string) => values.delete(`session:${key}`) },
  })
  Object.defineProperty(globalThis, 'document', {
    configurable: true,
    value: {
      documentElement: { classList: { toggle: () => {}, add: () => {} }, style: {} },
      body: { style: {} },
      addEventListener: () => {},
      removeEventListener: () => {},
    },
  })
})

afterEach(async () => {
  if (renderer) await act(async () => renderer?.unmount())
  renderer = undefined
})

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@tauri-apps/api/event', () => originalEvent)
  mock.module('@tauri-apps/api/window', () => originalWindowApi)
  mock.module('next/navigation', () => originalNavigation)
  if (originalGlobalWindow) Object.defineProperty(globalThis, 'window', { configurable: true, value: originalGlobalWindow })
  else Reflect.deleteProperty(globalThis, 'window')
  if (originalDocument) Object.defineProperty(globalThis, 'document', { configurable: true, value: originalDocument })
  else Reflect.deleteProperty(globalThis, 'document')
  if (originalLocalStorage) Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: originalLocalStorage })
  else Reflect.deleteProperty(globalThis, 'localStorage')
  if (originalSessionStorage) Object.defineProperty(globalThis, 'sessionStorage', { configurable: true, value: originalSessionStorage })
  else Reflect.deleteProperty(globalThis, 'sessionStorage')
})

describe('application startup composition', () => {
  test('server-renders the boot shell without requiring ShellProvider', () => {
    expect(() => renderToString(<MainAppLayout><div>ROUTE</div></MainAppLayout>)).not.toThrow()
  })

  test('does not mount app commands while startup status is unresolved', async () => {
    await act(async () => { renderer = create(<MainAppLayout><div>ROUTE</div></MainAppLayout>) })
    expect(JSON.stringify(renderer!.toJSON())).toContain('Opening Minutes')
    expect(invoked).not.toContain('get_pending_recording_request')
    expect(setNativeTheme).toHaveBeenCalledWith('dark')
  })

  test('applies persisted native chrome theme during boot', async () => {
    storedTheme = 'light'
    await act(async () => { renderer = create(<MainAppLayout><div>ROUTE</div></MainAppLayout>) })
    expect(setNativeTheme).toHaveBeenCalledWith('light')
    expect(document.body.style.visibility).toBe('visible')
  })

  test('keeps setup isolated from the recording controller', async () => {
    onboardingRead = Promise.resolve({ completed: false })
    await act(async () => { renderer = create(<MainAppLayout><div>ROUTE</div></MainAppLayout>) })
    await flush()
    expect(JSON.stringify(renderer!.toJSON())).toContain('SETUP FLOW')
    expect(invoked).not.toContain('get_pending_recording_request')
  })

  test('shows a retryable startup error without mounting commands', async () => {
    onboardingRead = Promise.reject(new Error('database locked'))
    await act(async () => { renderer = create(<MainAppLayout><div>ROUTE</div></MainAppLayout>) })
    await flush()
    expect(JSON.stringify(renderer!.toJSON())).toContain('Minutes could not start')
    expect(invoked).not.toContain('get_pending_recording_request')

    onboardingRead = Promise.resolve({ completed: false })
    const retry = renderer!.root.findByProps({ children: 'Retry' })
    await act(async () => retry.props.onClick())
    await flush()
    expect(JSON.stringify(renderer!.toJSON())).toContain('SETUP FLOW')
  })

  test('mounts the real activity, recording, transcript, config, catalog, and controller chain only when ready', async () => {
    onboardingRead = Promise.resolve({ completed: true })
    await act(async () => { renderer = create(<MainAppLayout><div>ROUTE READY</div></MainAppLayout>) })
    await flush()
    expect(JSON.stringify(renderer!.toJSON())).toContain('ROUTE READY')
    expect(JSON.stringify(renderer!.toJSON())).toContain('TOASTER')
    expect(invoked).toContain('get_meeting_activity_snapshot')
    expect(invoked).toContain('api_get_meetings')
    expect(invoked).toContain('get_pending_recording_request')
  })

  test('keeps finalization and active jobs visible ahead of retained failures', async () => {
    onboardingRead = Promise.resolve({ completed: true })
    activitySnapshot = {
      revision: 20,
      recording: { session_id: 'session-a', meeting_id: 'meeting-a', status: 'saving', error: null },
      activities: [
        ...Array.from({ length: 4 }, (_, index) => ({
          task_id: `failed-${index}`, meeting_id: `failed-meeting-${index}`, kind: 'import', status: 'failed',
          title: `Retained failure ${index}`, stage: null, progress_percentage: null, message: null,
          error: 'failed', warning: null, controls_available: false, revision: index + 1,
        })),
        {
          task_id: 'active-job', meeting_id: 'active-job-meeting', kind: 'import', status: 'transcribing',
          title: 'Active transcription', stage: 'transcribing', progress_percentage: 42, message: null,
          error: null, warning: null, controls_available: true, revision: 19,
        },
      ],
    }
    await act(async () => { renderer = create(<MainAppLayout><div>ROUTE READY</div></MainAppLayout>) })
    await flush()
    const rendered = JSON.stringify(renderer!.toJSON())
    expect(rendered).toContain('Saving meeting')
    expect(rendered).toContain('Active transcription')
    expect(rendered).toContain('more activities')
  })
})
