"use client"

import * as React from "react"

import { cn } from "@/lib/utils"

export interface ResizeSeparatorProps extends Omit<React.HTMLAttributes<HTMLDivElement>, "onChange"> {
  value: number
  min: number
  max: number
  onValueChange: (value: number) => void
  step?: number
  orientation?: "horizontal" | "vertical"
  label: string
}

export const ResizeSeparator = React.forwardRef<HTMLDivElement, ResizeSeparatorProps>(
  ({ value, min, max, onValueChange, step = 10, orientation = "vertical", label, className, onKeyDown, ...props }, ref) => {
    const clamp = (next: number) => Math.min(max, Math.max(min, next))

    return (
      <div
        ref={ref}
        {...props}
        role="separator"
        tabIndex={0}
        aria-label={label}
        aria-orientation={orientation}
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={value}
        className={cn(
          "group relative shrink-0 touch-none bg-hairline outline-none focus-visible:ring-2 focus-visible:ring-focus focus-visible:ring-offset-2",
          orientation === "vertical" ? "h-full w-px cursor-col-resize" : "h-px w-full cursor-row-resize",
          className,
        )}
        onKeyDown={(event) => {
          onKeyDown?.(event)
          if (event.defaultPrevented) return

          let next: number | undefined
          if (event.key === "Home") next = min
          if (event.key === "End") next = max
          if (orientation === "vertical" && event.key === "ArrowLeft") next = value - step
          if (orientation === "vertical" && event.key === "ArrowRight") next = value + step
          if (orientation === "horizontal" && event.key === "ArrowUp") next = value - step
          if (orientation === "horizontal" && event.key === "ArrowDown") next = value + step

          if (next !== undefined) {
            event.preventDefault()
            onValueChange(clamp(next))
          }
        }}
      >
        <span
          aria-hidden="true"
          className={cn(
            "absolute rounded-full bg-transparent transition-colors group-hover:bg-selected group-focus-visible:bg-selected",
            orientation === "vertical" ? "inset-y-0 left-1/2 w-2 -translate-x-1/2" : "inset-x-0 top-1/2 h-2 -translate-y-1/2",
          )}
        />
      </div>
    )
  },
)
ResizeSeparator.displayName = "ResizeSeparator"
