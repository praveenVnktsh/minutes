import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test'
import { act, create, type ReactTestRenderer } from 'react-test-renderer'
import type { MeetingModelProvenance } from '@/hooks/useModelProvenance'

const originalHook = { ...await import('@/hooks/useModelProvenance') }

let provenance: MeetingModelProvenance | null = null
const useModelProvenance = mock((_meetingId: string | undefined) => provenance)

mock.module('@/hooks/useModelProvenance', () => ({ ...originalHook, useModelProvenance }))

const { ModelProvenanceNote } = await import('./ModelProvenanceNote')

let renderer: ReactTestRenderer | undefined

async function render() {
  await act(async () => {
    renderer = create(<ModelProvenanceNote meetingId="meeting-1" />)
  })
}

beforeEach(() => {
  provenance = null
  useModelProvenance.mockClear()
})

afterEach(async () => {
  if (renderer) {
    const current = renderer
    renderer = undefined
    await act(async () => current.unmount())
  }
})

afterAll(() => mock.restore())

describe('ModelProvenanceNote', () => {
  test('renders nothing while provenance is unknown', async () => {
    provenance = null
    await render()
    expect(renderer?.toJSON()).toBeNull()
  })

  test('renders nothing when every leg is null', async () => {
    provenance = { transcription: null, diarization: null, summary: null }
    await render()
    expect(renderer?.toJSON()).toBeNull()
  })

  test('reports only the legs that are known', async () => {
    provenance = {
      transcription: { provider: 'whisper', model: 'large-v3-turbo' },
      diarization: null,
      summary: null,
    }
    await render()
    expect(JSON.stringify(renderer?.toJSON())).toContain('Transcribed by Whisper large-v3-turbo')
  })

  test('joins all three legs with human-friendly provider labels', async () => {
    provenance = {
      transcription: { provider: 'parakeet', model: 'tdt-0.6b' },
      diarization: { engine: 'pyannote', segmentation_model: 'segmentation-3.0', embedding_model: 'wespeaker' },
      summary: { provider: 'claude', model: 'sonnet-5' },
    }
    await render()
    expect(JSON.stringify(renderer?.toJSON())).toContain(
      'Transcribed by Parakeet tdt-0.6b · Speakers by pyannote (segmentation-3.0) · Enhanced by Claude sonnet-5',
    )
  })

  test('falls back to the raw id for an unmapped provider or engine', async () => {
    provenance = {
      transcription: null,
      diarization: { engine: 'custom-engine', segmentation_model: null, embedding_model: null },
      summary: { provider: 'some-new-provider', model: 'v1' },
    }
    await render()
    expect(JSON.stringify(renderer?.toJSON())).toContain('Speakers by custom-engine · Enhanced by some-new-provider v1')
  })
})
