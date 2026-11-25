import { afterAll, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create, type ReactTestRenderer } from 'react-test-renderer'

// Restore the real modules afterwards so these mocks cannot leak into a later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) }
const originalEvent = { ...(await import('@tauri-apps/api/event')) }
const originalOnboarding = { ...(await import('@/contexts/OnboardingContext')) }
const originalContainer = { ...(await import('../OnboardingContainer')) }
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@tauri-apps/api/event', () => originalEvent)
  mock.module('@/contexts/OnboardingContext', () => originalOnboarding)
  mock.module('../OnboardingContainer', () => originalContainer)
})

const invoked: string[] = []
mock.module('@tauri-apps/api/core', () => ({
  invoke: async (command: string) => {
    invoked.push(command)
    return undefined
  },
}))
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen: async () => () => {} }))
mock.module('../OnboardingContainer', () => ({
  OnboardingContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
}))

let goNextCalls = 0
const progress = { percent: 0, downloadedMb: 0, totalMb: 0, downloadedBytes: 0, totalBytes: 0, speedMbps: 0, etaSeconds: null }
let onboarding: Record<string, unknown> = {}
mock.module('@/contexts/OnboardingContext', () => ({
  ...originalOnboarding,
  useOnboarding: () => onboarding,
}))

function onboardingState(transcriptionSupported: boolean) {
  return {
    goNext: () => { goNextCalls += 1 },
    selectedSummaryModel: 'gemma',
    speechDownloaded: false,
    transcriptionSupported,
    setSpeechDownloaded: () => {},
    speechDownloadState: 'idle',
    speechDownloadError: undefined,
    speechProgressInfo: progress,
    speechSizeBytes: null,
    summaryModelDownloaded: true,
    summaryModelProgressInfo: progress,
    summaryModelSizeBytes: null,
    isBackgroundDownloading: false,
    startBackgroundDownloads: async () => {},
    retrySpeechDownload: async () => {},
    retrySummaryDownload: async () => {},
    cancelSpeechDownload: async () => {},
    cancelSummaryDownload: async () => {},
  }
}

const { DownloadProgressStep } = await import('./DownloadProgressStep')

const UNSUPPORTED = 'Transcription needs macOS 15 or later. Recording still works; update macOS to transcribe meetings.'

async function render(transcriptionSupported: boolean): Promise<ReactTestRenderer> {
  onboarding = onboardingState(transcriptionSupported)
  let renderer!: ReactTestRenderer
  await act(async () => {
    renderer = create(<DownloadProgressStep />)
  })
  return renderer
}

function continueButton(renderer: ReactTestRenderer) {
  return renderer.root.findAll(
    (node) => typeof node.props.className === 'string' && node.props.className.includes('bg-brand') && typeof node.props.onClick === 'function',
  )[0]
}

describe('DownloadProgressStep and macOS support', () => {
  test('waits for the speech models on a supported macOS', async () => {
    const renderer = await render(true)
    const json = JSON.stringify(renderer.toJSON())
    expect(json).toContain('Speech Models')
    expect(json).not.toContain(UNSUPPORTED)
    expect(continueButton(renderer).props.disabled).toBe(true)
  })

  test('skips the speech download before macOS 15 and lets the user continue', async () => {
    goNextCalls = 0
    invoked.length = 0
    const renderer = await render(false)
    const json = JSON.stringify(renderer.toJSON())
    expect(json).toContain(UNSUPPORTED)
    expect(json).not.toContain('Speech Models')
    const button = continueButton(renderer)
    expect(button.props.disabled).toBe(false)
    await act(async () => {
      await button.props.onClick()
    })
    expect(goNextCalls).toBe(1)
    expect(invoked).not.toContain('speech_models_status')
  })
})
