import { describe, expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { createElement } from "react"
import { act, create } from "react-test-renderer"
import { renderToStaticMarkup } from "react-dom/server"

import { buttonVariants } from "../../src/components/ui/button"
import { ResizeSeparator } from "../../src/components/ui/resize-separator"
import { SegmentedControl } from "../../src/components/ui/segmented-control"
import { SaveFeedback, StatusFeedback } from "../../src/components/ui/status-feedback"

const frontendRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..")

describe("UI feedback foundation", () => {
  test("announces routine and failed status with the appropriate urgency", () => {
    const saved = renderToStaticMarkup(createElement(SaveFeedback, { state: "saved" }))
    const failed = renderToStaticMarkup(createElement(StatusFeedback, { tone: "error" }, "Could not save"))

    expect(saved).toContain('role="status"')
    expect(saved).toContain("Saved")
    expect(failed).toContain('role="alert"')
    expect(failed).toContain('aria-live="assertive"')
  })

  test("keeps saving feedback announceable and preserves caller busy state", () => {
    const saving = renderToStaticMarkup(createElement(SaveFeedback, { state: "saving" }))
    const callerBusy = renderToStaticMarkup(
      createElement(StatusFeedback, { pending: true, "aria-busy": true }, "Working"),
    )

    expect(saving).toContain('aria-live="polite"')
    expect(saving).not.toContain('aria-busy="true"')
    expect(callerBusy).toContain('aria-busy="true"')
  })

  test("exposes segmented selection and supports arrow-key changes", () => {
    const changes: string[] = []
    const renderer = create(
      createElement(SegmentedControl, {
        "aria-label": "Document view",
        value: "raw",
        options: [
          { value: "raw", label: "Raw" },
          { value: "enhanced", label: "Enhanced" },
        ],
        onValueChange: (value: string) => changes.push(value),
      }),
      { createNodeMock: () => ({ focus() {} }) },
    )

    const buttons = renderer.root.findAllByType("button")
    expect(buttons[0].props["aria-pressed"]).toBe(true)
    expect(buttons[1].props["aria-pressed"]).toBe(false)
    act(() => buttons[0].props.onKeyDown({ key: "ArrowRight", preventDefault() {} }))
    expect(changes).toEqual(["enhanced"])
    renderer.unmount()
  })

  test("falls back to an enabled tab stop when the selected option is disabled or absent", () => {
    const changes: string[] = []
    const focused: React.ReactNode[] = []
    const disabledRenderer = create(
      createElement(SegmentedControl, {
        "aria-label": "Document view",
        value: "raw",
        options: [
          { value: "raw", label: "Raw", disabled: true },
          { value: "enhanced", label: "Enhanced" },
          { value: "notes", label: "Notes" },
        ],
        onValueChange: (value: string) => changes.push(value),
      }),
      { createNodeMock: (element) => ({ focus: () => focused.push(element.props.children) }) },
    )
    const disabledButtons = disabledRenderer.root.findAllByType("button")

    expect(disabledButtons.map((button) => button.props.tabIndex)).toEqual([-1, 0, -1])
    act(() => disabledButtons[1].props.onKeyDown({ key: "ArrowRight", preventDefault() {} }))
    expect(changes).toEqual(["notes"])
    expect(focused).toEqual(["Notes"])
    disabledRenderer.unmount()

    const removedRenderer = create(
      createElement(SegmentedControl, {
        "aria-label": "Document view",
        value: "raw",
        options: [
          { value: "enhanced", label: "Enhanced" },
          { value: "notes", label: "Notes" },
        ],
        onValueChange: () => {},
      }),
    )
    expect(removedRenderer.root.findAllByType("button").map((button) => button.props.tabIndex)).toEqual([0, -1])
    removedRenderer.unmount()
  })

  test("makes panel size keyboard adjustable and clamps its value", () => {
    const changes: number[] = []
    const renderer = create(
      createElement(ResizeSeparator, {
        label: "Resize transcript panel",
        value: 395,
        min: 240,
        max: 400,
        step: 20,
        onValueChange: (value: number) => changes.push(value),
      }),
    )
    const separator = renderer.root.findByProps({ role: "separator" })

    act(() => separator.props.onKeyDown({ key: "ArrowRight", preventDefault() {}, defaultPrevented: false }))
    act(() => separator.props.onKeyDown({ key: "Home", preventDefault() {}, defaultPrevented: false }))
    expect(changes).toEqual([400, 240])
    expect(separator.props["aria-valuenow"]).toBe(395)
    renderer.unmount()
  })

  test("allows callers to remove a resize separator from the tab order", () => {
    const html = renderToStaticMarkup(
      createElement(ResizeSeparator, {
        label: "Resize transcript panel",
        value: 320,
        min: 240,
        max: 400,
        tabIndex: -1,
        onValueChange: () => {},
      }),
    )
    expect(html).toContain('tabindex="-1"')
  })

  test("keeps compatibility button aliases legible in dark mode", () => {
    expect(buttonVariants({ variant: "green" })).toContain("dark:text-success")
    expect(buttonVariants({ variant: "blue" })).toContain("dark:text-info")
    expect(buttonVariants({ variant: "red" })).toContain("dark:text-error")
  })

  test("overrides Sonner variables and scoped description and button rules", () => {
    const toaster = readFileSync(join(frontendRoot, "src/components/ThemedToaster.tsx"), "utf8")
    const globals = readFileSync(join(frontendRoot, "src/app/globals.css"), "utf8")

    expect(toaster).toContain("'--normal-bg': 'var(--surface-raised)'")
    expect(toaster).toContain("'--success-bg': 'var(--success-subtle)'")
    expect(toaster).toContain("'--info-bg': 'var(--info-subtle)'")
    expect(toaster).toContain("'--warning-bg': 'var(--warning-subtle)'")
    expect(toaster).toContain("'--error-bg': 'var(--error-subtle)'")
    expect(globals).toContain('.minutes-toaster[data-sonner-toaster] [data-sonner-toast][data-styled="true"] [data-description]')
    expect(globals).toContain('.minutes-toaster[data-sonner-toaster] [data-rich-colors="true"][data-sonner-toast][data-styled="true"] [data-description]')
    expect(globals).toContain('.minutes-toaster[data-sonner-toaster] [data-sonner-toast][data-styled="true"] [data-button]')
  })

  test("compiles opacity modifiers for semantic state utilities", () => {
    const directory = mkdtempSync(join(tmpdir(), "minutes-tailwind-"))
    const input = join(directory, "input.css")
    const content = join(directory, "content.html")
    const output = join(directory, "output.css")

    try {
      writeFileSync(input, "@tailwind utilities;\n")
      writeFileSync(content, '<div class="bg-info/50 border-error/50 text-success/50"></div>\n')
      const result = spawnSync(
        "pnpm",
        ["exec", "tailwindcss", "-c", join(frontendRoot, "tailwind.config.js"), "-i", input, "-o", output, "--content", content],
        { cwd: frontendRoot, encoding: "utf8" },
      )

      expect(result.status, result.stderr).toBe(0)
      const css = readFileSync(output, "utf8")
      expect(css).toContain("rgb(var(--info-rgb) / 0.5)")
      expect(css).toContain("rgb(var(--error-rgb) / 0.5)")
      expect(css).toContain("rgb(var(--success-rgb) / 0.5)")
    } finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })
})
