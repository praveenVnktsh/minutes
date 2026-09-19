import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import React, { type ReactNode } from 'react'
import { act, create } from 'react-test-renderer'

const focusMeetingsSearch = mock(async () => {})
const openerFocus = mock(() => {})

const originalCommand = { ...await import('@/components/ui/command') }
const originalSidebar = { ...await import('@/components/Sidebar/SidebarProvider') }
const originalShell = { ...await import('@/contexts/ShellContext') }
const originalDebug = { ...await import('@/hooks/useDebugMode') }

function CommandDialogStub({ children }: { children: ReactNode }) {
  return <div>{children}</div>
}
function CommandItemStub({ children }: { children: ReactNode }) {
  return <div>{children}</div>
}
const PassThrough = ({ children }: { children?: ReactNode }) => <div>{children}</div>

mock.module('@/components/ui/command', () => ({
  ...originalCommand,
  CommandDialog: CommandDialogStub,
  CommandEmpty: PassThrough,
  CommandGroup: PassThrough,
  CommandInput: PassThrough,
  CommandItem: CommandItemStub,
  CommandList: PassThrough,
  CommandSeparator: PassThrough,
  CommandShortcut: PassThrough,
}))
mock.module('@/components/Sidebar/SidebarProvider', () => ({
  ...originalSidebar,
  useSidebar: () => ({ currentMeeting: null, selectMeetings: () => [] }),
}))
mock.module('@/contexts/ShellContext', () => ({
  ...originalShell,
  useShell: () => ({
    toggleTheme: () => {},
    recordingActionLabel: 'New meeting',
    runRecordingAction: async () => {},
    importActionLabel: 'Import recording',
    runImportAction: async () => {},
    navigate: async () => {},
    openMeeting: async () => {},
    recordingActionDisabled: false,
    focusMeetingsSearch,
  }),
}))
mock.module('@/hooks/useDebugMode', () => ({ ...originalDebug, useDebugMode: () => false }))

const originalDocument = globalThis.document
const originalHTMLElement = globalThis.HTMLElement
const { CommandPalette } = await import('./CommandPalette')

class TestHTMLElement {
  tagName = 'BUTTON'
  isContentEditable = false
  isConnected = true
  focus = openerFocus
}

beforeEach(() => {
  focusMeetingsSearch.mockClear()
  openerFocus.mockClear()
  Object.defineProperty(globalThis, 'HTMLElement', { configurable: true, value: TestHTMLElement })
  const documentTarget = new EventTarget() as EventTarget & { activeElement: TestHTMLElement }
  documentTarget.activeElement = new TestHTMLElement()
  Object.defineProperty(globalThis, 'document', { configurable: true, value: documentTarget })
})

afterAll(() => {
  mock.module('@/components/ui/command', () => originalCommand)
  mock.module('@/components/Sidebar/SidebarProvider', () => originalSidebar)
  mock.module('@/contexts/ShellContext', () => originalShell)
  mock.module('@/hooks/useDebugMode', () => originalDebug)
  if (originalDocument) Object.defineProperty(globalThis, 'document', { configurable: true, value: originalDocument })
  else Reflect.deleteProperty(globalThis, 'document')
  if (originalHTMLElement) Object.defineProperty(globalThis, 'HTMLElement', { configurable: true, value: originalHTMLElement })
  else Reflect.deleteProperty(globalThis, 'HTMLElement')
})

function openPalette() {
  const event = new Event('keydown', { cancelable: true })
  Object.defineProperties(event, {
    key: { value: 'k' },
    metaKey: { value: true },
  })
  document.dispatchEvent(event)
}

describe('CommandPalette focus ownership', () => {
  test('restores its connected keyboard opener after cancellation', async () => {
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<CommandPalette />) })
    await act(async () => openPalette())

    let dialog = renderer!.root.findByType(CommandDialogStub)
    expect(dialog.props.open).toBe(true)
    await act(async () => dialog.props.onOpenChange(false))
    dialog = renderer!.root.findByType(CommandDialogStub)
    dialog.props.onCloseAutoFocus({ preventDefault: () => {} })
    expect(openerFocus).toHaveBeenCalledTimes(1)
    await act(async () => renderer!.unmount())
  })

  test('does not restore the opener over an intentional search destination', async () => {
    let renderer: ReturnType<typeof create>
    await act(async () => { renderer = create(<CommandPalette />) })
    await act(async () => openPalette())

    const searchItem = renderer!.root.findAllByType(CommandItemStub)
      .find((item) => item.props.children.includes('Search meetings'))!
    await act(async () => {
      searchItem.props.onSelect()
      await Promise.resolve()
    })
    const dialog = renderer!.root.findByType(CommandDialogStub)
    dialog.props.onCloseAutoFocus({ preventDefault: () => {} })
    expect(focusMeetingsSearch).toHaveBeenCalledTimes(1)
    expect(openerFocus).not.toHaveBeenCalled()
    await act(async () => renderer!.unmount())
  })
})
