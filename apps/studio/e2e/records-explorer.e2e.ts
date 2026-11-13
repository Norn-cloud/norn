import { expect, test, type Page } from "@playwright/test"
import { objectCell, population, session, viewsCell } from "./object-cell.ts"

// The records explorer's verbs over the object, facets and definitions stand-ins: narrowing by a
// value's count, removing a chip, and saved views saved, opened, updated, renamed and deleted with undo.

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the object and definitions stand-ins`) })

async function explorer(page: Page) {
  await session(page)
  const cell = await objectCell(page, population(30))
  const definitions = await viewsCell(page)
  await page.goto("/objects/canon.Holding")
  await expect(page.getByText("30 records").first()).toBeVisible()
  return { cell, definitions }
}

const rail = (page: Page) => page.locator('aside [aria-label="Narrow by"]')
/** One value's button in one property's facet (the heading names the property). */
const value = (page: Page, property: string, text: string) => rail(page).locator("section")
  .filter({ has: page.getByRole("heading", { name: property, exact: true }) }).getByRole("button").filter({ hasText: text }).first()

test("facets: the rail counts each value; choosing one narrows the grid to it, as a chip; removing the chip widens it", async ({ page }) => {
  const { cell } = await explorer(page)
  // 30 holdings: every third is in the north (10), the rest in the south (20).
  await expect(rail(page)).toContainText("30 records, counted by the cell")
  await expect(value(page, "location", "south")).toContainText("20")
  await expect(value(page, "location", "north")).toContainText("10")
  await value(page, "location", "north").click()
  await expect(value(page, "location", "north")).toHaveAttribute("aria-pressed", "true")
  await expect(page.getByText("10 records").first()).toBeVisible()
  const chips = page.getByLabel("Applied filter")
  await expect(chips).toContainText("location is north")
  // The grid asked the cell for exactly that filter.
  const asked = cell.requests.filter((url) => url.pathname.endsWith("/v1/objects/canon.Holding")).at(-1)
  expect(JSON.parse(asked?.searchParams.get("filter") ?? "null")).toEqual({ all: [{ property: "location", op: "eq", value: "north" }] })
  // A property's own counts leave out its own choice, so a second value can join the first.
  await expect(value(page, "location", "south")).toContainText("20")
  await chips.getByRole("button", { name: "Remove location is north" }).click()
  await expect(page.getByLabel("Applied filter")).toHaveCount(0)
  await expect(page.getByText("30 records").first()).toBeVisible()
})

test("views: save, open, update, rename and delete with undo; an opened view has nothing unapplied", async ({ page }) => {
  const { definitions } = await explorer(page)
  await value(page, "location", "north").click()
  await expect(page.getByText("10 records").first()).toBeVisible()
  // Save as view: it opens as a tab, saved on the cell as a `view` definition.
  await page.getByRole("button", { name: "Save as view" }).click()
  await page.getByLabel("View name").fill("North holdings")
  await page.getByRole("button", { name: "Save view" }).click()
  const tab = page.getByRole("tab", { name: "North holdings" })
  await expect(tab).toHaveAttribute("aria-selected", "true")
  expect([...definitions.views.values()].map(({ payload }) => (payload as { title?: string }).title)).toEqual(["North holdings"])
  // Open every record, then the view again: its filter comes back, with nothing unapplied.
  await page.getByRole("tab", { name: "All records" }).click()
  await expect(page.getByText("30 records").first()).toBeVisible()
  await tab.click()
  await expect(page.getByText("10 records").first()).toBeVisible()
  await expect(page.getByLabel("Applied filter")).toContainText("location is north")
  await expect(page.getByLabel("Unapplied changes")).toHaveCount(0)
  // Update: narrow further, then save the view as it now is.
  const update = page.getByRole("button", { name: "Update view" })
  await expect(update).toBeDisabled()
  await value(page, "unit", "g").click()
  await expect(page.getByLabel("Applied filter")).toContainText("unit is g")
  await update.click()
  await expect(update).toBeDisabled() // the grid shows the view as saved again
  const saved = [...definitions.views.values()][0]
  expect(saved?.version).toBe(2)
  expect(JSON.stringify(saved?.payload)).toContain('"unit"')
  // Rename keeps the view, under its new title.
  await page.getByRole("button", { name: "Rename" }).click()
  await page.getByLabel("View name").fill("North FIFO holdings")
  await page.getByRole("button", { name: "Rename", exact: true }).last().click()
  await expect(page.getByRole("tab", { name: "North FIFO holdings" })).toBeVisible()
  // Delete retires it on the cell; Undo saves it back.
  await page.getByRole("button", { name: "Delete" }).click()
  await expect(page.getByRole("tab", { name: "North FIFO holdings" })).toHaveCount(0)
  expect([...definitions.views.values()].every(({ retired }) => retired)).toBe(true)
  await page.getByRole("button", { name: "Undo" }).click()
  await expect(page.getByRole("tab", { name: "North FIFO holdings" })).toBeVisible()
  expect([...definitions.views.values()].some(({ retired }) => !retired)).toBe(true)
})
