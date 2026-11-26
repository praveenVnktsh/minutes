import { describe, expect, test } from 'bun:test'
import type { MeetingActivity } from '@/types/meetingActivity'
import {
  DELETE_BLOCKED_RECORDING,
  DELETE_BLOCKED_SUMMARY,
  DELETE_BLOCKED_TRANSCRIBING,
  folderNotRemovedMessage,
  meetingDeleteBlockedReason,
} from './meetingDeletion'

const task = (meetingId: string, status: MeetingActivity['status']): MeetingActivity => ({
  task_id: `${meetingId}-${status}`, meeting_id: meetingId, kind: 'retranscription', status, title: 'Improving transcript',
  stage: null, progress_percentage: null, message: null, error: null, warning: null, controls_available: false, revision: 1,
})

describe('meetingDeleteBlockedReason', () => {
  test('a quiet meeting can be deleted', () => {
    expect(meetingDeleteBlockedReason('m', { recording: null, activities: [task('m', 'ready'), task('m', 'failed')] })).toBeNull()
  })

  test('the meeting being recorded is blocked, others are not', () => {
    const recording = { session_id: 's', meeting_id: 'm', status: 'saving' as const, error: null }
    expect(meetingDeleteBlockedReason('m', { recording, activities: [] })).toBe(DELETE_BLOCKED_RECORDING)
    expect(meetingDeleteBlockedReason('other', { recording, activities: [] })).toBeNull()
  })

  test('a queued or running transcription blocks', () => {
    expect(meetingDeleteBlockedReason('m', { recording: null, activities: [task('m', 'queued')] })).toBe(DELETE_BLOCKED_TRANSCRIBING)
    expect(meetingDeleteBlockedReason('m', { recording: null, activities: [task('other', 'transcribing')] })).toBeNull()
  })

  test('AI notes being written block until they finish', () => {
    expect(meetingDeleteBlockedReason('m', { recording: null, activities: [], summaryStatus: 'processing' })).toBe(DELETE_BLOCKED_SUMMARY)
    expect(meetingDeleteBlockedReason('m', { recording: null, activities: [], summaryStatus: 'completed' })).toBeNull()
  })
})

describe('folderNotRemovedMessage', () => {
  test('names the folder left behind', () => {
    expect(folderNotRemovedMessage({ folder_not_removed: '/x/Standup', folder_error: 'busy' }))
      .toBe("The meeting was deleted, but its recording folder couldn't be removed: /x/Standup")
    expect(folderNotRemovedMessage({ folder_not_removed: null, folder_error: null })).toBeNull()
    expect(folderNotRemovedMessage(null)).toBeNull()
  })
})
