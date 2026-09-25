import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'

const originalActivity = { ...await import('@/contexts/MeetingActivityContext') }
const originalController = { ...await import('@/contexts/RecordingControllerContext') }
const originalRecording = { ...await import('@/contexts/RecordingStateContext') }
const originalSidebar = { ...await import('@/components/Sidebar/SidebarProvider') }
const originalNavigation = { ...await import('next/navigation') }

const failedActivities = Array.from({ length: 4 }, (_, index) => ({
  task_id: `failed-${index}`, meeting_id: `failed-meeting-${index}`, kind: 'import' as const, status: 'failed' as const,
  title: `Retained failure ${index}`, stage: null, progress_percentage: null, message: null,
  error: 'failed', warning: null, controls_available: false, revision: index + 1,
}))

const pauseRecording = mock(async () => {})
const resumeRecording = mock(async () => {})
const dismissSummary = mock(() => {})
let summaries: Array<Record<string, unknown>> = []
let isRecording = false
let isPaused = false
let isCommandPending = true
let controllerCommand: string | null = 'finalize'
let activeMeetingId: string | null = null
let pathname = '/settings'
let currentMeeting: { id: string } | null = null

mock.module('@/contexts/MeetingActivityContext', () => ({
  ...originalActivity,
  useMeetingActivity: () => ({
    snapshot: {
      revision: 20,
      recording: null,
      activities: [
        ...failedActivities,
        { task_id: 'starting-import', meeting_id: null, kind: 'import', status: 'starting', title: 'Preparing early import', stage: 'starting', progress_percentage: null, message: null, error: null, warning: null, controls_available: false, revision: 21 },
        { task_id: 'recording-failed', meeting_id: 'recording-meeting', kind: 'recording', status: 'failed', title: 'Duplicate recording failure', stage: null, progress_percentage: null, message: null, error: 'capture failed', warning: null, controls_available: false, revision: 10 },
        { task_id: 'active-job', meeting_id: 'active-meeting', kind: 'import', status: 'transcribing', title: 'Active transcription', stage: 'transcribing', progress_percentage: 42, message: null, error: null, warning: null, controls_available: true, revision: 19 },
      ],
    },
    summaries,
    retrySummary: async () => {}, cancelSummary: async () => true, dismissSummary,
    cancelTranscription: async () => true, pauseTranscription: async () => true, resumeTranscription: async () => true,
  }),
}))
mock.module('@/contexts/RecordingControllerContext', () => ({
  ...originalController,
  useRecordingController: () => ({
    command: controllerCommand, activeMeetingId, isCommandPending, returnToRecording: async () => {},
    stopRecording: async () => {}, pauseRecording, resumeRecording,
  }),
}))
mock.module('@/contexts/RecordingStateContext', () => ({ ...originalRecording, useRecordingState: () => ({ isRecording, isPaused, isProcessing: false, isSaving: !isRecording }) }))
mock.module('@/components/Sidebar/SidebarProvider', () => ({ ...originalSidebar, useSidebar: () => ({ currentMeeting }) }))
mock.module('next/navigation', () => ({ ...originalNavigation, usePathname: () => pathname }))

const { ShellActivitySurface } = await import('./ShellActivitySurface')

beforeEach(() => {
  isRecording = false
  isPaused = false
  isCommandPending = true
  controllerCommand = 'finalize'
  activeMeetingId = null
  pathname = '/settings'
  currentMeeting = null
  summaries = [{
    activityId: 'summary-meeting:process', revision: 4, meetingId: 'summary-meeting', processId: 'process', status: 'processing',
    response: { status: 'processing', meetingName: 'Quarterly review', meeting_id: 'summary-meeting', start: 'process', end: null, data: null, error: null },
    error: null, reconciliationError: null,
  }]
  pauseRecording.mockClear()
  resumeRecording.mockClear()
  dismissSummary.mockClear()
})

afterAll(() => {
  mock.module('@/contexts/MeetingActivityContext', () => originalActivity)
  mock.module('@/contexts/RecordingControllerContext', () => originalController)
  mock.module('@/contexts/RecordingStateContext', () => originalRecording)
  mock.module('@/components/Sidebar/SidebarProvider', () => originalSidebar)
  mock.module('next/navigation', () => originalNavigation)
})

describe('ShellActivitySurface', () => {
  test('prioritizes finalization, active transcription, and summary over retained failures', () => {
    const renderer = create(<ShellActivitySurface />)
    const rendered = JSON.stringify(renderer.toJSON())
    expect(rendered).toContain('Saving meeting')
    expect(rendered).toContain('Active transcription')
    expect(rendered).toContain('Preparing early import')
    expect(rendered).toContain('Quarterly review')
    expect(rendered).toContain('more activities')
    expect(rendered).not.toContain('Duplicate recording failure')
    renderer.unmount()
  })

  test('pause button calls pauseRecording when recording is active and not paused', async () => {
    isRecording = true
    isPaused = false
    isCommandPending = false
    controllerCommand = null
    const renderer = create(<ShellActivitySurface />)
    const button = renderer.root.findByProps({ 'aria-label': 'Pause recording' })
    await act(async () => button.props.onClick())
    expect(pauseRecording).toHaveBeenCalledTimes(1)
    expect(resumeRecording).not.toHaveBeenCalled()
    renderer.unmount()
  })

  test('resume button calls resumeRecording when recording is paused', async () => {
    isRecording = true
    isPaused = true
    isCommandPending = false
    controllerCommand = null
    const renderer = create(<ShellActivitySurface />)
    const button = renderer.root.findByProps({ 'aria-label': 'Resume recording' })
    await act(async () => button.props.onClick())
    expect(resumeRecording).toHaveBeenCalledTimes(1)
    expect(pauseRecording).not.toHaveBeenCalled()
    renderer.unmount()
  })

  test('hides the recording card on the live workspace, which has its own controls', () => {
    isRecording = true
    isCommandPending = false
    controllerCommand = null
    activeMeetingId = 'live-meeting'
    pathname = '/meeting-details'
    currentMeeting = { id: 'live-meeting' }
    const renderer = create(<ShellActivitySurface />)
    expect(renderer.root.findAllByProps({ 'aria-label': 'Pause recording' })).toHaveLength(0)
    expect(JSON.stringify(renderer.toJSON())).not.toContain('Recording in progress')
    renderer.unmount()
  })

  test('keeps the recording card when viewing a different meeting', () => {
    isRecording = true
    isCommandPending = false
    controllerCommand = null
    activeMeetingId = 'live-meeting'
    pathname = '/meeting-details'
    currentMeeting = { id: 'other-meeting' }
    const renderer = create(<ShellActivitySurface />)
    expect(renderer.root.findAllByProps({ 'aria-label': 'Pause recording' })).not.toHaveLength(0)
    renderer.unmount()
  })

  test('dismissing an activity card hides it and reveals the next queued card', async () => {
    const renderer = create(<ShellActivitySurface />)
    expect(JSON.stringify(renderer.toJSON())).toContain('Show 4 more activities')
    const dismissButton = renderer.root.findByProps({ 'aria-label': 'Dismiss Preparing early import' })
    await act(async () => dismissButton.props.onClick())
    const rendered = JSON.stringify(renderer.toJSON())
    expect(rendered).not.toContain('Preparing early import')
    expect(rendered).toContain('Retained failure 3')
    expect(rendered).toContain('Show 3 more activities')
    renderer.unmount()
  })

  test('dismissing a failed summary hides it locally and calls dismissSummary', async () => {
    summaries = [{
      activityId: 'failed-summary:process-2', revision: 7, meetingId: 'failed-meeting', processId: 'process-2', status: 'failed',
      response: null, error: 'boom', reconciliationError: null,
    }]
    const renderer = create(<ShellActivitySurface />)
    const showMoreButton = renderer.root.findByProps({ className: 'w-full rounded-lg bg-surface-raised px-3 py-2 text-xs font-semibold text-ink-muted shadow' })
    await act(async () => showMoreButton.props.onClick())
    const dismissButton = renderer.root.findByProps({ 'aria-label': 'Dismiss Meeting summary' })
    await act(async () => dismissButton.props.onClick())
    expect(dismissSummary).toHaveBeenCalledWith('failed-meeting', 'process-2')
    expect(JSON.stringify(renderer.toJSON())).not.toContain('Meeting summary')
    renderer.unmount()
  })
})
