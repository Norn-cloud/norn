import { expect, test, type Page } from "@playwright/test"
import { clear, node, open, shown } from "./canvas.ts"

/** The canvas's zoom, from its viewport transform. */
const zoom = (page: Page) => page.locator(".react-flow__viewport").evaluate((element) => new DOMMatrix(getComputedStyle(element).transform).a)

// Nodes open readable: every node title is at least 12px on screen at the default view, on every canvas.
for (const tab of [undefined, "Automation"]) test(`legibility: node titles are 12px or larger at the default view (${tab ?? "lifecycle"})`, async ({ page }) => {
  await open(page, tab)
  const titles = page.locator("[data-node-title]")
  await expect(titles.first()).toBeVisible()
  const scale = await zoom(page)
  const sizes = await titles.evaluateAll((elements) => elements.map((element) => Number.parseFloat(getComputedStyle(element).fontSize)))
  expect(sizes.length).toBeGreaterThan(0)
  expect(Math.min(...sizes) * scale).toBeGreaterThanOrEqual(12)
})

// A graph too wide to fit readable opens framed on where it starts, and the user pans to the rest.
test("legibility: a wide lifecycle opens at the readable zoom on its initial state and pans to the rest", async ({ page }) => {
  await open(page)
  expect(await zoom(page)).toBeGreaterThanOrEqual(0.85)
  const surface = await page.locator("[data-canvas-surface]").boundingBox()
  const draft = await node(page, "draft").boundingBox()
  expect(draft && surface && draft.x >= surface.x && draft.x + draft.width <= surface.x + surface.width).toBe(true)
  await (await shown(page, "completed")).click()
  await expect(page.getByLabel("Name")).toHaveValue("completed")
})

test("minimap: shown by default, and the toolbar toggles it", async ({ page }) => {
  await open(page)
  const minimap = page.locator(".react-flow__minimap")
  await expect(minimap).toBeVisible()
  await page.getByRole("button", { name: "Minimap" }).click()
  await expect(minimap).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Minimap" })).toHaveAttribute("aria-pressed", "false")
  await page.getByRole("button", { name: "Minimap" }).click()
  await expect(minimap).toBeVisible()
})

// Narrower than the rail width, the inspector is a drawer: selecting opens it, Esc or × closes it.
test.describe("below 1440px", () => {
  test.use({ viewport: { width: 1280, height: 900 } })
  test("inspector drawer: select opens, Esc and × close, clearing the selection closes", async ({ page }) => {
    await open(page)
    const drawer = page.getByRole("dialog", { name: "Inspector" })
    await expect(drawer).toBeHidden()
    await (await shown(page, "draft")).click()
    await expect(drawer).toBeVisible()
    await expect(drawer.getByLabel("Name")).toHaveValue("draft")
    await page.keyboard.press("Escape")
    await expect(drawer).toBeHidden()
    await (await shown(page, "submitted")).click()
    await expect(drawer).toBeVisible()
    await drawer.getByRole("button", { name: "Close inspector" }).click()
    await expect(drawer).toBeHidden()
    await (await shown(page, "draft")).click()
    await expect(drawer).toBeVisible()
    await clear(page)
    await expect(drawer).toBeHidden()
  })
})
