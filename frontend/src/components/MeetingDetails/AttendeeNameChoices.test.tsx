import { describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'
import { AttendeeNameChoices, attendeeNames } from './AttendeeNameChoices'
import { SpeakerEditor } from '../VirtualizedTranscriptView'

const text = (node: React.ReactElement) => JSON.stringify(create(node).toJSON())

describe('attendee names for speakers', () => {
  test('are trimmed and deduplicated in order', () => {
    expect(attendeeNames([{ name: ' Priya Shah ' }, { name: 'Dev Patel' }, { name: 'priya shah' }, { name: ' ' }])).toEqual([
      'Priya Shah',
      'Dev Patel',
    ])
  })

  test('render nothing without attendees', () => {
    expect(create(<AttendeeNameChoices names={[]} onPick={() => {}} />).toJSON()).toBeNull()
  })

  test('come first in the speaker picker, and one click renames the speaker', () => {
    const onRename = mock((_speakerId: string, _name: string) => {})
    const onDone = mock(() => {})
    const props = {
      transcriptId: 't1',
      label: 'speaker_01',
      currentId: 'speaker_01',
      attendeeNames: ['Dev Patel', 'Priya Shah'],
      speakerOptions: [
        { speaker_id: 'mic', display_name: 'You' },
        { speaker_id: 'speaker_01', display_name: 'speaker_01' },
      ],
      onSpeakerChange: () => {},
      onRenameSpeaker: onRename,
      onDone,
    }
    const json = text(<SpeakerEditor {...props} />)
    const attendee = json.indexOf('Dev Patel')
    expect(attendee).toBeGreaterThan(-1)
    expect(attendee).toBeLessThan(json.indexOf('Priya Shah'))
    expect(json.indexOf('Priya Shah')).toBeLessThan(json.indexOf('Speaker name'))
    expect(json.indexOf('Speaker name')).toBeLessThan(json.indexOf('Assign this line to'))
    expect(json.indexOf('Assign this line to')).toBeLessThan(json.indexOf('"You"'))

    const renderer = create(<SpeakerEditor {...props} />)
    const chip = renderer.root.findAll(
      (node) => node.type === 'button' && node.children.includes('Dev Patel'),
    )[0]
    act(() => chip.props.onClick())
    expect(onRename).toHaveBeenCalledWith('speaker_01', 'Dev Patel')
    expect(onDone).toHaveBeenCalled()
  })

  test('are not offered when the speaker cannot be renamed', () => {
    const json = text(
      <SpeakerEditor
        transcriptId="t1"
        label="You"
        currentId="mic"
        attendeeNames={['Dev Patel']}
        onDone={() => {}}
      />,
    )
    expect(json).not.toContain('Dev Patel')
  })
})
