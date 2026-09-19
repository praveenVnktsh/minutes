import * as React from "react"
import { Slot } from "@radix-ui/react-slot"
import { cva, type VariantProps } from "class-variance-authority"

import { cn } from "@/lib/utils"

const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-focus focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0",
  {
    variants: {
      variant: {
        default:
          "bg-primary text-primary-foreground shadow hover:bg-primary/90",
        destructive:
          "bg-destructive text-destructive-foreground shadow-sm hover:bg-destructive/90",
        outline:
          "border border-input bg-background shadow-sm hover:bg-accent hover:text-accent-foreground",
        secondary:
          "bg-secondary text-secondary-foreground shadow-sm hover:bg-secondary/80",
        ghost: "hover:bg-accent hover:text-accent-foreground",
        link: "text-primary underline-offset-4 hover:underline",
        selected: "bg-selected text-selected-foreground hover:brightness-95",
        info: "bg-info text-white hover:brightness-90 dark:bg-info-subtle dark:text-info",
        success: "bg-success text-white hover:brightness-90 dark:bg-success-subtle dark:text-success",
        warning: "bg-warning text-white hover:brightness-90 dark:bg-warning-subtle dark:text-warning",
        error: "bg-error text-white hover:brightness-90 dark:bg-error-subtle dark:text-error",
        recording: "bg-recording text-white hover:brightness-90 dark:bg-recording-subtle dark:text-recording",
        paused: "bg-paused text-white hover:brightness-90 dark:bg-paused-subtle dark:text-paused",
        green: "bg-success text-white hover:brightness-90 dark:bg-success-subtle dark:text-success",
        blue: "bg-info text-white hover:brightness-90 dark:bg-info-subtle dark:text-info",
        red: "bg-error text-white hover:brightness-90 dark:bg-error-subtle dark:text-error",
        gray: "border bg-surface-2 border-input shadow-sm hover:bg-surface-2 hover:text-accent-foreground",
      },
      size: {
        default: "h-9 px-4 py-2",
        sm: "h-8 rounded-md px-3 text-xs",
        lg: "h-10 rounded-md px-8",
        icon: "h-9 w-9",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  }
)

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {
  asChild?: boolean
}

const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, ...props }, ref) => {
    const Comp = asChild ? Slot : "button"
    return (
      <Comp
        className={cn(buttonVariants({ variant, size, className }))}
        ref={ref}
        {...props}
      />
    )
  }
)
Button.displayName = "Button"

export { Button, buttonVariants }
