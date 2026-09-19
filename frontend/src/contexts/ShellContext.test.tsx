import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'

const startRecording = mock(async () => {})
const stopRecording = mock(async () => {})
const returnToRecording = mock(async () => {})
const openImportDialog = mock(() => {})
const navigate = mock(async () => {})
const openMeeting = mock(async () => {})
const searchTranscripts = mock(async () => {})
let sessionId: string | null = null
let activeMeetingId: string | null = null
let isRecording = false
let importEnabled = false
let command: string | null = null
let isCommandPending = false
let isProcessing = false
let isSaving = false

const originalController = { ...await import('@/contexts/RecordingControllerContext') }
const originalRecording = { ...await import('@/contexts/RecordingStateContext') }
const originalNavigation = { ...await import('@/hooks/useNavigation') }
const originalImport = { ...await import('@/contexts/ImportDialogContext') }
const originalConfig = { ...await import('@/contexts/ConfigContext') }
const originalTheme = { ...await import('@/lib/theme') }
const originalSidebar = { ...await import('@/components/Sidebar/SidebarProvider') }
const originalActivity = { ...await import('@/contexts/MeetingActivityContext') }
const originalDebug = { ...await import('@/hooks/useDebugMode') }
const originalNextNavigation = { ...await import('next/navigation') }

mock.module('@/contexts/RecordingControllerContext', () => ({
  ...originalController,
  useRecordingController: () => ({ sessionId, activeMeetingId, command, isCommandPending, startRecording, stopRecording, returnToRecording }),
}))
mock.module('@/contexts/RecordingStateContext', () => ({ ...originalRecording, useRecordingState: () => ({ isRecording, isProcessing, isSaving }) }))
mock.module('@/hooks/useNavigation', () => ({
  ...originalNavigation,
  useMeetingNavigation: () => ({ navigate, openMeeting, isNavigating: false, navigationError: null }),
}))
mock.module('@/contexts/ImportDialogContext', () => ({ ...originalImport, useImportDialog: () => ({ openImportDialog }) }))
mock.module('@/contexts/ConfigContext', () => ({ ...originalConfig, useConfig: () => ({ betaFeatures: { importAndRetranscribe: importEnabled } }) }))
mock.module('@/lib/theme', () => ({ ...originalTheme, readTheme: () => 'dark', applyTheme: () => {}, persistAndBroadcastTheme: async () => {} }))
mock.module('@/components/Sidebar/SidebarProvider', () => ({
  ...originalSidebar,
  useSidebar: () => ({
    currentMeeting: null,
    searchTranscripts,
    searchStatus: 'idle',
    searchError: null,
    catalogStatus: 'ready',
    catalogError: null,
    selectMeetings: () => [],
    selectSearchResults: () => [],
    setMeetingPinned: async () => {},
    setMeetingArchived: async () => {},
    meetingMutations: {},
    refetchMeetings: async () => {},
  }),
}))
mock.module('@/contexts/MeetingActivityContext', () => ({ ...originalActivity, useMeetingActivity: () => ({ activeMeetingId: null, getMeetingActivities: () => [] }) }))
mock.module('@/hooks/useDebugMode', () => ({ ...originalDebug, useDebugMode: () => false }))
mock.module('next/navigation', () => ({ ...originalNextNavigation, usePathname: () => '/' }))

const originalWindow = globalThis.window
const originalDocument = globalThis.document
const originalLocalStorage = globalThis.localStorage
const { ShellNavigationError, ShellProvider, handleShellActionError, useShell } = await import('./ShellContext')
const { default: SimpleSidebar } = await import('@/components/SimpleSidebar')
const { MeetingsLibrary } = await import('@/components/MeetingsLibrary')

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
  command = null
  isCommandPending = false
  isProcessing = false
  isSaving = false
  for (const fn of [startRecording, stopRecording, returnToRecording, openImportDialog, navigate, openMeeting, searchTranscripts]) fn.mockClear()
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
  mock.module('@/components/Sidebar/SidebarProvider', () => originalSidebar)
  mock.module('@/contexts/MeetingActivityContext', () => originalActivity)
  mock.module('@/hooks/useDebugMode', () => originalDebug)
  mock.module('next/navigation', () => originalNextNavigation)
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

  test('shares one search owner and disables recording actions during finalization', async () => {
    command = 'finalize'
    isCommandPending = true
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<ShellProvider><Probe /></ShellProvider>) })
    expect(current.recordingActionLabel).toBe('Finishing meeting')
    expect(current.recordingActionDisabled).toBe(true)
    await act(async () => current.runRecordingAction())
    expect(startRecording).not.toHaveBeenCalled()

    await act(async () => current.setMeetingSearchQuery('roadmap'))
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 300)) })
    expect(searchTranscripts).toHaveBeenLastCalledWith('roadmap')
    await act(async () => renderer!.unmount())
  })

  test('keeps controller failures with the controller and retains navigation for retry', async () => {
    startRecording.mockRejectedValueOnce(new Error('capture unavailable'))
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<ShellProvider><Probe /></ShellProvider>) })
    await expect(current.runRecordingAction()).resolves.toBeUndefined()

    navigate.mockRejectedValueOnce(new Error('notes unsaved'))
    await expect(current.navigate('/settings')).rejects.toBeInstanceOf(ShellNavigationError)
    await current.retryNavigation()
    expect(navigate).toHaveBeenLastCalledWith('/settings')
    await act(async () => renderer!.unmount())
  })

  test('reserves navigation failures for global feedback while reporting other action errors', () => {
    const reported: string[] = []
    handleShellActionError(new ShellNavigationError(new Error('notes unsaved')), (message) => reported.push(message))
    handleShellActionError(new Error('archive failed'), (message) => reported.push(message))
    expect(reported).toEqual(['archive failed'])
  })

  test('keeps sidebar and library search inputs on one query', async () => {
    let renderer: ReturnType<typeof create>
    await act(async () => {
      renderer = create(<ShellProvider><SimpleSidebar /><MeetingsLibrary /></ShellProvider>)
    })
    searchTranscripts.mockClear()
    let inputs = renderer!.root.findAllByProps({ 'aria-label': 'Search meetings' })
    expect(inputs).toHaveLength(2)
    await act(async () => inputs[0].props.onChange({ target: { value: 'roadmap' } }))
    inputs = renderer!.root.findAllByProps({ 'aria-label': 'Search meetings' })
    expect(inputs.map((input) => input.props.value)).toEqual(['roadmap', 'roadmap'])
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 300)) })
    expect(searchTranscripts).toHaveBeenCalledTimes(1)
    expect(searchTranscripts).toHaveBeenCalledWith('roadmap')
    await act(async () => renderer!.unmount())
  })
})
