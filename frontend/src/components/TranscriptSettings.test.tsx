import { afterAll, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create, type ReactTestRenderer } from 'react-test-renderer'
import type { SpeechModelsStatus } from '@/lib/speechModels'

// Restore the real modules afterwards so these mocks cannot leak into a later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) }
const originalEvent = { ...(await import('@tauri-apps/api/event')) }
const originalLiveSettings = { ...(await import('@/components/LiveTranscriptionModelSettings')) }
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@tauri-apps/api/event', () => originalEvent)
  mock.module('@/components/LiveTranscriptionModelSettings', () => originalLiveSettings)
})

const MIB = 1024 * 1024
let speechStatus: SpeechModelsStatus

function missingStatus(extra: Partial<SpeechModelsStatus> = {}): SpeechModelsStatus {
  return {
    models: [
      { id: 'nemotron', kind: 'live-asr', displayName: 'Nemotron Streaming 0.6B', sizeBytes: 600 * MIB, downloaded: false, downloading: false },
      { id: 'parakeet', kind: 'final-asr', displayName: 'Parakeet TDT 0.6B v2', sizeBytes: 450 * MIB, downloaded: false, downloading: false },
      { id: 'silero', kind: 'vad', displayName: 'Silero VAD', sizeBytes: 2 * MIB, downloaded: false, downloading: false },
      { id: 'pyannote', kind: 'diarization', displayName: 'Pyannote speakers', sizeBytes: 30 * MIB, downloaded: false, downloading: false },
    ],
    liveReady: false,
    batchReady: false,
    diarizationReady: false,
    ...extra,
  }
}

mock.module('@tauri-apps/api/core', () => ({
  invoke: async (command: string) => {
    if (command === 'speech_models_status') return speechStatus
    return undefined
  },
}))
mock.module('@tauri-apps/api/event', () => ({
  ...originalEvent,
  listen: async () => () => {},
}))
// Their own file covers them; here they only need to render nothing.
mock.module('@/components/LiveTranscriptionModelSettings', () => ({
  ...originalLiveSettings,
  LiveEngineSettings: () => null,
  FinalPassTimingSettings: () => null,
}))

const { TranscriptSettings } = await import('./TranscriptSettings')

const UNSUPPORTED = 'Transcription needs macOS 15 or later. Recording still works; update macOS to transcribe meetings.'

function text(renderer: ReactTestRenderer): string {
  return JSON.stringify(renderer.toJSON())
}

async function render(status: SpeechModelsStatus): Promise<ReactTestRenderer> {
  speechStatus = status
  let renderer!: ReactTestRenderer
  await act(async () => {
    renderer = create(<TranscriptSettings />)
  })
  return renderer
}

describe('TranscriptSettings and macOS support', () => {
  test('offers the downloads on a supported macOS', async () => {
    const renderer = await render(missingStatus({ osSupported: true }))
    expect(text(renderer)).toContain('Download all')
    expect(text(renderer)).not.toContain(UNSUPPORTED)
  })

  test('an older backend without osSupported still offers the downloads', async () => {
    const renderer = await render(missingStatus())
    expect(text(renderer)).toContain('Download all')
    expect(text(renderer)).not.toContain(UNSUPPORTED)
  })

  test('shows the macOS 15 notice instead of any download before macOS 15', async () => {
    const renderer = await render(missingStatus({ osSupported: false }))
    expect(text(renderer)).toContain(UNSUPPORTED)
    expect(text(renderer)).not.toContain('Download all')
    expect(text(renderer)).not.toContain('Parakeet TDT 0.6B v2')
  })
})
