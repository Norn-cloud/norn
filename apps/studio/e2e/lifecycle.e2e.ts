import { expect, test } from "@playwright/test"
import { clear, clickEdge, drag, edge, edgeEnds, handle, lefts, node, open, placed, press, reveal, shown } from "./canvas.ts"

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
  const before = await (await shown(page, "review")).boundingBox()
  await (await shown(page, "review")).hover()
  await page.mouse.down(); await page.mouse.move((before?.x ?? 0) + 200, (before?.y ?? 0) + 80, { steps: 8 }); await page.mouse.up()
  expect((await node(page, "review").boundingBox())?.x).toBeGreaterThan((before?.x ?? 0) + 100)
  await press(page, "z")
  expect(Math.round((await node(page, "review").boundingBox())?.x ?? 0)).toBe(Math.round(before?.x ?? 0))
  // duplicate.
  await (await shown(page, "review")).click()
  await press(page, "d")
  await expect(node(page, "review_copy")).toBeVisible()
  // delete, undo, redo.
  await (await shown(page, "review_copy")).click()
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
  await expect(page.locator('[data-edge-label="invoiced_to_completed:invoiced"]')).toHaveText("Finish")
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
  // Panning to reach both ends can clear the selection; select the step to read where it goes now.
  await clickEdge(page, "invoiced_to_completed:invoiced")
  await expect(page.getByText(/Invoiced → Cancelled/)).toBeVisible()
  await press(page, "z")
  await expect(page.getByText(/Invoiced → Completed/)).toBeVisible()
  // reopen: the drawn step survives a reload.
  await page.reload()
  await expect(edge(page, "invoiced_to_completed:invoiced")).toHaveCount(1)
})

test("state flags (port config): edit, undo/redo, validate, reopen", async ({ page }) => {
  await open(page)
  await (await shown(page, "draft")).click()
  await page.getByLabel("Initial").uncheck()
  await expect(page.getByText(/Checker: \d+ new finding/)).toBeVisible()
  await expect(page.getByText("lifecycle.validation.empty_initial").first()).toBeVisible()
  await press(page, "z")
  await expect(page.getByLabel("Initial")).toBeChecked()
  await press(page, "Shift+z")
  await expect(page.getByLabel("Initial")).not.toBeChecked()
  await page.reload()
  await (await shown(page, "draft")).click()
  await expect(page.getByLabel("Initial")).not.toBeChecked()
})

test("selection: multi-select, copy/paste, select all, delete selected", async ({ page }) => {
  await open(page)
  await (await shown(page, "invoiced")).click()
  await (await shown(page, "completed")).click({ modifiers: ["Shift"] })
  await expect(page.getByText("2 states and 0 steps selected.")).toBeVisible()
  await press(page, "c")
  await press(page, "v")
  await expect(node(page, "invoiced_copy")).toBeVisible()
  await expect(node(page, "completed_copy")).toBeVisible()
  await expect(page.getByText("2 states and 0 steps selected.")).toBeVisible()
  await page.getByRole("button", { name: "Delete selected" }).click()
  await expect(node(page, "invoiced_copy")).toHaveCount(0)
  // Select-all is the canvas's only while the canvas is in use: click it first.
  await clear(page)
  await press(page, "a")
  await expect(page.getByText(/^\d+ states and \d+ steps selected\.$/)).toBeVisible()
})

test("shortcuts: copy and select-all stay the browser's outside the canvas and over selected text", async ({ page }) => {
  await open(page)
  await (await shown(page, "invoiced")).click()
  // Selecting text elsewhere and copying copies the text: the canvas does not claim it.
  await page.getByText(/Draw how a process moves/).selectText()
  await press(page, "c")
  await expect(page.getByText(/^Copied \d+ items?\.$/)).toHaveCount(0)
  // With another modifier held, the shortcut is not the canvas's either.
  await (await shown(page, "invoiced")).click()
  await page.keyboard.press(`${process.platform === "darwin" ? "Meta" : "Control"}+Shift+c`)
  await expect(page.getByText(/^Copied \d+ items?\.$/)).toHaveCount(0)
  // A node focused and selected from the keyboard, with no pointer, makes copy the canvas's.
  await clear(page)
  await page.getByText(/Draw how a process moves/).click()
  await node(page, "completed").focus()
  await page.keyboard.press("Enter")
  await press(page, "c")
  await expect(page.getByText("Copied 1 item.")).toBeVisible()
  // Back on the canvas, copy is the canvas's again.
  await (await shown(page, "invoiced")).click()
  await press(page, "c")
  await expect(page.getByText("Copied 1 item.")).toBeVisible()
})

test("problems panel: lists each finding and jumps to the state it is on", async ({ page }) => {
  await open(page)
  await page.getByRole("button", { name: "State", exact: true }).click()
  await clear(page)
  await expect(page.getByTestId("problem")).toHaveCount(2)
  await page.getByTestId("problem").first().click()
  await expect(page.getByLabel("Name")).toHaveValue("new_state")
})

test("tidy up: lays the lifecycle out from its initial state, as one step undo puts back", async ({ page }) => {
  await open(page)
  // The person moves a state somewhere of their own; that layout is what undo must bring back.
  const moved = await (await shown(page, "approved")).boundingBox()
  await node(page, "approved").hover()
  await page.mouse.down(); await page.mouse.move((moved?.x ?? 0) + 60, (moved?.y ?? 0) + 160, { steps: 8 }); await page.mouse.up()
  const before = await placed(page)
  await page.getByRole("button", { name: "Tidy up" }).click()
  await expect.poll(async () => JSON.stringify(await placed(page))).not.toBe(JSON.stringify(before))
  const after = await placed(page)
  await page.waitForTimeout(400)
  // The order holds: the initial state leads, and every other state is reached by a step from a column to its left.
  // (A lifecycle may step back, so a step running right to left is allowed; each state's first way in is not.)
  const x = await lefts(page)
  const ends = await edgeEnds(page)
  expect(ends.length).toBeGreaterThan(0)
  for (const id of Object.keys(x)) {
    if (id === "draft") expect(x[id]).toBe(Math.min(...Object.values(x)))
    else expect(ends.some(([source, target]) => target === id && x[source]! < x[id]! - 1)).toBe(true)
  }
  // One undo restores the person's layout; redo tidies again.
  await press(page, "z")
  await expect.poll(async () => JSON.stringify(await placed(page))).toBe(JSON.stringify(before))
  await press(page, "Shift+z")
  await expect.poll(async () => JSON.stringify(await placed(page))).toBe(JSON.stringify(after))
})

test("step label: clicking a step's label selects the step, as clicking its line does", async ({ page }) => {
  await open(page)
  const label = page.locator('[data-edge-label="complete:invoiced"]')
  await reveal(page, label)
  await label.click()
  await expect(edge(page, "complete:invoiced")).toHaveClass(/selected/)
  await expect(page.getByLabel("Step id")).toHaveValue("complete")
})

test("focus: hovering or selecting a state lights its steps and dims the rest, without rebuilding any step", async ({ page }) => {
  await open(page)
  const opacity = (selector: string) => page.locator(selector).first().evaluate((element) => getComputedStyle(element).opacity)
  const approved = await shown(page, "approved")
  // Hovering must not rebuild a step: the same element is there before and after.
  await page.locator('.react-flow__edge[data-id="invoice:approved"]').evaluate((element) => { (element as HTMLElement & { marker?: number }).marker = 1 })
  await approved.hover()
  await expect.poll(() => opacity('.react-flow__edge[data-id="submit:draft"]')).toBe("0.35")
  await expect.poll(() => opacity('[data-edge-label="submit:draft"]')).toBe("0.35")
  await expect.poll(() => opacity('.react-flow__edge[data-id="invoice:approved"]')).toBe("1")
  expect(await page.locator('.react-flow__edge[data-id="invoice:approved"]').evaluate((element) => (element as HTMLElement & { marker?: number }).marker)).toBe(1)
  // Moving away clears it.
  await page.mouse.move(5, 5)
  await expect.poll(() => opacity('.react-flow__edge[data-id="submit:draft"]')).toBe("1")
  // Selecting holds the same focus.
  await approved.click()
  await page.mouse.move(5, 5)
  await expect(edge(page, "invoice:approved")).toHaveClass(/canvas-edge-active/)
  await expect.poll(() => opacity('.react-flow__edge[data-id="submit:draft"]')).toBe("0.35")
})

test("bundle: steps of one step into one state draw as one; hover lights its sources; select lists them; remove one, undo", async ({ page }) => {
  await open(page)
  const bundle = page.locator('[data-edge-label^="bundle:cancel:"]')
  await reveal(page, bundle)
  await expect(bundle).toHaveText(/^Cancel · from \d+ states$/)
  const count = Number((await bundle.textContent())?.match(/from (\d+)/)?.[1])
  // Its steps are not drawn one by one.
  await expect(page.locator('.react-flow__edge[data-id^="cancel:"]')).toHaveCount(0)
  // Hovering it lights its sources (an accent ring on each source card).
  // Read before any click, so the ring comes from hover alone; the card's shadow eases in (200ms), so poll until it settles.
  const ring = () => page.locator('.react-flow__node[data-id="draft"] .canvas-node').evaluate((element) => getComputedStyle(element).boxShadow)
  await page.locator('.react-flow__edge[data-id^="bundle:cancel:"]').first().hover({ force: true })
  await expect.poll(ring).toContain("0px 0px 0px 4px")
  await bundle.click()
  await expect(page.getByTestId("step-sources").locator("li")).toHaveCount(count)
  await expect(page.getByTestId("step-sources")).toContainText("Draft → Cancelled")
  // Removing one step leaves the bundle one source smaller; undo brings it back.
  await page.getByRole("button", { name: "Remove the step from Draft" }).click()
  await expect(bundle).toHaveText(`Cancel · from ${count - 1} states`)
  await press(page, "z")
  await expect(bundle).toHaveText(`Cancel · from ${count} states`)
})

test("bundle reconnect: re-point one step of a bundle to another state; the bundle shrinks, the step draws on its own; undo", async ({ page }) => {
  await open(page)
  const bundle = page.locator('[data-edge-label^="bundle:cancel:"]')
  await reveal(page, bundle)
  const count = Number((await bundle.textContent())?.match(/from (\d+)/)?.[1])
  await bundle.click()
  await page.getByLabel("Change target of the step from Approved").selectOption("on_hold")
  await expect(bundle).toHaveText(`Cancel · from ${count - 1} states`)
  await expect(page.locator('.react-flow__edge[data-id="cancel:approved"]')).toHaveCount(1)
  await expect(page.getByText(/Approved → On hold/).first()).toBeVisible()
  await press(page, "z")
  await expect(bundle).toHaveText(`Cancel · from ${count} states`)
  await expect(page.locator('.react-flow__edge[data-id="cancel:approved"]')).toHaveCount(0)
})
