import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'
import { selectMeetings as selectCatalogMeetings, selectSearchResults as selectCatalogSearchResults } from '@/lib/meetingCatalog'

const meetings = [
  { id: 'active', title: 'Active planning', created_at: '2026-09-19T10:00:00Z', pinned: true, archived: false, debug: false },
  { id: 'archived', title: 'Archived review', archived: true, debug: false },
  { id: 'debug', title: 'Debug capture', archived: false, debug: true },
]
const searchTranscripts = mock(async () => {})
const refetchMeetings = mock(async () => {})
const setMeetingPinned = mock(async () => {})
const setMeetingArchived = mock(async () => {})
const runRecordingAction = mock(async () => {})
const runImportAction = mock(async () => {})
const openMeeting = mock(async () => {})

const originalSidebar = { ...await import('@/components/Sidebar/SidebarProvider') }
const originalShell = { ...await import('@/contexts/ShellContext') }
const originalActivity = { ...await import('@/contexts/MeetingActivityContext') }
const originalDebug = { ...await import('@/hooks/useDebugMode') }

mock.module('@/components/Sidebar/SidebarProvider', () => ({
  ...originalSidebar,
  useSidebar: () => ({
    catalogStatus: 'ready', catalogError: null, searchStatus: 'idle', searchError: null,
    searchTranscripts, refetchMeetings, setMeetingPinned, setMeetingArchived, meetingMutations: {},
    selectMeetings: (visibility?: Parameters<typeof selectCatalogMeetings>[1]) => selectCatalogMeetings(meetings, visibility),
    selectSearchResults: (visibility?: Parameters<typeof selectCatalogSearchResults>[2]) => selectCatalogSearchResults([], meetings, visibility),
  }),
}))
mock.module('@/contexts/ShellContext', () => ({
  ...originalShell,
  useShell: () => ({
    recordingActionLabel: 'Return to recording', runRecordingAction,
    importActionLabel: 'Enable audio import', runImportAction,
    openMeeting, isNavigating: false, navigationError: null,
  }),
}))
mock.module('@/contexts/MeetingActivityContext', () => ({
  ...originalActivity,
  useMeetingActivity: () => ({ activeMeetingId: 'active', getMeetingActivities: () => [] }),
}))
mock.module('@/hooks/useDebugMode', () => ({ ...originalDebug, useDebugMode: () => false }))

const originalWindow = globalThis.window
const { MeetingsLibrary } = await import('./MeetingsLibrary')

beforeEach(() => {
  for (const fn of [searchTranscripts, refetchMeetings, setMeetingPinned, setMeetingArchived, runRecordingAction, runImportAction, openMeeting]) fn.mockClear()
  Object.defineProperty(globalThis, 'window', { configurable: true, value: new EventTarget() })
})

afterAll(() => {
  mock.module('@/components/Sidebar/SidebarProvider', () => originalSidebar)
  mock.module('@/contexts/ShellContext', () => originalShell)
  mock.module('@/contexts/MeetingActivityContext', () => originalActivity)
  mock.module('@/hooks/useDebugMode', () => originalDebug)
  if (originalWindow) Object.defineProperty(globalThis, 'window', { configurable: true, value: originalWindow })
  else Reflect.deleteProperty(globalThis, 'window')
})

describe('MeetingsLibrary', () => {
  test('shows truthful active identity and shared shell action labels', async () => {
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<MeetingsLibrary />) })
    const rendered = JSON.stringify(renderer!.toJSON())
    expect(rendered).toContain('Active planning')
    expect(rendered).toContain('Live')
    expect(rendered).toContain('Return to recording')
    expect(rendered).toContain('Enable audio import')
    expect(rendered).not.toContain('Archived review')
    expect(rendered).not.toContain('Debug capture')
    await act(async () => renderer!.unmount())
  })

  test('uses the catalog archive filter without exposing debug meetings', async () => {
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<MeetingsLibrary />) })
    const archived = renderer!.root.findAllByType('button').find((button) => button.children.includes('archived'))!
    await act(async () => archived.props.onClick())
    const rendered = JSON.stringify(renderer!.toJSON())
    expect(rendered).toContain('Archived review')
    expect(rendered).not.toContain('Active planning')
    expect(rendered).not.toContain('Debug capture')
    await act(async () => renderer!.unmount())
  })
})
