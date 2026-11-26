import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create } from 'react-test-renderer'

const originalCore = { ...await import('@tauri-apps/api/core') }
const originalEvent = { ...await import('@tauri-apps/api/event') }

let attending: { name: string }[] = []
const listeners = new Map<string, (event: { payload: unknown }) => void>()
const invoke = mock(async (command: string) => (command === 'meeting_attendees_get' ? attending : null))
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }))
mock.module('@tauri-apps/api/event', () => ({
  ...originalEvent,
  listen: async (event: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(event, handler)
    return () => listeners.delete(event)
  },
}))

const { useMeetingAttendeeNames, MEETING_ATTENDEES_UPDATED_EVENT } = await import('./AttendeeNameChoices')

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@tauri-apps/api/event', () => originalEvent)
})

beforeEach(() => {
  invoke.mockClear()
  listeners.clear()
  attending = [{ name: 'Dev Patel' }]
})

let seen: string[] = []
function Probe({ meetingId }: { meetingId: string }) {
  seen = useMeetingAttendeeNames(meetingId)
  return null
}

describe('useMeetingAttendeeNames', () => {
  test('reloads when this meeting\'s attendees change, and only then', async () => {
    await act(async () => {
      create(<Probe meetingId="m1" />)
    })
    expect(seen).toEqual(['Dev Patel'])
    expect(invoke).toHaveBeenCalledTimes(1)

    attending = [{ name: 'Dev Patel' }, { name: 'Priya Shah' }]
    await act(async () => {
      listeners.get(MEETING_ATTENDEES_UPDATED_EVENT)?.({ payload: { meeting_id: 'other' } })
    })
    expect(invoke).toHaveBeenCalledTimes(1)
    expect(seen).toEqual(['Dev Patel'])

    await act(async () => {
      listeners.get(MEETING_ATTENDEES_UPDATED_EVENT)?.({ payload: { meeting_id: 'm1' } })
    })
    expect(invoke).toHaveBeenCalledTimes(2)
    expect(seen).toEqual(['Dev Patel', 'Priya Shah'])
  })
})
