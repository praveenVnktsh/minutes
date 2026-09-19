import { afterAll, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { create } from 'react-test-renderer'

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
    summaries: [{
      activityId: 'summary-meeting:process', revision: 4, meetingId: 'summary-meeting', processId: 'process', status: 'processing',
      response: { status: 'processing', meetingName: 'Quarterly review', meeting_id: 'summary-meeting', start: 'process', end: null, data: null, error: null },
      error: null, reconciliationError: null,
    }],
    retrySummary: async () => {}, cancelSummary: async () => true,
    cancelTranscription: async () => true, pauseTranscription: async () => true, resumeTranscription: async () => true,
  }),
}))
mock.module('@/contexts/RecordingControllerContext', () => ({
  ...originalController,
  useRecordingController: () => ({ command: 'finalize', activeMeetingId: null, isCommandPending: true, returnToRecording: async () => {}, stopRecording: async () => {} }),
}))
mock.module('@/contexts/RecordingStateContext', () => ({ ...originalRecording, useRecordingState: () => ({ isRecording: false, isPaused: false, isProcessing: false, isSaving: true }) }))
mock.module('@/components/Sidebar/SidebarProvider', () => ({ ...originalSidebar, useSidebar: () => ({ currentMeeting: null }) }))
mock.module('next/navigation', () => ({ ...originalNavigation, usePathname: () => '/settings' }))

const { ShellActivitySurface } = await import('./ShellActivitySurface')

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
})
