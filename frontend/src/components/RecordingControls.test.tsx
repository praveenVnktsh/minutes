import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'

const retryFeedback = mock(async () => {})
let feedbackManagedGlobally = true

const originalController = { ...await import('@/contexts/RecordingControllerContext') }
const originalRecording = { ...await import('@/contexts/RecordingStateContext') }
const originalTooltip = { ...await import('@/components/ui/tooltip') }

mock.module('@/contexts/RecordingControllerContext', () => ({
  ...originalController,
  useRecordingController: () => ({
    activeMeetingId: 'meeting-a', sessionId: 'session-a', command: null, isCommandPending: false,
    feedback: { kind: 'capture', title: 'Recording could not be paused', message: 'device unavailable' },
    feedbackManagedGlobally,
    startRecording: async () => {}, stopRecording: async () => {}, pauseRecording: async () => {}, resumeRecording: async () => {},
    returnToRecording: async () => {}, dismissFeedback: () => {}, canRetryFeedback: true, retryFeedback,
    openFeedbackSettings: async () => {}, recovery: {}, isRecoveryOpen: false, openRecovery: () => {}, closeRecovery: () => {},
  }),
}))
mock.module('@/contexts/RecordingStateContext', () => ({
  ...originalRecording,
  useRecordingState: () => ({ isPaused: false, recordingDuration: 12, activeDuration: 9 }),
}))
mock.module('@/components/ui/tooltip', () => ({
  ...originalTooltip,
  TooltipProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}))

const { RecordingControls } = await import('./RecordingControls')
const { RecordingControllerFeedback } = await import('./RecordingControllerFeedback')

const controls = <RecordingControls
  isRecording
  barHeights={[]}
  onRecordingStop={() => {}}
  onRecordingStart={() => {}}
  onTranscriptReceived={() => {}}
  isRecordingDisabled={false}
  isParentProcessing={false}
/>

beforeEach(() => {
  feedbackManagedGlobally = true
  retryFeedback.mockClear()
})

afterAll(() => {
  mock.module('@/contexts/RecordingControllerContext', () => originalController)
  mock.module('@/contexts/RecordingStateContext', () => originalRecording)
  mock.module('@/components/ui/tooltip', () => originalTooltip)
})

describe('recording feedback composition', () => {
  test('renders one global error and one retry when the root owns feedback', async () => {
    const renderer = create(<>{controls}<RecordingControllerFeedback /></>)
    const rendered = JSON.stringify(renderer.toJSON())
    expect(rendered.match(/Recording could not be paused/g)).toHaveLength(1)
    expect(renderer.root.findAllByProps({ children: 'Retry' })).toHaveLength(1)
    await act(async () => renderer.root.findByProps({ children: 'Retry' }).props.onClick())
    expect(retryFeedback).toHaveBeenCalledTimes(1)
    renderer.unmount()
  })

  test('shows activeDuration (excluding pauses) rather than recordingDuration', () => {
    const renderer = create(controls)
    const rendered = JSON.stringify(renderer.toJSON())
    // activeDuration: 9 -> "00:09"; recordingDuration: 12 -> "00:12"
    expect(rendered).toContain('00:09')
    expect(rendered).not.toContain('00:12')
    renderer.unmount()
  })

  test('keeps inline feedback for standalone controls by default', () => {
    feedbackManagedGlobally = false
    const renderer = create(controls)
    expect(JSON.stringify(renderer.toJSON())).toContain('Recording could not be paused')
    expect(renderer.root.findAllByProps({ children: 'Retry' })).toHaveLength(1)
    renderer.unmount()
  })
})
