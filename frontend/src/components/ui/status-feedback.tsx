import * as React from "react"
import { AlertCircle, Check, Circle, Info, Loader2, Pause, Radio } from "lucide-react"

import { cn } from "@/lib/utils"

export type StatusTone = "neutral" | "info" | "success" | "warning" | "error" | "recording" | "paused"

const toneClasses: Record<StatusTone, string> = {
  neutral: "text-ink-muted",
  info: "text-info",
  success: "text-success",
  warning: "text-warning",
  error: "text-error",
  recording: "text-recording",
  paused: "text-paused",
}

const toneIcons: Record<StatusTone, React.ComponentType<{ className?: string }>> = {
  neutral: Circle,
  info: Info,
  success: Check,
  warning: AlertCircle,
  error: AlertCircle,
  recording: Radio,
  paused: Pause,
}

export interface StatusFeedbackProps extends React.HTMLAttributes<HTMLDivElement> {
  tone?: StatusTone
  pending?: boolean
  actionLabel?: string
  onAction?: () => void
}

export function StatusFeedback({
  tone = "neutral",
  pending = false,
  actionLabel,
  onAction,
  className,
  children,
  ...props
}: StatusFeedbackProps) {
  const Icon = pending ? Loader2 : toneIcons[tone]
  const role = tone === "error" ? "alert" : "status"

  return (
    <div
      {...props}
      role={role}
      aria-live={tone === "error" ? "assertive" : "polite"}
      aria-atomic="true"
      className={cn("inline-flex min-h-5 items-center gap-1.5 text-xs font-medium", toneClasses[tone], className)}
    >
      <Icon className={cn("h-3.5 w-3.5 shrink-0", pending && "animate-spin")} />
      <span>{children}</span>
      {actionLabel && onAction ? (
        <button
          type="button"
          className="ml-1 rounded-sm font-semibold underline underline-offset-2 hover:no-underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus focus-visible:ring-offset-2"
          onClick={onAction}
        >
          {actionLabel}
        </button>
      ) : null}
    </div>
  )
}

export type SaveFeedbackState = "unsaved" | "saving" | "saved" | "error"

export interface SaveFeedbackProps extends Omit<StatusFeedbackProps, "tone" | "pending" | "children"> {
  state: SaveFeedbackState
  labels?: Partial<Record<SaveFeedbackState, React.ReactNode>>
}

const saveDefaults: Record<SaveFeedbackState, React.ReactNode> = {
  unsaved: "Unsaved",
  saving: "Saving",
  saved: "Saved",
  error: "Could not save",
}

export function SaveFeedback({ state, labels, ...props }: SaveFeedbackProps) {
  const tone: StatusTone = state === "saved" ? "success" : state === "error" ? "error" : "neutral"
  return (
    <StatusFeedback tone={tone} pending={state === "saving"} {...props}>
      {labels?.[state] ?? saveDefaults[state]}
    </StatusFeedback>
  )
}
