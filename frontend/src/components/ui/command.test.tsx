import { describe, expect, test } from 'bun:test'
import React, { isValidElement, type ReactElement } from 'react'
import { DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { CommandDialog } from './command'

describe('CommandDialog', () => {
  test('renders real hidden dialog title and description primitives', () => {
    const dialog = CommandDialog({ open: true, children: <span>Commands</span> }) as ReactElement
    const content = React.Children.only(dialog.props.children) as ReactElement
    expect(content.type).toBe(DialogContent)

    const command = React.Children.only(content.props.children) as ReactElement
    const children = React.Children.toArray(command.props.children).filter(isValidElement) as ReactElement[]
    const title = children.find((child) => child.type === DialogTitle)
    const description = children.find((child) => child.type === DialogDescription)

    expect(title?.props.className).toBe('sr-only')
    expect(title?.props.children).toBe('Command palette')
    expect(description?.props.className).toBe('sr-only')
    expect(description?.props.children).toContain('Search meetings')
  })
})
