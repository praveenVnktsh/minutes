import { describe, expect, test } from 'bun:test'
import React from 'react'
import { create } from 'react-test-renderer'
import { TranscriptStatusLine } from './TranscriptPanel'

const speakers = <p>Speakers have not been identified for this meeting.</p>

describe('TranscriptStatusLine', () => {
  test('shows only the most important status', () => {
    const text = (node: React.ReactElement) => JSON.stringify(create(node).toJSON())
    const all = text(<TranscriptStatusLine playerError="Audio missing" locked speakerNotice={speakers} />)
    expect(all).toContain('Audio missing')
    expect(all).not.toContain('locked')
    expect(all).not.toContain('Speakers')

    const locked = text(<TranscriptStatusLine locked speakerNotice={speakers} />)
    expect(locked).toContain('Transcript is locked while the AI notes are written.')
    expect(locked).not.toContain('Speakers')

    expect(text(<TranscriptStatusLine locked={false} speakerNotice={speakers} />)).toContain('Speakers have not been identified')
    expect(create(<TranscriptStatusLine locked={false} speakerNotice={null} />).toJSON()).toBeNull()
  })
})
