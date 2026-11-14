import { readFileSync } from "node:fs"
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
  await expect(page.getByText("30 records", { exact: true })).toBeVisible()
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
  await expect(page.getByText("10 records", { exact: true })).toBeVisible()
  const chips = page.getByLabel("Applied filter")
  await expect(chips).toContainText("location is north")
  // The grid asked the cell for exactly that filter.
  const asked = cell.requests.filter((url) => url.pathname.endsWith("/v1/objects/canon.Holding")).at(-1)
  expect(JSON.parse(asked?.searchParams.get("filter") ?? "null")).toEqual({ all: [{ property: "location", op: "eq", value: "north" }] })
  // A property's own counts leave out its own choice, so a second value can join the first.
  await expect(value(page, "location", "south")).toContainText("20")
  await chips.getByRole("button", { name: "Remove location is north" }).click()
  await expect(page.getByLabel("Applied filter")).toHaveCount(0)
  await expect(page.getByText("30 records", { exact: true })).toBeVisible()
})

test("views: save, open, update, rename and delete with undo; an opened view has nothing unapplied", async ({ page }) => {
  const { definitions } = await explorer(page)
  await value(page, "location", "north").click()
  await expect(page.getByText("10 records", { exact: true })).toBeVisible()
  // Save as view: it opens as a tab, saved on the cell as a `view` definition.
  await page.getByRole("button", { name: "Save as view" }).click()
  await page.getByLabel("View name").fill("North holdings")
  await page.getByRole("button", { name: "Save view" }).click()
  const tab = page.getByRole("tab", { name: "North holdings" })
  await expect(tab).toHaveAttribute("aria-selected", "true")
  expect([...definitions.views.values()].map(({ payload }) => (payload as { title?: string }).title)).toEqual(["North holdings"])
  // Open every record, then the view again: its filter comes back, with nothing unapplied.
  await page.getByRole("tab", { name: "All records" }).click()
  await expect(page.getByText("30 records", { exact: true })).toBeVisible()
  await tab.click()
  await expect(page.getByText("10 records", { exact: true })).toBeVisible()
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

test("facets that never arrive: the rail stops counting after its deadline, says the counts aren't available, and counts again on request", async ({ page }) => {
  test.setTimeout(60_000)
  await session(page)
  await objectCell(page, population(30))
  await viewsCell(page)
  // The first counts never come back (a cold or busy cell); the next ask is answered by the stand-in.
  let held = true
  await page.route("**/v1/objects/canon.Holding:facets**", (route) => held ? new Promise<void>(() => {}) : route.fallback())
  await page.goto("/objects/canon.Holding")
  await expect(page.getByText("30 records", { exact: true })).toBeVisible()
  // The rail renders twice (the side panel, and a disclosure for narrow screens); the side panel is read.
  const side = page.locator("aside")
  await expect(side.getByText("Counting values…")).toBeVisible()
  await expect(side.getByText("Counts aren't available right now.")).toBeVisible({ timeout: 25_000 })
  await expect(side.getByText("Counting values…")).toHaveCount(0)
  held = false
  await side.getByRole("button", { name: "Count again" }).click()
  await expect(rail(page)).toContainText("30 records, counted by the cell")
})

test("connections: a cell without the connection type says 'No connections yet' and never asks for it; with the type, a refused read is said quietly with a retry", async ({ page }) => {
  await session(page)
  await objectCell(page, population(3))
  await viewsCell(page)
  const asked: string[] = []
  await page.route("**/api/host/v1/objects/source.Connection**", (route) => {
    asked.push(route.request().url())
    return route.fulfill({ status: 403, json: { format: "norn/refusal@2", code: "authz.denied", message: "the read request is not authorized", reason: null } })
  })
  // The recorded three systems declare no source.Connection: nothing to read, and nothing is asked.
  await page.goto("/sources")
  await expect(page.getByText("No connections yet")).toBeVisible()
  await expect(page.getByText("Couldn't read the connections.")).toHaveCount(0)
  expect(asked).toHaveLength(0)
  // Once the cell declares the type, a refused read is the cell's answer: shown, quietly, never as "empty".
  const artifacts = JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown[]
  const connection = { artifactId: "source.Connection.entity", dependencies: [], irVersion: "0.1", kind: "entity", lifecycle: "active", name: "Connection", namespace: "source", version: "0.1.0",
    payload: { entityVersion: "2.0", objectType: { name: "source.Connection", implements: [], properties: [{ id: "status", type: { kind: "text" } }] } } }
  await page.route("**/api/host/v1/definitions?**", (route) => route.fulfill({ json: { artifacts: [...artifacts, connection] } }))
  await page.reload()
  const unread = page.locator('[data-connections="unread"]')
  await expect(unread).toContainText("Couldn't read the connections.")
  await expect(unread).toContainText("the read request is not authorized")
  await expect(unread.getByRole("button", { name: "Read again" })).toBeVisible()
  await expect(page.getByText("No connections yet")).toHaveCount(0)
  await expect(page.getByRole("alert").filter({ hasText: "not authorized" })).toHaveCount(0)
  expect(asked.length).toBeGreaterThan(0)
})

test("questions: the first question asks its own type's profile at once, gives up on it after its budget, and answers before any question below it asks the cell", async ({ page }) => {
  test.setTimeout(60_000)
  await session(page)
  await viewsCell(page)
  const metric = (name: string, objectType: string, timeProperty: string) => ({ name, label: "quantity", kind: "simple", objectType, expression: "quantity", timeProperty, timeLinks: [], numerator: null, denominator: null })
  await page.route("**/api/host/v1/questions:metrics**", (route) => route.fulfill({ json: { metrics: [metric("m.delivery", "canon.Delivery", "occurred_at"), metric("m.orderline", "canon.OrderLine", "due_at"), metric("m.holding", "canon.Holding", "observed_at")] } }))
  const order: string[] = []
  // A cold cell: Delivery's profile never comes back; the others take 12 s each.
  await page.route("**/api/host/v1/objects/*:profile**", async (route) => {
    const type = decodeURIComponent(new URL(route.request().url()).pathname.split("/v1/objects/")[1] ?? "").replace(":profile", "")
    order.push(`profile ${type}`)
    if (type === "canon.Delivery") return new Promise<void>(() => {})
    await new Promise((resolve) => setTimeout(resolve, 12_000))
    return route.fulfill({ json: { objectType: type, rows: 10, withheldRows: 0, quarantinedRows: 0, properties: [] } })
  })
  const answer = JSON.parse(readFileSync(new URL("../../../fixtures/demo/answers/scrap-yield.k1.answer.json", import.meta.url), "utf8")) as unknown
  await page.route("**/api/host/v1/questions:evaluate**", (route) => { order.push("evaluate"); return route.fulfill({ json: answer }) })
  const started = Date.now()
  await page.goto("/questions")
  const first = page.locator("[data-slot=panel]").first()
  await expect(first.locator("[data-slot=answer-lead]")).toBeVisible({ timeout: 10_000 })
  expect(Date.now() - started).toBeLessThan(10_000)
  await expect(first.getByText("Latest 52 weeks")).toBeVisible()
  // The first question asked its own profile and its answer before any question below it asked anything.
  expect(order[0]).toBe("profile canon.Delivery")
  expect(order[1]).toBe("evaluate")
})
