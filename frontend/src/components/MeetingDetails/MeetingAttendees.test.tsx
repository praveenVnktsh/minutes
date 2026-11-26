import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create, type ReactTestInstance, type ReactTestRenderer } from 'react-test-renderer'
import type { Person } from '@/lib/people'

const originalCore = { ...await import('@tauri-apps/api/core') }
const originalEvent = { ...await import('@tauri-apps/api/event') }

function person(id: string, name: string, extra: Partial<Person> = {}): Person {
  return {
    id, name, email: null, role: null, team: null, aliases: [], projects: [], notes: null,
    is_me: false, locked_fields: [], sources: [], ...extra,
  }
}

let directory: Person[] = []
let attending: string[] = []
const listeners = new Map<string, (event: { payload: unknown }) => void>()

const attendeesOf = () => directory.filter((p) => attending.includes(p.id))
const invoke = mock(async (command: string, args: Record<string, unknown> = {}) => {
  switch (command) {
    case 'meeting_attendees_get': return attendeesOf()
    case 'people_list': return directory
    case 'people_create': {
      const created = person(`p${directory.length + 1}`, String((args.edit as { name: string }).name))
      directory = [...directory, created]
      return created
    }
    case 'meeting_attendees_add':
      attending = [...attending, String(args.personId)]
      return attendeesOf()
    case 'meeting_attendees_remove':
      attending = attending.filter((id) => id !== args.personId)
      return attendeesOf()
    default: return null
  }
})
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }))
mock.module('@tauri-apps/api/event', () => ({
  ...originalEvent,
  listen: async (event: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(event, handler)
    return () => listeners.delete(event)
  },
}))

const { MeetingAttendees, personDetails } = await import('./MeetingAttendees')

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@tauri-apps/api/event', () => originalEvent)
})

beforeEach(() => {
  invoke.mockClear()
  listeners.clear()
  directory = [
    person('p1', 'Priya Shah', { role: 'Product manager', team: 'Growth', projects: [{ title: 'Offline mode', source: 'linear' }] }),
    person('p2', 'Dev Patel', { email: 'dev@acme.com', aliases: ['Dev'] }),
  ]
  attending = ['p1']
})

const text = (renderer: ReactTestRenderer) => JSON.stringify(renderer.toJSON())
const buttons = (renderer: ReactTestRenderer) => renderer.root.findAllByType('button')
const byLabel = (renderer: ReactTestRenderer, label: string) =>
  buttons(renderer).find((b) => b.props['aria-label'] === label) as ReactTestInstance
const textOf = (node: ReactTestInstance | string): string =>
  typeof node === 'string' ? node : node.children.map(textOf).join('')
const optionWith = (renderer: ReactTestRenderer, content: string) =>
  buttons(renderer).find((b) => b.props.role === 'option' && textOf(b).includes(content)) as ReactTestInstance

async function render() {
  let renderer: ReactTestRenderer
  await act(async () => { renderer = create(<MeetingAttendees meetingId="m1" />) })
  return renderer!
}

describe('MeetingAttendees', () => {
  test('renders a chip per attendee with details on hover', async () => {
    const renderer = await render()
    expect(invoke).toHaveBeenCalledWith('meeting_attendees_get', { meetingId: 'm1' })
    expect(text(renderer)).toContain('Priya Shah')
    expect(text(renderer)).not.toContain('Dev Patel')
    const chip = renderer.root.findAll((node) => node.type === 'span' && typeof node.props.title === 'string')[0]
    expect(chip.props.title).toContain('Product manager, Growth')
    expect(chip.props.title).toContain('Projects: Offline mode')
    expect(byLabel(renderer, 'Add attendee')).toBeDefined()
    await act(async () => renderer.unmount())
  })

  test('shows only a subtle affordance when nobody is attending', async () => {
    attending = []
    const renderer = await render()
    expect(byLabel(renderer, 'Add attendees')).toBeDefined()
    expect(text(renderer)).toContain('Add attendees')
    await act(async () => renderer.unmount())
  })

  test('adds an existing person from the picker', async () => {
    const renderer = await render()
    await act(async () => { byLabel(renderer, 'Add attendee').props.onClick() })
    expect(invoke).toHaveBeenCalledWith('people_list')
    // Priya already attends, so only Dev is offered.
    expect(optionWith(renderer, 'Priya Shah')).toBeUndefined()
    const input = renderer.root.findByProps({ 'aria-label': 'Search people' })
    await act(async () => { input.props.onChange({ target: { value: 'dev' } }) })
    await act(async () => { await optionWith(renderer, 'Dev Patel').props.onClick() })
    expect(invoke).toHaveBeenCalledWith('meeting_attendees_add', { meetingId: 'm1', personId: 'p2' })
    expect(text(renderer)).toContain('Dev Patel')
    expect(renderer.root.findAllByProps({ 'aria-label': 'Search people' })).toHaveLength(0)
    await act(async () => renderer.unmount())
  })

  test('creates a new person from the typed name', async () => {
    const renderer = await render()
    await act(async () => { byLabel(renderer, 'Add attendee').props.onClick() })
    const input = renderer.root.findByProps({ 'aria-label': 'Search people' })
    await act(async () => { input.props.onChange({ target: { value: 'Ana Ruiz' } }) })
    await act(async () => { await optionWith(renderer, "Create 'Ana Ruiz'").props.onClick() })
    expect(invoke).toHaveBeenCalledWith('people_create', { edit: { name: 'Ana Ruiz' } })
    expect(invoke).toHaveBeenCalledWith('meeting_attendees_add', { meetingId: 'm1', personId: 'p3' })
    expect(text(renderer)).toContain('Ana Ruiz')
    await act(async () => renderer.unmount())
  })

  test('removes an attendee', async () => {
    const renderer = await render()
    await act(async () => { await byLabel(renderer, 'Remove Priya Shah').props.onClick() })
    expect(invoke).toHaveBeenCalledWith('meeting_attendees_remove', { meetingId: 'm1', personId: 'p1' })
    expect(text(renderer)).not.toContain('Priya Shah')
    await act(async () => renderer.unmount())
  })

  test('reloads when calendar linking finishes for this meeting', async () => {
    attending = []
    const renderer = await render()
    attending = ['p2']
    await act(async () => { listeners.get('meeting-attendees-updated')?.({ payload: { meeting_id: 'other' } }) })
    expect(text(renderer)).not.toContain('Dev Patel')
    await act(async () => { listeners.get('meeting-attendees-updated')?.({ payload: { meeting_id: 'm1' } }) })
    expect(text(renderer)).toContain('Dev Patel')
    await act(async () => renderer.unmount())
  })

  test('personDetails lists role, team, projects and email', () => {
    expect(personDetails(directory[1])).toBe('Dev Patel\ndev@acme.com')
  })
})
