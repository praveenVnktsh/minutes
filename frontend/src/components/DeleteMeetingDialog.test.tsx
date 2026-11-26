import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create, type ReactTestRenderer } from 'react-test-renderer'
import type { MeetingActivity, RecordingActivity } from '@/types/meetingActivity'

const originalSidebar = { ...await import('@/components/Sidebar/SidebarProvider') }
const originalActivity = { ...await import('@/contexts/MeetingActivityContext') }
const originalDialog = { ...await import('@/components/ui/dialog') }
const originalToast = { ...await import('sonner') }

const deleteMeeting = mock(async (_meetingId: string): Promise<unknown> => ({ folder_not_removed: null, folder_error: null }))
const toastSuccess = mock((_message: string) => {})
const toastError = mock((_message: string, _options?: unknown) => {})
let recording: RecordingActivity | null = null
let activities: MeetingActivity[] = []
let summaryStatus: string | null = null

mock.module('@/components/Sidebar/SidebarProvider', () => ({
  ...originalSidebar,
  useSidebar: () => ({ deleteMeeting }),
}))
mock.module('@/contexts/MeetingActivityContext', () => ({
  ...originalActivity,
  useMeetingActivity: () => ({
    recording,
    activeMeetingId: recording?.meeting_id ?? null,
    getMeetingActivities: (meetingId: string) => activities.filter((activity) => activity.meeting_id === meetingId),
    getSummaryActivity: () => (summaryStatus ? { status: summaryStatus } : null),
  }),
}))
// Radix renders dialog content in a portal, which the test renderer cannot.
mock.module('@/components/ui/dialog', () => ({
  Dialog: ({ open, children }: { open?: boolean; children?: React.ReactNode }) => (open ? <>{children}</> : null),
  DialogContent: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  DialogHeader: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  DialogFooter: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  DialogTitle: ({ children }: { children?: React.ReactNode }) => <h2>{children}</h2>,
  DialogDescription: ({ children }: { children?: React.ReactNode }) => <p>{children}</p>,
}))
mock.module('sonner', () => ({ ...originalToast, toast: { success: toastSuccess, error: toastError } }))

const { DeleteMeetingDialog } = await import('./DeleteMeetingDialog')

afterAll(() => {
  mock.module('@/components/Sidebar/SidebarProvider', () => originalSidebar)
  mock.module('@/contexts/MeetingActivityContext', () => originalActivity)
  mock.module('@/components/ui/dialog', () => originalDialog)
  mock.module('sonner', () => originalToast)
})

const meeting = { id: 'meeting-1', title: 'Quarterly review' }
let renderer: ReactTestRenderer
const onOpenChange = mock((_open: boolean) => {})
const onDeleted = mock((_meetingId: string) => {})

beforeEach(() => {
  for (const fn of [deleteMeeting, toastSuccess, toastError, onOpenChange, onDeleted]) fn.mockClear()
  recording = null
  activities = []
  summaryStatus = null
})

async function show(target: { id: string; title: string } | null = meeting) {
  await act(async () => {
    renderer = create(<DeleteMeetingDialog meeting={target} onOpenChange={onOpenChange} onDeleted={onDeleted} />)
  })
}
const text = () => JSON.stringify(renderer.toJSON())
const buttonLabelled = (label: string) => renderer.root.findAll((node) => (
  node.type === 'button' && node.children.some((child) => typeof child === 'string' && child === label)
))[0]
const deleteButton = () => renderer.root.findByProps({ 'aria-label': 'Delete Quarterly review permanently' })

describe('DeleteMeetingDialog', () => {
  test('is closed without a meeting', async () => {
    await show(null)
    expect(renderer.toJSON()).toBeNull()
  })

  test('names the meeting and says what is deleted for good', async () => {
    await show()
    expect(text()).toContain('Quarterly review')
    expect(text()).toContain('notes, transcript and recording are permanently deleted')
    expect(text()).toContain("can't be undone")
    expect(deleteButton().props.disabled).toBe(false)
  })

  test('cancel closes without deleting', async () => {
    await show()
    await act(async () => buttonLabelled('Cancel').props.onClick())
    expect(onOpenChange).toHaveBeenCalledWith(false)
    expect(deleteMeeting).not.toHaveBeenCalled()
    expect(onDeleted).not.toHaveBeenCalled()
  })

  test('confirming deletes, closes and reports success', async () => {
    await show()
    await act(async () => deleteButton().props.onClick())
    expect(deleteMeeting).toHaveBeenCalledWith('meeting-1')
    expect(onOpenChange).toHaveBeenCalledWith(false)
    expect(onDeleted).toHaveBeenCalledWith('meeting-1')
    expect(toastSuccess).toHaveBeenCalledTimes(1)
    expect(toastError).not.toHaveBeenCalled()
  })

  test('a folder that could not be removed is shown after the delete', async () => {
    deleteMeeting.mockResolvedValueOnce({ folder_not_removed: '/Users/me/Movies/minutes/Quarterly review', folder_error: 'Permission denied' })
    await show()
    await act(async () => deleteButton().props.onClick())
    expect(toastError).toHaveBeenCalledTimes(1)
    expect(toastError.mock.calls[0][0]).toBe(
      "The meeting was deleted, but its recording folder couldn't be removed: /Users/me/Movies/minutes/Quarterly review",
    )
    expect(toastSuccess).not.toHaveBeenCalled()
    expect(onDeleted).toHaveBeenCalledWith('meeting-1')
  })

  test('a meeting still recording cannot be deleted', async () => {
    recording = { session_id: 's', meeting_id: 'meeting-1', status: 'recording', error: null }
    await show()
    expect(text()).toContain('This meeting is still recording')
    expect(deleteButton().props.disabled).toBe(true)
    await act(async () => deleteButton().props.onClick())
    expect(deleteMeeting).not.toHaveBeenCalled()
  })

  test('a meeting still transcribing cannot be deleted', async () => {
    activities = [{
      task_id: 't', meeting_id: 'meeting-1', kind: 'retranscription', status: 'transcribing', title: 'Improving transcript',
      stage: null, progress_percentage: 40, message: null, error: null, warning: null, controls_available: true, revision: 1,
    }]
    await show()
    expect(text()).toContain('Wait for transcription to finish')
    expect(deleteButton().props.disabled).toBe(true)
  })

  test('a refusal from the core is shown and the dialog stays open', async () => {
    deleteMeeting.mockRejectedValueOnce('Wait for the AI notes to finish before deleting this meeting.')
    await show()
    await act(async () => deleteButton().props.onClick())
    expect(text()).toContain('Wait for the AI notes to finish')
    expect(onOpenChange).not.toHaveBeenCalled()
    expect(onDeleted).not.toHaveBeenCalled()
  })
})
