"use client"

import * as React from "react"

import { cn } from "@/lib/utils"

export interface SegmentedControlOption<T extends string> {
  value: T
  label: React.ReactNode
  disabled?: boolean
  ariaLabel?: string
}

export interface SegmentedControlProps<T extends string> {
  value: T
  options: readonly SegmentedControlOption<T>[]
  onValueChange: (value: T) => void
  "aria-label": string
  disabled?: boolean
  className?: string
}

export function SegmentedControl<T extends string>({
  value,
  options,
  onValueChange,
  "aria-label": ariaLabel,
  disabled = false,
  className,
}: SegmentedControlProps<T>) {
  const refs = React.useRef<Array<HTMLButtonElement | null>>([])

  function selectAdjacent(index: number, direction: 1 | -1) {
    for (let offset = 1; offset <= options.length; offset += 1) {
      const nextIndex = (index + direction * offset + options.length) % options.length
      const option = options[nextIndex]
      if (!option.disabled) {
        onValueChange(option.value)
        refs.current[nextIndex]?.focus()
        return
      }
    }
  }

  return (
    <div
      role="group"
      aria-label={ariaLabel}
      className={cn("inline-flex items-center rounded-lg border border-hairline bg-surface-1 p-1", className)}
    >
      {options.map((option, index) => {
        const selected = option.value === value
        return (
          <button
            key={option.value}
            ref={(node) => { refs.current[index] = node }}
            type="button"
            aria-label={option.ariaLabel}
            aria-pressed={selected}
            disabled={disabled || option.disabled}
            tabIndex={selected ? 0 : -1}
            className={cn(
              "rounded-md px-3 py-1.5 text-sm font-medium text-ink-muted transition-colors hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50",
              selected && "bg-selected text-selected-foreground shadow-sm",
            )}
            onClick={() => onValueChange(option.value)}
            onKeyDown={(event) => {
              if (event.key === "ArrowRight" || event.key === "ArrowDown") {
                event.preventDefault()
                selectAdjacent(index, 1)
              } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
                event.preventDefault()
                selectAdjacent(index, -1)
              }
            }}
          >
            {option.label}
          </button>
        )
      })}
    </div>
  )
}
