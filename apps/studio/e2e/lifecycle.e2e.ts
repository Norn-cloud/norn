import { expect, test } from "@playwright/test"
import { clickEdge, drag, edge, handle, node, open, press } from "./canvas.ts"

// One test per row of the lifecycle canvas's verb matrix (see the PR body). Each test exercises every ✓ verb of its row.

test("state: create, view, edit, delete, duplicate, move, undo/redo, validate, save draft, reopen", async ({ page }) => {
  await open(page)
  // create + view + validate: a new state with no steps is unreachable and stuck.
  await page.getByRole("button", { name: "State", exact: true }).click()
  await expect(node(page, "new_state")).toContainText("New state")
  await expect(node(page, "new_state")).toContainText("2 problems")
  await expect(page.getByText("Checker: 2 new findings")).toBeVisible()
  // edit: rename through the inspector.
  await page.getByLabel("Name").fill("review")
  await page.getByLabel("Name").blur()
  await expect(node(page, "review")).toBeVisible()
  // move: drag the state; undo puts it back in one step.
  const before = await node(page, "review").boundingBox()
  await node(page, "review").hover()
  await page.mouse.down(); await page.mouse.move((before?.x ?? 0) + 200, (before?.y ?? 0) + 80, { steps: 8 }); await page.mouse.up()
  expect((await node(page, "review").boundingBox())?.x).toBeGreaterThan((before?.x ?? 0) + 100)
  await press(page, "z")
  expect(Math.round((await node(page, "review").boundingBox())?.x ?? 0)).toBe(Math.round(before?.x ?? 0))
  // duplicate.
  await node(page, "review").click()
  await press(page, "d")
  await expect(node(page, "review_copy")).toBeVisible()
  // delete, undo, redo.
  await node(page, "review_copy").click()
  await page.keyboard.press("Delete")
  await expect(node(page, "review_copy")).toHaveCount(0)
  await press(page, "z")
  await expect(node(page, "review_copy")).toBeVisible()
  await press(page, "Shift+z")
  await expect(node(page, "review_copy")).toHaveCount(0)
  // save: a draft in this browser; the server save says why it is not available.
  await press(page, "s")
  await expect(page.getByText(/Draft saved in this browser/)).toBeVisible()
  await expect(page.getByRole("button", { name: "Save to server" })).toBeDisabled()
  // reopen: the draft comes back after a reload, then discard returns the admitted definition.
  await page.reload()
  await expect(page.getByText(/Restored your unsaved draft/)).toBeVisible()
  await expect(node(page, "review")).toBeVisible()
  await page.getByRole("button", { name: "Discard draft" }).click()
  await expect(node(page, "review")).toHaveCount(0)
})

test("step: create, view, edit, delete, reconnect, undo/redo, validate, reopen", async ({ page }) => {
  await open(page)
  // delete + validate: removing `complete` strands invoiced and completed.
  await clickEdge(page, "complete:invoiced")
  await page.keyboard.press("Delete")
  await expect(edge(page, "complete:invoiced")).toHaveCount(0)
  await expect(node(page, "invoiced")).toContainText("1 problem")
  await expect(node(page, "completed")).toContainText("1 problem")
  // undo / redo.
  await press(page, "z")
  await expect(edge(page, "complete:invoiced")).toHaveCount(1)
  await press(page, "Shift+z")
  await expect(edge(page, "complete:invoiced")).toHaveCount(0)
  // create: draw invoiced → completed again; findings clear.
  await drag(page, handle(page, "invoiced", "right"), handle(page, "completed", "left"))
  await expect(edge(page, "invoiced_to_completed:invoiced")).toHaveCount(1)
  await expect(page.getByText("Checker: no new findings")).toBeVisible()
  // edit + view: rename the step.
  await clickEdge(page, "invoiced_to_completed:invoiced")
  await page.getByLabel("Step id").fill("finish")
  await page.getByLabel("Step id").blur()
  await expect(edge(page, "invoiced_to_completed:invoiced")).toContainText("Finish")
  // edit the step's rule: a guard that does not parse is a typed finding on the step; clearing it clears the finding.
  await page.getByLabel("Guards").fill("self.total >")
  await page.getByLabel("Guards").blur()
  await expect(page.getByText("lifecycle.validation.invalid_guard").first()).toBeVisible()
  await page.getByLabel("Guards").fill("")
  await page.getByLabel("Guards").blur()
  await expect(page.getByText("Checker: no new findings")).toBeVisible()
  // reconnect: drag the step's target end onto another state, then undo.
  const updater = edge(page, "invoiced_to_completed:invoiced").locator(".react-flow__edgeupdater-target")
  await drag(page, updater, handle(page, "cancelled", "left"))
  await expect(page.getByText(/Invoiced → Cancelled/)).toBeVisible()
  await press(page, "z")
  await expect(page.getByText(/Invoiced → Completed/)).toBeVisible()
  // reopen: the drawn step survives a reload.
  await page.reload()
  await expect(edge(page, "invoiced_to_completed:invoiced")).toHaveCount(1)
})

test("state flags (port config): edit, undo/redo, validate, reopen", async ({ page }) => {
  await open(page)
  await node(page, "draft").click()
  await page.getByLabel("Initial").uncheck()
  await expect(page.getByText(/Checker: \d+ new finding/)).toBeVisible()
  await expect(page.getByText("lifecycle.validation.empty_initial").first()).toBeVisible()
  await press(page, "z")
  await expect(page.getByLabel("Initial")).toBeChecked()
  await press(page, "Shift+z")
  await expect(page.getByLabel("Initial")).not.toBeChecked()
  await page.reload()
  await node(page, "draft").click()
  await expect(page.getByLabel("Initial")).not.toBeChecked()
})

test("selection: multi-select, copy/paste, select all, delete selected", async ({ page }) => {
  await open(page)
  await node(page, "invoiced").click()
  await node(page, "completed").click({ modifiers: ["Shift"] })
  await expect(page.getByText("2 states and 0 steps selected.")).toBeVisible()
  await press(page, "c")
  await press(page, "v")
  await expect(node(page, "invoiced_copy")).toBeVisible()
  await expect(node(page, "completed_copy")).toBeVisible()
  await expect(page.getByText("2 states and 0 steps selected.")).toBeVisible()
  await page.getByRole("button", { name: "Delete selected" }).click()
  await expect(node(page, "invoiced_copy")).toHaveCount(0)
  // Select-all is the canvas's only while the canvas is in use: click it first.
  await page.locator(".react-flow__pane").click({ position: { x: 10, y: 10 } })
  await press(page, "a")
  await expect(page.getByText(/^\d+ states and \d+ steps selected\.$/)).toBeVisible()
})

test("shortcuts: copy and select-all stay the browser's outside the canvas and over selected text", async ({ page }) => {
  await open(page)
  await node(page, "invoiced").click()
  // Selecting text elsewhere and copying copies the text: the canvas does not claim it.
  await page.getByText(/Draw how a process moves/).selectText()
  await press(page, "c")
  await expect(page.getByText(/^Copied \d+ items?\.$/)).toHaveCount(0)
  // With another modifier held, the shortcut is not the canvas's either.
  await node(page, "invoiced").click()
  await page.keyboard.press(`${process.platform === "darwin" ? "Meta" : "Control"}+Shift+c`)
  await expect(page.getByText(/^Copied \d+ items?\.$/)).toHaveCount(0)
  // Back on the canvas, copy is the canvas's again.
  await node(page, "invoiced").click()
  await press(page, "c")
  await expect(page.getByText("Copied 1 item.")).toBeVisible()
})

test("problems panel: lists each finding and jumps to the state it is on", async ({ page }) => {
  await open(page)
  await page.getByRole("button", { name: "State", exact: true }).click()
  await page.locator(".react-flow__pane").click({ position: { x: 10, y: 10 } })
  await expect(page.getByTestId("problem")).toHaveCount(2)
  await page.getByTestId("problem").first().click()
  await expect(page.getByLabel("Name")).toHaveValue("new_state")
})
