import { describe, expect, test } from 'bun:test'
import {
  resumedMeetingTitle,
  shouldDiscardShortMeeting,
  stampResumeTarget,
  type ShortMeetingCheck,
  type StampResumeTargetOptions,
} from '@/contexts/RecordingControllerContext'

const noopWait = async () => {}

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

describe('stampResumeTarget', () => {
  test('stamps the new recovery id with the resume target', async () => {
    const calls: Array<[string, string]> = []
    const result = await stampResumeTarget({
      resumeMeetingId: 'meeting-resume',
      previousRecoveryId: 'stale-id',
      readRecoveryId: () => 'fresh-id',
      stamp: async (recoveryId, resumeOfMeetingId) => {
        calls.push([recoveryId, resumeOfMeetingId])
        return true
      },
      wait: noopWait,
    })
    expect(result).toBe('fresh-id')
    expect(calls).toEqual([['fresh-id', 'meeting-resume']])
  })

  test('a fresh start (no resume target) never calls stamp', async () => {
    let called = false
    const result = await stampResumeTarget({
      resumeMeetingId: null,
      previousRecoveryId: 'stale-id',
      readRecoveryId: () => {
        called = true
        return 'fresh-id'
      },
      stamp: async () => true,
      wait: noopWait,
    })
    expect(result).toBeNull()
    expect(called).toBe(false)
  })

  test('a stale id equal to previousRecoveryId is not stamped; it waits for a new one', async () => {
    const ids = ['stale-id', 'stale-id', 'fresh-id']
    const calls: Array<[string, string]> = []
    const result = await stampResumeTarget({
      resumeMeetingId: 'meeting-resume',
      previousRecoveryId: 'stale-id',
      readRecoveryId: () => ids.shift() ?? 'fresh-id',
      stamp: async (recoveryId, resumeOfMeetingId) => {
        calls.push([recoveryId, resumeOfMeetingId])
        return true
      },
      wait: noopWait,
    })
    expect(result).toBe('fresh-id')
    expect(calls).toEqual([['fresh-id', 'meeting-resume']])
  })

  test('stamp returning false (record not created yet) retries then succeeds', async () => {
    let call = 0
    const result = await stampResumeTarget({
      resumeMeetingId: 'meeting-resume',
      previousRecoveryId: null,
      readRecoveryId: () => 'fresh-id',
      stamp: async () => {
        call += 1
        return call >= 3
      },
      wait: noopWait,
    })
    expect(result).toBe('fresh-id')
    expect(call).toBe(3)
  })

  test('stamp throwing is logged and resolves null without throwing', async () => {
    const warn = console.warn
    const warnings: unknown[] = []
    console.warn = (...args: unknown[]) => { warnings.push(args) }
    try {
      const result = await stampResumeTarget({
        resumeMeetingId: 'meeting-resume',
        previousRecoveryId: null,
        readRecoveryId: () => 'fresh-id',
        stamp: async () => { throw new Error('boom') },
        wait: noopWait,
      })
      expect(result).toBeNull()
      expect(warnings.length).toBe(1)
    } finally {
      console.warn = warn
    }
  })

  test('gives up after attempts', async () => {
    const warn = console.warn
    const warnings: unknown[] = []
    console.warn = (...args: unknown[]) => { warnings.push(args) }
    let calls = 0
    try {
      const options: StampResumeTargetOptions = {
        resumeMeetingId: 'meeting-resume',
        previousRecoveryId: 'stale-id',
        readRecoveryId: () => 'stale-id',
        stamp: async () => { calls += 1; return true },
        wait: noopWait,
        attempts: 3,
        intervalMs: 0,
      }
      const result = await stampResumeTarget(options)
      expect(result).toBeNull()
      expect(calls).toBe(0)
      expect(warnings.length).toBe(1)
    } finally {
      console.warn = warn
    }
  })
})
