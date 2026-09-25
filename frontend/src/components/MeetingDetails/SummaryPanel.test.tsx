import { describe, expect, test } from 'bun:test'
import { isValidElement, type ReactElement, type ReactNode } from 'react'
import type { RefObject } from 'react'
import { SummaryPanel } from './SummaryPanel'
import type { BlockNoteSummaryViewRef } from '@/components/AISummary/BlockNoteSummaryView'
import type { ModelConfig } from '@/types/modelConfig'
import type { MeetingSummary } from '@/types'

// SummaryPanel is a plain function component with no hooks of its own, so it
// can be called directly (the same pattern command.test.tsx uses for
// CommandDialog) instead of mounting it. Its JSX children, such as
// <BlockNoteSummaryView />, become inert element descriptors here -- React
// never invokes their function bodies unless something actually renders them
// -- so there is nothing Tauri- or BlockNote-related to stub.

interface FoundElement {
  element: ReactElement
  ancestors: ReactElement[]
}

/** Depth-first walk over a React element tree (elements, arrays, fragments). */
function walk(node: ReactNode, ancestors: ReactElement[], found: FoundElement[], predicate: (el: ReactElement) => boolean) {
  if (Array.isArray(node)) {
    node.forEach((child) => walk(child, ancestors, found, predicate))
    return
  }
  if (!isValidElement(node)) return

  if (predicate(node)) {
    found.push({ element: node, ancestors })
  }

  const props = node.props as { children?: ReactNode }
  walk(props.children, [...ancestors, node], found, predicate)
}

function findAll(root: ReactElement, predicate: (el: ReactElement) => boolean): FoundElement[] {
  const found: FoundElement[] = []
  walk(root, [], found, predicate)
  return found
}

/** Concatenates all string/number leaves under a node's subtree. */
function textContent(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textContent).join('')
  if (isValidElement(node)) return textContent((node.props as { children?: ReactNode }).children)
  return ''
}

/** Every className string reachable anywhere in the tree, elements and plain-object props alike. */
function allClassNames(root: ReactElement): string[] {
  const classNames: string[] = []
  const found: FoundElement[] = []
  walk(root, [], found, (el) => {
    const className = (el.props as { className?: unknown }).className
    if (typeof className === 'string') classNames.push(className)
    return false
  })
  return classNames
}

function baseProps() {
  const summaryRef: RefObject<BlockNoteSummaryViewRef> = { current: null }
  const modelConfig: ModelConfig = {
    provider: 'ollama',
    model: 'llama3',
    whisperModel: 'base',
  }

  return {
    meeting: { id: 'meeting-1', title: 'Standup', created_at: '2026-09-24T00:00:00Z' },
    meetingTitle: 'Standup',
    isSummaryDirty: false,
    summaryRef,
    isSaving: false,
    onCopySummary: async () => {},
    modelConfig,
    onGenerateSummary: async () => {},
    onStopGeneration: () => {},
    customPrompt: '',
    onSaveSummary: async () => {},
    onSummaryChange: () => {},
    onDirtyChange: () => {},
    summaryError: null,
    summaryReadError: null,
    onRetrySummaryRead: () => {},
    onRegenerateSummary: async () => {},
    getSummaryStatusMessage: () => 'Something went wrong generating the summary.',
    transcripts: [],
    availableTemplates: [],
    selectedTemplate: '',
    onTemplateSelect: () => {},
    isModelConfigLoading: false,
    onOpenModelSettings: () => {},
  }
}

function renderPanel(overrides: Partial<Parameters<typeof SummaryPanel>[0]>) {
  const props = { ...baseProps(), ...overrides } as Parameters<typeof SummaryPanel>[0]
  return SummaryPanel(props) as ReactElement
}

const aiSummary: MeetingSummary = {
  markdown: '# Notes\n\nWe decided to ship it.',
}

describe('SummaryPanel regen banner', () => {
  test('regenerating banner is not a descendant of the notes scroller', () => {
    const root = renderPanel({ aiSummary, summaryStatus: 'regenerating' })

    const scrollers = findAll(root, (el) => (el.props as { 'data-testid'?: string })['data-testid'] === 'summary-notes-scroller')
    expect(scrollers.length).toBe(1)
    const scrollerElement = scrollers[0].element

    const banners = findAll(
      root,
      (el) => (el.props as { role?: string }).role === 'status' && textContent(el).includes('Regenerating'),
    )
    expect(banners.length).toBe(1)
    const banner = banners[0]

    // Not inside the scroller: the scroller element must not appear among the
    // banner's ancestors.
    expect(banner.ancestors).not.toContain(scrollerElement)

    const className = (banner.element.props as { className?: string }).className ?? ''
    expect(className).not.toContain('-soft')
    // panel.info recipe: 'border border-info/30 bg-info-subtle text-info'
    expect(className).toContain('bg-info-subtle')
    expect(className).toContain('text-info')
    expect(className).toContain('border-info/30')
  })

  test('no regenerating banner once the summary has completed', () => {
    const root = renderPanel({ aiSummary, summaryStatus: 'completed' })

    const banners = findAll(
      root,
      (el) => (el.props as { role?: string }).role === 'status' && textContent(el).includes('Regenerating'),
    )
    expect(banners.length).toBe(0)
  })

  test('no rendered className ever uses a "-soft" token, in either status', () => {
    for (const summaryStatus of ['regenerating', 'completed'] as const) {
      const root = renderPanel({ aiSummary, summaryStatus })
      const classNames = allClassNames(root)
      expect(classNames.length).toBeGreaterThan(0)
      for (const className of classNames) {
        expect(className).not.toContain('-soft')
      }
    }
  })
})
