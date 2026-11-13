import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

// Typed transforms (ViewIR v1, #341): the list, a transform's rows, one row's lineage and the plan
// graph. The cell routes are stubbed with test/fixtures/transforms.json, which
// crates/norn-api/tests/transform_studio.rs checks byte for byte against what the routes serve.
// SHOTS=<dir> saves each view in light and dark for review.

const fixture = JSON.parse(readFileSync(new URL("../test/fixtures/transforms.json", import.meta.url), "utf8")) as {
  readonly catalog: unknown
  readonly rows: { readonly rows: readonly { readonly rowId: string }[]; readonly nodeRows?: Readonly<Record<string, number>> }
  readonly lineage: Readonly<Record<string, unknown>>
  readonly plan: unknown
}
const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(500)
    await page.screenshot({ path: `${shots}/transform-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the transform route stand-ins`) })

async function cell(page: Page) {
  await session(page)
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.route("**/api/host/v1/definitions?**", (route) => route.fulfill({ json: fixture.catalog }))
  await page.route("**/api/host/v1/definitions", (route) => route.fulfill({ json: fixture.catalog }))
  await page.route("**/api/host/v1/transforms/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/plan")) return route.fulfill({ json: fixture.plan })
    if (path.endsWith("/rows")) return route.fulfill({ json: fixture.rows })
    const lineage = /\/rows\/([^/]+)\/lineage$/.exec(path)
    const document = lineage ? fixture.lineage[decodeURIComponent(lineage[1]!)] : undefined
    return document === undefined
      ? route.fulfill({ status: 404, json: { code: "transform.row_not_found", message: "No such row." } })
      : route.fulfill({ json: document })
  })
}

test("view: the transforms list names each installed transform and the types it reads", async ({ page }) => {
  await cell(page)
  await page.goto("/transforms")
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Transforms")
  await expect(page.getByRole("link", { name: "Weekly revenue by region" })).toBeVisible()
  await expect(page.getByText("shop.Order", { exact: true })).toBeVisible()
  await shoot(page, "list")
})

test("view: a transform's rows carry their column contracts and exact numbers", async ({ page }) => {
  await cell(page)
  await page.goto("/transforms/shop.revenue_by_region")
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Weekly revenue by region")
  await expect(page.getByText(`${fixture.rows.rows.length} rows`)).toBeVisible()
  // Business: a column's contract is in its header's tooltip, and a weekly bucket reads as its week.
  await expect(page.getByRole("columnheader", { name: /revenue/ }).first().locator("[title]")).toHaveAttribute("title", /additive/)
  await expect(page.getByRole("cell", { name: "Week of 26 Oct 2026" }).first()).toBeVisible()
  await expect(page.getByRole("cell", { name: "north" }).first()).toBeVisible()
  // Decimals are written to their declared scale, as everywhere in Studio (195.5 at decimal<2>).
  await expect(page.getByRole("cell", { name: "195.50" }).first()).toBeVisible()
  await expect(page.getByRole("cell", { name: "195.5", exact: true })).toHaveCount(0)
  await shoot(page, "rows")
  // Technical: the contract is a second header line and the bucket is its exact date. The wider
  // headers overflow: the hidden columns are signalled, and scrolled to the end the first column stays put.
  await page.getByRole("radio", { name: "Technical" }).click()
  await expect(page.getByRole("columnheader", { name: /revenue/ }).first()).toContainText("additive")
  await expect(page.getByRole("cell", { name: "2026-10-26" }).first()).toBeVisible()
  await expect(page.locator("[data-edge=right]")).toBeVisible()
  await shoot(page, "rows-technical")
  await page.locator("[data-slot=table-container]").first().evaluate((scroller) => { scroller.scrollLeft = scroller.scrollWidth })
  await expect(page.locator("[data-edge=right]")).toHaveCount(0)
  await expect(page.getByRole("cell", { name: "east" }).first()).toBeInViewport()
  await shoot(page, "rows-scrolled")
  await page.getByRole("radio", { name: "Business" }).click()
})

test("view: a row's lineage lists the records it derives from, each a link to its record", async ({ page }) => {
  await cell(page)
  await page.goto("/transforms/shop.revenue_by_region")
  await page.getByRole("button", { name: /records/ }).first().click()
  await expect(page.getByRole("tab", { name: "Lineage" })).toHaveAttribute("aria-selected", "true")
  await expect(page.getByRole("link", { name: /^o-10/ }).first()).toBeVisible()
  await shoot(page, "lineage")
})

test("view: the plan draws every admitted operator and the output's column lineage", async ({ page }) => {
  await cell(page)
  await page.goto("/transforms/shop.revenue_by_region")
  await page.getByRole("tab", { name: "Plan" }).click()
  await expect(page.getByText("Definition graph")).toBeVisible()
  for (const op of ["scan", "filter", "join", "project", "aggregate", "window"]) await expect(page.getByText(op, { exact: true }).first()).toBeVisible()
  // The output is selected first: its columns and their lineage.
  await expect(page.getByRole("heading", { name: "Node ranked" })).toBeVisible()
  await expect(page.getByText("from order.amount").first()).toBeVisible()
  // Read at full size: the plan scrolls sideways rather than shrinking.
  await expect(page.locator("[data-node=order]")).toHaveCSS("width", "220px")
  await shoot(page, "plan")
  // Selecting in the list selects in the graph and shows the node's definition.
  const nodes = page.getByRole("table", { name: "Plan nodes" })
  await nodes.getByRole("button", { name: "weekly", exact: true }).click()
  await expect(page.getByRole("heading", { name: "Node weekly" })).toBeVisible()
  // Business says what the operator does; Technical shows its declaration.
  await expect(page.getByLabel("What weekly does")).toContainText("Totals of regional per region and placed at by week")
  await page.getByRole("radio", { name: "Technical" }).click()
  await expect(page.getByLabel("Definition of weekly")).toContainText("groupBy")
  await page.getByRole("radio", { name: "Business" }).click()
  await expect(page.locator("[data-node=weekly][data-selected]")).toBeVisible()
  // Selecting in the graph selects in the list.
  await page.locator("[data-node=joined]").click()
  await expect(nodes.getByRole("row", { selected: true })).toContainText("joined")
  await expect(page.getByLabel("What joined does")).toContainText("Left join customer on paid.customer = customer.id")
  // The volume lens shows the rows each node produced, as the cell reports them.
  const volumes = fixture.rows.nodeRows ?? {}
  await page.getByRole("radio", { name: "volume" }).click()
  await expect(page.locator("[data-node=order]")).toContainText(new Intl.NumberFormat("en").format(volumes["order"] ?? -1))
  await expect(nodes.getByRole("row", { name: /^ranked/ })).toContainText(new Intl.NumberFormat("en").format(volumes["ranked"] ?? -1))
  await shoot(page, "plan-volume")
})

test("view: a cell without the transform routes says so instead of showing nothing", async ({ page }) => {
  await session(page)
  await page.route("**/api/host/v1/definitions**", (route) => route.fulfill({ json: fixture.catalog }))
  await page.route("**/api/host/v1/transforms/**", (route) => route.fulfill({ status: 404, json: { code: "route.not_found", message: "no such route" } }))
  await page.goto("/transforms/shop.revenue_by_region")
  await expect(page.getByText("Not served", { exact: true })).toBeVisible()
})
