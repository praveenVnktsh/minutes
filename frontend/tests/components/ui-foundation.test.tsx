import { describe, expect, test } from "bun:test"
import { createElement } from "react"
import { act, create } from "react-test-renderer"
import { renderToStaticMarkup } from "react-dom/server"

import { ResizeSeparator } from "../../src/components/ui/resize-separator"
import { SegmentedControl } from "../../src/components/ui/segmented-control"
import { SaveFeedback, StatusFeedback } from "../../src/components/ui/status-feedback"

describe("UI feedback foundation", () => {
  test("announces routine and failed status with the appropriate urgency", () => {
    const saved = renderToStaticMarkup(createElement(SaveFeedback, { state: "saved" }))
    const failed = renderToStaticMarkup(createElement(StatusFeedback, { tone: "error" }, "Could not save"))

    expect(saved).toContain('role="status"')
    expect(saved).toContain("Saved")
    expect(failed).toContain('role="alert"')
    expect(failed).toContain('aria-live="assertive"')
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
})
