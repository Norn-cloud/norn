import { expect, test, type Page } from "@playwright/test"
import { objectCell, population, session } from "./object-cell.ts"

// One test per row of the records grid's verb matrix (see the PR), plus each state the grid shows.
// They use a stand-in for CELL-DO's object route, so they skip on a deployed stage until it serves one.
// SHOTS=<dir> saves every state in light and dark for review.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(150)
    await page.screenshot({ path: `${shots}/grid-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

const grid = (page: Page) => page.getByRole("grid", { name: "Holding records" })
const rows = (page: Page) => grid(page).locator('[role="row"][aria-rowindex]:not([aria-rowindex="1"])')
const firstCell = (page: Page, column: number) => rows(page).first().locator('[role="gridcell"]').nth(column)

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the object-route stand-in`) })

test("view: browse, typed cells, virtualized rows, columns, server sort, paging and page size", async ({ page }) => {
  await session(page)
  const cell = await objectCell(page, population(500))
  await page.goto("/objects/canon.Holding")
  await expect(page.getByText("500 records")).toBeVisible()
  await expect(firstCell(page, 0)).toHaveText("demo/holding/0001")
  // Exact decimal text with its unit; enums as declared, never reworded.
  await expect(rows(page).first()).toContainText("37.007 g")
  await expect(rows(page).first()).toContainText("moving_average")
  // Only the rows in view are in the DOM.
  expect(await rows(page).count()).toBeLessThan(50)
  await shoot(page, "live")

  await page.getByRole("button", { name: "Columns" }).click()
  await page.getByRole("menuitemcheckbox", { name: "unit" }).click()
  await page.getByRole("menuitemcheckbox", { name: "cost currency" }).click()
  await page.keyboard.press("Escape")
  await expect(page.getByRole("columnheader", { name: "unit", exact: true })).toHaveCount(0)
  await expect(page.getByRole("columnheader", { name: "cost currency" })).toBeVisible()

  await page.getByRole("columnheader", { name: "quantity", exact: true }).getByRole("button").click()
  await expect(page.getByRole("columnheader", { name: "quantity", exact: true })).toHaveAttribute("aria-sort", "ascending")
  expect(cell.requests.at(-1)?.searchParams.get("sort")).toBe("quantity:asc")
  await page.getByRole("columnheader", { name: "location", exact: true }).getByRole("button").click({ modifiers: ["Shift"] })
  expect(cell.requests.at(-1)?.searchParams.get("sort")).toBe("quantity:asc,location:asc")
  await page.getByRole("columnheader", { name: "quantity", exact: true }).getByRole("button").click()
  expect(cell.requests.at(-1)?.searchParams.get("sort")).toBe("quantity:desc")
  await shoot(page, "sorted")

  await page.getByRole("button", { name: "Next" }).click()
  await expect(page.getByText("Page 2")).toBeVisible()
  expect(cell.requests.at(-1)?.searchParams.get("after")).toBeTruthy()
  await page.getByRole("button", { name: "Previous" }).click()
  await expect(page.getByText("Page 1")).toBeVisible()
  await page.getByLabel("Per page").selectOption("200")
  expect(cell.requests.at(-1)?.searchParams.get("limit")).toBe("200")
  await expect(page.getByText("Page 1")).toBeVisible()
})

test("view: keyboard moves through rows and opens the record drawer", async ({ page }) => {
  await session(page)
  await objectCell(page, population(120))
  await page.goto("/objects/canon.Holding")
  await expect(page.getByText("120 records")).toBeVisible()
  await grid(page).focus()
  await page.keyboard.press("ArrowDown")
  await page.keyboard.press("ArrowDown")
  await expect(grid(page)).toHaveAttribute("aria-activedescendant", "row-2")
  await page.keyboard.press("End")
  await expect(grid(page)).toHaveAttribute("aria-activedescendant", "row-49")
  await page.keyboard.press("Home")
  await page.keyboard.press("Enter")
  const drawer = page.getByRole("dialog")
  await expect(drawer.getByRole("heading", { name: "demo/holding/0001" })).toBeVisible()
  await expect(drawer).toContainText("37.007 g")
  await shoot(page, "drawer")
  await page.keyboard.press("Escape")
  await expect(drawer).toHaveCount(0)
})

test("view: the filter runs on the server; no match and clear", async ({ page }) => {
  await session(page)
  const cell = await objectCell(page, population(300))
  await page.goto("/objects/canon.Holding")
  await expect(page.getByText("300 records")).toBeVisible()
  await page.getByRole("button", { name: "Filter" }).click()
  await page.getByLabel("Condition 1 property").selectOption("valuation_method")
  await page.getByLabel("Condition 1 value").selectOption("fifo")
  await page.getByRole("button", { name: "Add condition" }).click()
  await page.getByLabel("Condition 2 property").selectOption("quantity")
  await page.getByLabel("Condition 2 test").selectOption("gte")
  await page.getByLabel("Condition 2 value").fill("500.000")
  await expect(page.getByText("Not applied yet")).toBeVisible()
  await shoot(page, "filter-editor")
  await page.getByRole("button", { name: "Apply" }).click()
  const sent = JSON.parse(cell.requests.at(-1)?.searchParams.get("filter") ?? "null")
  expect(sent).toEqual({ all: [{ property: "valuation_method", op: "eq", value: "fifo" }, { property: "quantity", op: "gte", value: "500.000" }] })
  await expect(page.getByText(/^\d+ records?$/)).not.toHaveText("300 records")
  await page.getByRole("button", { name: /Filter/ }).click()
  await page.getByLabel("Condition 2 value").fill("99999.000")
  await page.getByRole("button", { name: "Apply" }).click()
  await expect(page.getByText("No records match this filter.")).toBeVisible()
  await shoot(page, "no-match")
  await page.getByRole("button", { name: "Clear filter" }).click()
  await expect(page.getByText("300 records")).toBeVisible()
  expect(cell.requests.at(-1)?.searchParams.get("filter")).toBeNull()
})

test("validate: the cell's refusal shows under the condition it points at; fixing it applies", async ({ page }) => {
  await session(page)
  await objectCell(page, population(80))
  await page.goto("/objects/canon.Holding")
  await page.getByRole("button", { name: "Filter" }).click()
  await page.getByLabel("Condition 1 property").selectOption("quantity")
  await page.getByLabel("Condition 1 test").selectOption("lt")
  await page.getByLabel("Condition 1 value").fill("12,5")
  await page.getByRole("button", { name: "Apply" }).click()
  await expect(page.getByRole("alert").filter({ hasText: "The cell refused this query." })).toBeVisible()
  await page.getByRole("button", { name: /Filter/ }).click()
  await expect(page.getByRole("alert").filter({ hasText: "12,5 is not an exact decimal number" })).toBeVisible()
  await shoot(page, "refused")
  await page.getByLabel("Condition 1 value").fill("12.5")
  await page.getByRole("button", { name: "Apply" }).click()
  await expect(page.getByText(/^\d+ records?$/)).toBeVisible()
  await expect(page.getByText("The cell refused this query.")).toHaveCount(0)
})

test("reopen: a record opens from the grid, closes, reopens, and opens in full", async ({ page }) => {
  await session(page)
  await objectCell(page, population(60))
  await page.goto("/objects/canon.Holding")
  await rows(page).nth(3).click()
  await expect(page.getByRole("dialog").getByRole("heading", { name: "demo/holding/0004" })).toBeVisible()
  await page.getByRole("button", { name: "Close" }).click()
  await expect(page.getByRole("dialog")).toHaveCount(0)
  await rows(page).nth(3).click()
  await expect(page.getByRole("dialog").getByRole("heading", { name: "demo/holding/0004" })).toBeVisible()
  await expect(page.getByRole("link", { name: "Open full record" })).toHaveAttribute("href", "/sources/canon.Holding/demo/holding/0004")
})

test("withheld rows read Withheld everywhere, sort last, and never match a filter", async ({ page }) => {
  await session(page)
  const cell = await objectCell(page, population(40, 4))
  await page.goto("/objects/canon.Holding")
  const withheld = grid(page).locator('[data-withheld="true"]')
  await expect(withheld.first()).toContainText("Withheld")
  await expect(withheld.first()).not.toContainText(" g")
  await shoot(page, "withheld")
  await withheld.first().click()
  await expect(page.getByRole("dialog")).toContainText("Withheld: The server withheld this record's values")
  await page.keyboard.press("Escape")
  await page.getByRole("columnheader", { name: "quantity", exact: true }).getByRole("button").click()
  await expect(rows(page).first()).not.toHaveAttribute("data-withheld", "true")
  await page.getByRole("button", { name: "Filter" }).click()
  await page.getByLabel("Condition 1 property").selectOption("location")
  await page.getByLabel("Condition 1 test").selectOption("present")
  await page.getByRole("button", { name: "Apply" }).click()
  await expect(page.getByText("30 records")).toBeVisible()
  expect(cell.requests.length).toBeGreaterThan(1)
})

test("a commit under the cursor reloads from page one and says so; an outage offers retry", async ({ page }) => {
  await session(page)
  const cell = await objectCell(page, population(200))
  await page.goto("/objects/canon.Holding")
  await page.getByRole("button", { name: "Next" }).click()
  await expect(page.getByText("Page 2")).toBeVisible()
  cell.commit()
  await page.getByRole("button", { name: "Next" }).click()
  await expect(page.getByText("Data changed, reloaded.")).toBeVisible()
  await expect(page.getByText("Page 1")).toBeVisible()
  await expect(firstCell(page, 0)).toHaveText("demo/holding/0001")
  await shoot(page, "reloaded")
  cell.failNext()
  await page.getByRole("button", { name: "Next" }).click()
  await expect(page.getByRole("button", { name: "Retry" })).toBeVisible()
  await shoot(page, "error")
  await page.getByRole("button", { name: "Retry" }).click()
  await expect(page.getByText("Page 2")).toBeVisible()
})

test("while the cell has no object route, the recording shows and sort and filter say why they're off", async ({ page }) => {
  await session(page)
  await page.route("**/api/host/v1/objects/**", (route) => route.fulfill({ status: 404, body: "" }))
  await page.goto("/objects/canon.Item")
  await expect(page.getByRole("grid", { name: "Item records" })).toContainText("Gold scrap")
  await expect(page.getByText("Recorded", { exact: true })).toBeVisible()
  await expect(page.getByText("This cell doesn't sort or filter records on the server yet.")).toBeVisible()
  await expect(page.getByRole("button", { name: "Filter" })).toBeDisabled()
  await expect(page.getByRole("columnheader", { name: "code" }).getByRole("button")).toBeDisabled()
  await shoot(page, "recorded")
})

test("a workspace with no data cell says so and calls nothing", async ({ page }) => {
  await session(page, null)
  let calls = 0
  await page.route("**/api/host/**", (route) => { calls += 1; return route.fulfill({ status: 500 }) })
  await page.goto("/objects/canon.Holding")
  await expect(page.getByText("This workspace has no data cell yet.")).toBeVisible()
  await expect(page.getByRole("grid")).toHaveCount(0)
  expect(calls).toBe(0)
  await shoot(page, "no-cell")
})

test("loading shows placeholder rows, and an empty type says nothing is stored yet", async ({ page }) => {
  await session(page)
  let release: () => void = () => {}
  const held = new Promise<void>((resolve) => { release = resolve })
  await page.route("**/api/host/v1/objects/**", async (route) => {
    await held
    return route.fulfill({ json: { objectType: "canon.Holding", records: [], quarantineCount: 0, scannedThrough: null, query: { sort: [], filter: null }, next: null, total: { count: 0, approximate: false } } })
  })
  await page.goto("/objects/canon.Holding")
  await expect(page.getByLabel("Reading records")).toBeVisible()
  await shoot(page, "loading")
  release()
  await expect(page.getByText("No holding records are stored yet.")).toBeVisible()
  await shoot(page, "empty")
})
