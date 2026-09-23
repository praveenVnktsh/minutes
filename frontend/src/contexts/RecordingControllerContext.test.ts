import { describe, expect, test } from 'bun:test'
import {
  resumedMeetingTitle,
  shouldDiscardShortMeeting,
  type ShortMeetingCheck,
} from '@/contexts/RecordingControllerContext'

const shortEmpty: ShortMeetingCheck = {
  resumed: false,
  minimumSeconds: 10,
  recordingSeconds: 3,
  transcriptSeconds: 0,
  hasNotes: false,
  hasTranscript: false,
}

describe('resumedMeetingTitle', () => {
  test('keeps the saved title of the meeting being resumed', () => {
    expect(resumedMeetingTitle('Weekly sync', () => 'Meeting fallback')).toBe('Weekly sync')
  })

  test('falls back only when the saved meeting has no title', () => {
    expect(resumedMeetingTitle('', () => 'Meeting fallback')).toBe('Meeting fallback')
    expect(resumedMeetingTitle('   ', () => 'Meeting fallback')).toBe('Meeting fallback')
    expect(resumedMeetingTitle(null, () => 'Meeting fallback')).toBe('Meeting fallback')
    expect(resumedMeetingTitle(undefined, () => 'Meeting fallback')).toBe('Meeting fallback')
  })
})

describe('shouldDiscardShortMeeting', () => {
  test('discards a short, empty new meeting', () => {
    expect(shouldDiscardShortMeeting(shortEmpty)).toBe(true)
  })

  test('never discards a resumed meeting, however short the new session', () => {
    expect(shouldDiscardShortMeeting({ ...shortEmpty, resumed: true })).toBe(false)
  })

  test('keeps meetings that are long enough or have content', () => {
    expect(shouldDiscardShortMeeting({ ...shortEmpty, recordingSeconds: 12 })).toBe(false)
    expect(shouldDiscardShortMeeting({ ...shortEmpty, transcriptSeconds: 11 })).toBe(false)
    expect(shouldDiscardShortMeeting({ ...shortEmpty, hasNotes: true })).toBe(false)
    expect(shouldDiscardShortMeeting({ ...shortEmpty, hasTranscript: true })).toBe(false)
  })

  test('keeps meetings when the duration is unknown or the minimum is disabled', () => {
    expect(shouldDiscardShortMeeting({ ...shortEmpty, recordingSeconds: null })).toBe(false)
    expect(shouldDiscardShortMeeting({ ...shortEmpty, minimumSeconds: 0 })).toBe(false)
  })
})
