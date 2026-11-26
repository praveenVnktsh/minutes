import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create, type ReactTestInstance, type ReactTestRenderer } from 'react-test-renderer'
import type { PeopleSourceStatus, Person } from '@/lib/people'

const originalCore = { ...(await import('@tauri-apps/api/core')) }
const originalEvent = { ...(await import('@tauri-apps/api/event')) }
const originalDialog = { ...(await import('@/components/ui/dialog')) }
const originalToast = { ...(await import('sonner')) }

const toastSuccess = mock((_message: string, _options?: unknown) => {})
const toastError = mock((_message: string, _options?: unknown) => {})
const eventHandlers = new Map<string, (event: { payload: unknown }) => void>()

mock.module('@tauri-apps/api/event', () => ({
  ...originalEvent,
  listen: async (event: string, handler: (event: { payload: unknown }) => void) => {
    eventHandlers.set(event, handler)
    return () => eventHandlers.delete(event)
  },
}))
// Radix renders dialog content in a portal, which the test renderer cannot.
mock.module('@/components/ui/dialog', () => ({
  Dialog: ({ open, children }: { open?: boolean; children?: React.ReactNode }) => (open ? <>{children}</> : null),
  DialogContent: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  DialogHeader: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  DialogFooter: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  DialogTitle: ({ children }: { children?: React.ReactNode }) => <h2>{children}</h2>,
  DialogDescription: ({ children }: { children?: React.ReactNode }) => <p>{children}</p>,
}))
mock.module('sonner', () => ({ ...originalToast, toast: { success: toastSuccess, error: toastError } }))

function status(source: 'linear' | 'notion', patch: Partial<PeopleSourceStatus> = {}): PeopleSourceStatus {
  return {
    source,
    connected: false,
    workspace: null,
    last_synced_at: null,
    last_attempt_at: null,
    last_error: null,
    last_count: null,
    notion: null,
    ...patch,
  }
}

function person(id: string, name: string, patch: Partial<Person> = {}): Person {
  return {
    id,
    name,
    email: null,
    role: null,
    team: null,
    aliases: [],
    projects: [],
    notes: null,
    is_me: false,
    locked_fields: [],
    sources: [],
    ...patch,
  }
}

let statuses: PeopleSourceStatus[]
let people: Person[]
let syncedPeople: Person[]
let connectError: string | null = null
const calls: Array<{ command: string; args?: Record<string, unknown> }> = []

mock.module('@tauri-apps/api/core', () => ({
  ...originalCore,
  invoke: async (command: string, args?: Record<string, unknown>) => {
    calls.push({ command, args })
    switch (command) {
      case 'people_sync_status':
        return statuses
      case 'people_list':
        return people
      case 'people_connect':
        if (connectError) throw connectError
        statuses = statuses.map((s) => (s.source === args!.source ? { ...s, connected: true, workspace: 'Acme' } : s))
        return { test: { workspace: 'Acme', user_count: 12 }, statuses }
      case 'people_test_connection':
        return { workspace: 'Acme', user_count: 12 }
      case 'people_sync_now':
        people = syncedPeople
        statuses = statuses.map((s) => (s.source === args!.source ? { ...s, last_synced_at: '2026-11-25T10:00:00Z', last_count: syncedPeople.length } : s))
        return [{ source: args!.source, synced: syncedPeople.length, failed: 0, error: null }]
      case 'people_disconnect':
        statuses = statuses.map((s) => (s.source === args!.source ? status(s.source) : s))
        people = []
        return 1
      case 'people_notion_databases':
        return [{
          id: 'db1',
          title: 'Team directory',
          url: null,
          properties: [{ name: 'Name', type: 'title' }, { name: 'Squad', type: 'select' }],
          detected: { name: 'Name', email: null, role: null, team: null, projects: null },
        }]
      case 'people_notion_save_mapping':
        statuses = statuses.map((s) => (s.source === 'notion'
          ? { ...s, notion: { database_id: args!.databaseId as string, database_title: args!.databaseTitle as string, mapping: args!.mapping as Record<string, string> } }
          : s))
        return statuses
      case 'people_update': {
        const edit = args!.edit as Partial<Person>
        people = people.map((p) => (p.id === args!.personId ? { ...p, ...edit } : edit.is_me ? { ...p, is_me: false } : p))
        return people.find((p) => p.id === args!.personId)
      }
      case 'people_create': {
        const created = person('new', (args!.edit as { name: string }).name, args!.edit as Partial<Person>)
        people = [...people, created]
        return created
      }
      case 'people_delete':
        people = people.filter((p) => p.id !== args!.personId)
        return undefined
      default:
        return undefined
    }
  },
}))

const { PeopleSettings } = await import('./PeopleSettings')

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('@tauri-apps/api/event', () => originalEvent)
  mock.module('@/components/ui/dialog', () => originalDialog)
  mock.module('sonner', () => originalToast)
})

type TextNode = { children: Array<TextNode | string> }
const textOf = (node: TextNode | string): string =>
  typeof node === 'string' ? node : node.children.map(textOf).join('')
const text = (renderer: ReactTestRenderer) => textOf(renderer.root as unknown as TextNode)

async function render() {
  let renderer!: ReactTestRenderer
  await act(async () => {
    renderer = create(<PeopleSettings />)
  })
  return renderer
}

function card(renderer: ReactTestRenderer, title: 'Linear' | 'Notion') {
  return renderer.root.find((node) => node.type === 'section' && node.props['aria-label'] === `${title} people sync`)
}

function button(root: ReactTestInstance, label: string) {
  const found = root.findAll((node) => node.type === 'button' && (node.props['aria-label'] === label || textOf(node as unknown as TextNode).trim() === label))
  if (found.length === 0) throw new Error(`No button "${label}"`)
  return found[0]
}

function field(root: ReactTestInstance, label: string, type: 'input' | 'select' | 'textarea' = 'input') {
  return root.find((node) => node.type === type && node.props['aria-label'] === label)
}

async function click(target: ReactTestInstance) {
  await act(async () => { await target.props.onClick() })
}

async function type(target: ReactTestInstance, value: string) {
  await act(async () => { target.props.onChange({ target: { value } }) })
}

const personForm = (renderer: ReactTestRenderer) =>
  renderer.root.find((node) => node.type === 'form' && node.props['aria-label'] === 'Person details')

const lastCall = (command: string) => [...calls].reverse().find((call) => call.command === command)

describe('PeopleSettings', () => {
  beforeEach(() => {
    statuses = [status('linear'), status('notion')]
    people = [
      person('p1', 'Priya Shah', {
        email: 'priya@example.com',
        role: 'Product manager',
        team: 'Growth',
        aliases: ['Pri'],
        projects: [{ title: 'Offline mode', source: 'linear' }],
        sources: [{ source: 'linear', external_id: 'u1' }],
      }),
      person('p2', 'Dev Patel', { team: 'Platform', sources: [{ source: 'manual', external_id: 'p2' }] }),
    ]
    syncedPeople = people
    connectError = null
    calls.length = 0
    eventHandlers.clear()
    toastSuccess.mockClear()
    toastError.mockClear()
  })

  test('connects Linear with a key and shows the workspace', async () => {
    const renderer = await render()
    const linear = card(renderer, 'Linear')
    await type(field(linear, 'Linear API key'), '  lin_api_123 ')
    await act(async () => { await linear.findByType('form').props.onSubmit({ preventDefault: () => {} }) })
    expect(lastCall('people_connect')?.args).toEqual({ source: 'linear', token: '  lin_api_123 ' })
    expect(textOf(card(renderer, 'Linear') as unknown as TextNode)).toContain('Acme')
    expect(textOf(card(renderer, 'Linear') as unknown as TextNode)).toContain('12 active users')
    // The key is gone from the page once stored.
    expect(card(renderer, 'Linear').findAll((node) => node.type === 'input' && node.props['aria-label'] === 'Linear API key')).toHaveLength(0)
    expect(toastSuccess).toHaveBeenCalled()
  })

  test('a rejected key shows an error and stays disconnected', async () => {
    connectError = 'Linear rejected the API key'
    const renderer = await render()
    const linear = card(renderer, 'Linear')
    await type(field(linear, 'Linear API key'), 'bad')
    await act(async () => { await linear.findByType('form').props.onSubmit({ preventDefault: () => {} }) })
    expect(toastError).toHaveBeenCalledWith('Could not connect Linear', { description: 'Linear rejected the API key' })
    expect(field(card(renderer, 'Linear'), 'Linear API key').props.value).toBe('bad')
  })

  test('syncs now and lists the synced people', async () => {
    statuses = [status('linear', { connected: true, workspace: 'Acme' }), status('notion')]
    people = []
    syncedPeople = [person('p9', 'Sam Lee', { role: 'Engineer', sources: [{ source: 'linear', external_id: 'u9' }] })]
    const renderer = await render()
    expect(text(renderer)).toContain('No people yet')
    expect(text(renderer)).toContain('Not synced yet')
    await click(button(card(renderer, 'Linear'), 'Sync now'))
    expect(lastCall('people_sync_now')?.args).toEqual({ source: 'linear' })
    expect(text(renderer)).toContain('Sam Lee')
    expect(text(renderer)).toContain('Engineer')
    expect(text(renderer)).toContain('Last synced')
    expect(toastSuccess).toHaveBeenCalledWith('Synced 1 person from Linear')
  })

  test('shows the last sync error on the card', async () => {
    statuses = [status('linear', { connected: true, last_error: 'Linear rejected the API key' }), status('notion')]
    const renderer = await render()
    const alert = card(renderer, 'Linear').find((node) => node.props.role === 'alert')
    expect(textOf(alert as unknown as TextNode)).toBe('Linear rejected the API key')
  })

  test('disconnects a source', async () => {
    statuses = [status('linear', { connected: true, workspace: 'Acme' }), status('notion')]
    const renderer = await render()
    await click(button(card(renderer, 'Linear'), 'Disconnect'))
    expect(lastCall('people_disconnect')?.args).toEqual({ source: 'linear' })
    expect(card(renderer, 'Linear').findAll((node) => node.type === 'input' && node.props['aria-label'] === 'Linear API key')).toHaveLength(1)
  })

  test('lists people with sources and projects, and searches', async () => {
    const renderer = await render()
    const all = text(renderer)
    expect(all).toContain('Priya Shah')
    expect(all).toContain('Product manager · Growth')
    expect(all).toContain('Offline mode')
    expect(all).toContain('Added by you')
    await type(field(renderer.root, 'Search people'), 'offline')
    expect(text(renderer)).toContain('Priya Shah')
    expect(text(renderer)).not.toContain('Dev Patel')
    await type(field(renderer.root, 'Search people'), 'pri')
    expect(text(renderer)).toContain('Priya Shah')
    await type(field(renderer.root, 'Search people'), 'zzz')
    expect(text(renderer)).toContain('Nobody matches')
  })

  test('edits only the fields that changed', async () => {
    const renderer = await render()
    await click(button(renderer.root, 'Edit Priya Shah'))
    expect(text(renderer)).toContain('Edit Priya Shah')
    await type(field(renderer.root, 'Role'), 'Head of product')
    await type(field(renderer.root, 'Aliases (comma-separated)'), 'Pri, PS ,')
    await act(async () => { await personForm(renderer).props.onSubmit({ preventDefault: () => {} }) })
    expect(lastCall('people_update')?.args).toEqual({
      personId: 'p1',
      edit: { role: 'Head of product', aliases: ['Pri', 'PS'] },
    })
    expect(text(renderer)).toContain('Head of product · Growth')
  })

  test('marks a person as me', async () => {
    const renderer = await render()
    await click(button(renderer.root, 'Edit Dev Patel'))
    const toggle = renderer.root.find((node) => node.props['aria-label'] === 'This is me' && typeof node.props.onCheckedChange === 'function')
    await act(async () => { toggle.props.onCheckedChange(true) })
    await act(async () => { await personForm(renderer).props.onSubmit({ preventDefault: () => {} }) })
    expect(lastCall('people_update')?.args).toEqual({ personId: 'p2', edit: { is_me: true } })
    expect(text(renderer)).toContain('You')
  })

  test('adds a person by hand', async () => {
    const renderer = await render()
    await click(button(renderer.root, 'Add person'))
    await type(field(renderer.root, 'Name'), ' Sam Lee ')
    await type(field(renderer.root, 'Team'), 'Design')
    await act(async () => { await personForm(renderer).props.onSubmit({ preventDefault: () => {} }) })
    expect(lastCall('people_create')?.args).toEqual({ edit: { name: 'Sam Lee', team: 'Design' } })
    expect(text(renderer)).toContain('Sam Lee')
  })

  test('deletes a person after confirming', async () => {
    const renderer = await render()
    await click(button(renderer.root, 'Delete Priya Shah'))
    expect(calls.some((call) => call.command === 'people_delete')).toBe(false)
    await click(button(renderer.root, 'Confirm delete Priya Shah'))
    expect(lastCall('people_delete')?.args).toEqual({ personId: 'p1' })
    expect(text(renderer)).not.toContain('Priya Shah')
  })

  test('picks a Notion database and overrides a property', async () => {
    statuses = [status('linear'), status('notion', { connected: true, workspace: 'Acme', notion: { database_id: null, database_title: null, mapping: {} } })]
    const renderer = await render()
    const notion = () => card(renderer, 'Notion')
    expect(button(notion(), 'Sync now').props.disabled).toBe(true)
    await click(button(notion(), 'Choose database'))
    // The only database is preselected; its detected mapping shows as Auto.
    expect(field(notion(), 'Notion database', 'select').props.value).toBe('db1')
    expect(textOf(notion() as unknown as TextNode)).toContain('Auto (Name)')
    await act(async () => { field(notion(), 'Team property', 'select').props.onChange({ target: { value: 'Squad' } }) })
    await act(async () => { field(notion(), 'Role property', 'select').props.onChange({ target: { value: '' } }) })
    await click(button(notion(), 'Save database and mapping'))
    expect(lastCall('people_notion_save_mapping')?.args).toEqual({
      databaseId: 'db1',
      databaseTitle: 'Team directory',
      mapping: { team: 'Squad', role: '' },
    })
    expect(textOf(notion() as unknown as TextNode)).toContain('Team directory')
    expect(button(notion(), 'Sync now').props.disabled).toBe(false)
  })

  test('connecting Notion offers its databases straight away', async () => {
    const renderer = await render()
    const notion = card(renderer, 'Notion')
    await type(field(notion, 'Notion integration token'), 'ntn_abc')
    await act(async () => { await notion.findByType('form').props.onSubmit({ preventDefault: () => {} }) })
    expect(lastCall('people_connect')?.args).toEqual({ source: 'notion', token: 'ntn_abc' })
    expect(lastCall('people_notion_databases')).toBeTruthy()
    expect(field(card(renderer, 'Notion'), 'Notion database', 'select').props.value).toBe('db1')
  })

  test('refreshes when a background sync finishes', async () => {
    const renderer = await render()
    people = [...people, person('p3', 'Ana Lima')]
    await act(async () => { eventHandlers.get('people-sync-updated')?.({ payload: statuses }) })
    expect(text(renderer)).toContain('Ana Lima')
  })
})
