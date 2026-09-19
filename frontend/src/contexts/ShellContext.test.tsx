import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'

const startRecording = mock(async () => {})
const stopRecording = mock(async () => {})
const returnToRecording = mock(async () => {})
const openImportDialog = mock(() => {})
const navigate = mock(async () => {})
const openMeeting = mock(async () => {})
let sessionId: string | null = null
let activeMeetingId: string | null = null
let isRecording = false
let importEnabled = false

const originalController = { ...await import('@/contexts/RecordingControllerContext') }
const originalRecording = { ...await import('@/contexts/RecordingStateContext') }
const originalNavigation = { ...await import('@/hooks/useNavigation') }
const originalImport = { ...await import('@/contexts/ImportDialogContext') }
const originalConfig = { ...await import('@/contexts/ConfigContext') }
const originalTheme = { ...await import('@/lib/theme') }

mock.module('@/contexts/RecordingControllerContext', () => ({
  ...originalController,
  useRecordingController: () => ({ sessionId, activeMeetingId, startRecording, stopRecording, returnToRecording }),
}))
mock.module('@/contexts/RecordingStateContext', () => ({ ...originalRecording, useRecordingState: () => ({ isRecording }) }))
mock.module('@/hooks/useNavigation', () => ({
  ...originalNavigation,
  useMeetingNavigation: () => ({ navigate, openMeeting, isNavigating: false, navigationError: null }),
}))
mock.module('@/contexts/ImportDialogContext', () => ({ ...originalImport, useImportDialog: () => ({ openImportDialog }) }))
mock.module('@/contexts/ConfigContext', () => ({ ...originalConfig, useConfig: () => ({ betaFeatures: { importAndRetranscribe: importEnabled } }) }))
mock.module('@/lib/theme', () => ({ ...originalTheme, readTheme: () => 'dark', applyTheme: () => {}, persistAndBroadcastTheme: async () => {} }))

const originalWindow = globalThis.window
const originalDocument = globalThis.document
const originalLocalStorage = globalThis.localStorage
const { ShellProvider, useShell } = await import('./ShellContext')

let current!: ReturnType<typeof useShell>
function Probe() {
  current = useShell()
  return <div>{current.recordingActionLabel}|{current.importActionLabel}</div>
}

beforeEach(() => {
  sessionId = null
  activeMeetingId = null
  isRecording = false
  importEnabled = false
  for (const fn of [startRecording, stopRecording, returnToRecording, openImportDialog, navigate, openMeeting]) fn.mockClear()
  const browserWindow = new EventTarget() as Window & typeof globalThis
  Object.assign(browserWindow, { innerWidth: 1440 })
  Object.defineProperty(globalThis, 'window', { configurable: true, value: browserWindow })
  Object.defineProperty(globalThis, 'document', { configurable: true, value: { documentElement: { classList: { toggle: () => {} }, style: {} } } })
  Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: { getItem: () => null, setItem: () => {} } })
})

afterAll(() => {
  mock.module('@/contexts/RecordingControllerContext', () => originalController)
  mock.module('@/contexts/RecordingStateContext', () => originalRecording)
  mock.module('@/hooks/useNavigation', () => originalNavigation)
  mock.module('@/contexts/ImportDialogContext', () => originalImport)
  mock.module('@/contexts/ConfigContext', () => originalConfig)
  mock.module('@/lib/theme', () => originalTheme)
  if (originalWindow) Object.defineProperty(globalThis, 'window', { configurable: true, value: originalWindow })
  else Reflect.deleteProperty(globalThis, 'window')
  if (originalDocument) Object.defineProperty(globalThis, 'document', { configurable: true, value: originalDocument })
  else Reflect.deleteProperty(globalThis, 'document')
  if (originalLocalStorage) Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: originalLocalStorage })
  else Reflect.deleteProperty(globalThis, 'localStorage')
})

describe('ShellProvider shared actions', () => {
  test('starts only when there is no active native session', async () => {
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<ShellProvider><Probe /></ShellProvider>) })
    expect(renderer!.root.findByType('div').children.join('')).toContain('New meeting|Enable audio import')
    await act(async () => current.runRecordingAction())
    expect(startRecording).toHaveBeenCalledWith({ source: 'app_shell' })
    await act(async () => renderer!.unmount())
  })

  test('returns to the authoritative active meeting and stops when its row is unavailable', async () => {
    sessionId = 'session-a'
    activeMeetingId = 'meeting-a'
    isRecording = true
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<ShellProvider><Probe /></ShellProvider>) })
    expect(current.recordingActionLabel).toBe('Return to recording')
    await act(async () => current.runRecordingAction())
    expect(returnToRecording).toHaveBeenCalledTimes(1)

    activeMeetingId = null
    await act(async () => renderer!.update(<ShellProvider><Probe /></ShellProvider>))
    expect(current.recordingActionLabel).toBe('Stop recording')
    await act(async () => current.runRecordingAction())
    expect(stopRecording).toHaveBeenCalledTimes(1)
    await act(async () => renderer!.unmount())
  })

  test('routes disabled import to canonical Beta settings and opens it when enabled', async () => {
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<ShellProvider><Probe /></ShellProvider>) })
    await act(async () => current.runImportAction())
    expect(navigate).toHaveBeenCalledWith('/settings?section=beta')
    expect(openImportDialog).not.toHaveBeenCalled()

    importEnabled = true
    await act(async () => renderer!.update(<ShellProvider><Probe /></ShellProvider>))
    expect(current.importActionLabel).toBe('Import recording')
    await act(async () => current.runImportAction())
    expect(openImportDialog).toHaveBeenCalledTimes(1)
    await act(async () => renderer!.unmount())
  })
})
