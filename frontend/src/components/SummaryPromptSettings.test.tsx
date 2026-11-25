import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React from 'react'
import { act, create, type ReactTestInstance, type ReactTestRenderer } from 'react-test-renderer'

// Restore the real modules afterwards so these mocks cannot leak into a later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) }
const originalSonner = { ...(await import('sonner')) }
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  mock.module('sonner', () => originalSonner)
})

const DEFAULT_PROMPT = 'You are an expert meeting summarizer.'

// A tiny stand-in for the Rust store: the saved override, or null when none is set.
let savedOverride: string | null = null
// The notes prompt's override when a test renders the no-notes card.
let notesOverride: string | null = null
let failNext: string | null = null
let calls: Array<{ command: string; args?: Record<string, unknown> }> = []
let toasts: Array<{ kind: 'success' | 'error'; message: string }> = []

function settings() {
  return {
    prompt: savedOverride ?? DEFAULT_PROMPT,
    defaultPrompt: DEFAULT_PROMPT,
    isCustom: savedOverride !== null,
  }
}

mock.module('@tauri-apps/api/core', () => ({
  ...originalCore,
  invoke: async (command: string, args?: Record<string, unknown>) => {
    calls.push({ command, args })
    if (failNext === command) {
      failNext = null
      throw new Error('store unavailable')
    }
    if (command === 'api_get_summary_prompt') {
      if (args?.kind === 'notes' && notesOverride !== null) {
        return { prompt: notesOverride, defaultPrompt: DEFAULT_PROMPT, isCustom: true }
      }
      return settings()
    }
    if (command === 'api_save_summary_prompt') {
      const prompt = String(args?.prompt ?? '').trim()
      savedOverride = prompt === '' || prompt === DEFAULT_PROMPT ? null : prompt
      return settings()
    }
    if (command === 'api_reset_summary_prompt') {
      savedOverride = null
      return settings()
    }
    return undefined
  },
}))
mock.module('sonner', () => ({
  ...originalSonner,
  toast: Object.assign(() => {}, {
    success: (message: string) => toasts.push({ kind: 'success', message }),
    error: (message: string) => toasts.push({ kind: 'error', message }),
  }),
}))

const { SummaryPromptSettings, PROMPT_SPLIT_NOTICE_KEY } = await import('./SummaryPromptSettings')

async function render(kind?: 'notes' | 'transcriptOnly') {
  let renderer!: ReactTestRenderer
  await act(async () => {
    renderer = create(<SummaryPromptSettings kind={kind} />)
  })
  return renderer
}

function textarea(renderer: ReactTestRenderer) {
  return renderer.root.find((node) => node.type === 'textarea')
}

function button(renderer: ReactTestRenderer, label: string): ReactTestInstance {
  return renderer.root.find(
    (node) =>
      node.type === 'button' &&
      React.Children.toArray(node.props.children).some((child) => child === label),
  )
}

function status(renderer: ReactTestRenderer) {
  return renderer.root.findByProps({ 'data-testid': 'summary-prompt-status' }).props.children
}

async function type(renderer: ReactTestRenderer, value: string) {
  await act(async () => {
    textarea(renderer).props.onChange({ target: { value } })
  })
}

async function click(node: ReactTestInstance) {
  await act(async () => {
    node.props.onClick()
  })
}

describe('SummaryPromptSettings', () => {
  beforeEach(() => {
    savedOverride = null
    notesOverride = null
    try {
      localStorage.removeItem(PROMPT_SPLIT_NOTICE_KEY)
    } catch {
      // No storage in this runtime: the notice is never remembered.
    }
    failNext = null
    calls = []
    toasts = []
  })

  test('loads the default prompt with save and reset disabled', async () => {
    const renderer = await render()

    expect(textarea(renderer).props.value).toBe(DEFAULT_PROMPT)
    expect(status(renderer)).toBe('Default')
    expect(button(renderer, 'Save prompt').props.disabled).toBe(true)
    expect(button(renderer, 'Reset to default').props.disabled).toBe(true)
  })

  test('editing enables save, and saving stores the prompt and marks it custom', async () => {
    const renderer = await render()

    await type(renderer, 'Write terse bullet points.')
    expect(button(renderer, 'Save prompt').props.disabled).toBe(false)
    expect(button(renderer, 'Reset to default').props.disabled).toBe(false)

    await click(button(renderer, 'Save prompt'))

    expect(calls.at(-1)).toEqual({
      command: 'api_save_summary_prompt',
      args: { prompt: 'Write terse bullet points.', kind: 'notes' },
    })
    expect(status(renderer)).toBe('Custom')
    expect(button(renderer, 'Save prompt').props.disabled).toBe(true)
    expect(toasts).toEqual([{ kind: 'success', message: 'Notes prompt saved' }])
  })

  test('the no-notes card edits its own prompt kind', async () => {
    const renderer = await render('transcriptOnly')
    expect(calls[0]).toEqual({ command: 'api_get_summary_prompt', args: { kind: 'transcriptOnly' } })
    expect(textarea(renderer).props['aria-label']).toBe('No-notes prompt')

    await type(renderer, 'Bullets only.')
    await click(button(renderer, 'Save prompt'))

    expect(calls.at(-1)).toEqual({
      command: 'api_save_summary_prompt',
      args: { prompt: 'Bullets only.', kind: 'transcriptOnly' },
    })
    expect(toasts).toEqual([{ kind: 'success', message: 'No-notes prompt saved' }])
  })

  test('a custom notes prompt from before the split gets a one-time notice on the no-notes card', async () => {
    notesOverride = 'My old custom prompt.'
    const renderer = await render('transcriptOnly')

    expect(renderer.root.findAllByProps({ 'data-testid': 'prompt-split-notice' })).toHaveLength(1)
    await click(button(renderer, 'Start from my Notes prompt'))

    expect(textarea(renderer).props.value).toBe('My old custom prompt.')
    expect(button(renderer, 'Save prompt').props.disabled).toBe(false)
    expect(renderer.root.findAllByProps({ 'data-testid': 'prompt-split-notice' })).toHaveLength(0)
  })

  test('the split notice comes back if the copied prompt was never saved, not after saving', async () => {
    notesOverride = 'My old custom prompt.'
    const first = await render('transcriptOnly')
    await click(button(first, 'Start from my Notes prompt'))
    // Left without saving: the offer is still there next time.
    const again = await render('transcriptOnly')
    expect(again.root.findAllByProps({ 'data-testid': 'prompt-split-notice' })).toHaveLength(1)

    await click(button(again, 'Start from my Notes prompt'))
    await click(button(again, 'Save prompt'))
    expect(savedOverride).toBe('My old custom prompt.')
    savedOverride = null // even if the no-notes prompt is reset later
    const afterSave = await render('transcriptOnly')
    const remembered = (() => {
      try {
        return typeof localStorage !== 'undefined'
      } catch {
        return false
      }
    })()
    expect(afterSave.root.findAllByProps({ 'data-testid': 'prompt-split-notice' })).toHaveLength(
      remembered ? 0 : 1,
    )
  })

  test('no split notice without a custom notes prompt, or on the notes card', async () => {
    const plain = await render('transcriptOnly')
    expect(plain.root.findAllByProps({ 'data-testid': 'prompt-split-notice' })).toHaveLength(0)

    notesOverride = 'My old custom prompt.'
    savedOverride = 'Already customised.'
    const customised = await render('transcriptOnly')
    expect(customised.root.findAllByProps({ 'data-testid': 'prompt-split-notice' })).toHaveLength(0)

    const notesCard = await render('notes')
    expect(notesCard.root.findAllByProps({ 'data-testid': 'prompt-split-notice' })).toHaveLength(0)
  })

  test('reset clears a saved override and restores the default text', async () => {
    savedOverride = 'Only list action items.'
    const renderer = await render()

    expect(textarea(renderer).props.value).toBe('Only list action items.')
    expect(status(renderer)).toBe('Custom')
    expect(button(renderer, 'Reset to default').props.disabled).toBe(false)

    await click(button(renderer, 'Reset to default'))

    expect(calls.at(-1)?.command).toBe('api_reset_summary_prompt')
    expect(textarea(renderer).props.value).toBe(DEFAULT_PROMPT)
    expect(status(renderer)).toBe('Default')
    expect(button(renderer, 'Reset to default').props.disabled).toBe(true)
  })

  test('a failed save keeps the edit and shows an error toast', async () => {
    const renderer = await render()
    await type(renderer, 'Something new.')

    failNext = 'api_save_summary_prompt'
    await click(button(renderer, 'Save prompt'))

    expect(textarea(renderer).props.value).toBe('Something new.')
    expect(status(renderer)).toBe('Default')
    expect(button(renderer, 'Save prompt').props.disabled).toBe(false)
    expect(toasts.map((t) => t.kind)).toEqual(['error'])
  })

  test('a failed load shows an error with a retry that recovers', async () => {
    failNext = 'api_get_summary_prompt'
    const renderer = await render()

    const alert = renderer.root.findByProps({ role: 'alert' })
    const message = alert.find((node) => node.type === 'span').children.join('')
    expect(message).toContain('store unavailable')
    expect(renderer.root.findAll((node) => node.type === 'textarea')).toHaveLength(0)

    await click(button(renderer, 'Retry'))

    expect(textarea(renderer).props.value).toBe(DEFAULT_PROMPT)
    expect(renderer.root.findAllByProps({ role: 'alert' })).toHaveLength(0)
  })
})
