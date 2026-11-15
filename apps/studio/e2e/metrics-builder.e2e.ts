import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * Explore a metric, end to end: open the builder, Run, and read the number with its chart. The
 * stand-ins are the alpha cell's own answers for its Holding metrics (recorded 2026-11-15, scope
 * ids replaced): the catalog, the checker's admission, the metrics with their unit partition, the
 * unit groups, and one group's weekly answer.
 */
const fixture = (name: string) => JSON.parse(readFileSync(new URL(`../test/fixtures/metrics-builder/${name}.json`, import.meta.url), "utf8")) as Record<string, unknown>

interface Asked { readonly groupBy?: readonly { readonly grain?: string }[]; readonly filter?: readonly unknown[] }

async function holdingsCell(page: Page) {
  const asked: Asked[] = []
  // Anything else the shell reads is not served here: Studio opens the recorded world around the builder.
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
  await page.route("**/api/questions/catalog", (route) => route.fulfill({ json: fixture("catalog") }))
  await page.route("**/api/questions/check", (route) => route.request().method() === "GET"
    ? route.fulfill({ json: { served: true } })
    : route.fulfill({ json: { ...fixture("check"), question: (route.request().postDataJSON() as { question: unknown }).question } }))
  await page.route("**/api/host/v1/questions:metrics**", (route) => route.fulfill({ json: fixture("metrics") }))
  await page.route("**/api/host/v1/questions:evaluate**", (route) => {
    const { question } = route.request().postDataJSON() as { question: Asked }
    asked.push(question)
    // The unit groups are asked without a time; one group's answer by week, held to that group.
    return route.fulfill({ json: fixture(question.groupBy?.some(({ grain }) => grain !== undefined) ? "weekly" : "unit-groups") })
  })
  return asked
}

for (const scheme of ["light", "dark"] as const) {
  test(`metrics: Explore a metric runs at once and answers with a number and a chart (${scheme})`, async ({ page }, info) => {
    test.skip(Boolean(process.env.E2E_BASE), "this row uses the recorded alpha answers as a stand-in")
    await page.emulateMedia({ colorScheme: scheme })
    await page.setViewportSize({ width: 1440, height: 900 })
    const asked = await holdingsCell(page)
    // An old address still opens the builder.
    await page.goto("/questions/new")
    await expect(page).toHaveURL((url) => url.pathname === "/metrics/new")
    await expect(page.getByRole("heading", { name: "Explore a metric" })).toBeVisible()
    // The server's own words for each measure, never the engine's label; fields in words.
    // A label wraps its select, so its text holds every option: each field is found by its data-field.
    await expect(page.locator('[data-field="measure.0"] select option:checked')).toHaveText("Quantity on hand")
    await expect(page.locator("main")).not.toContainText("last value per")
    await expect(page.locator('[data-field="by"] select option:checked')).toHaveText("Observed at")
    await expect(page.getByText("No conditions: every holding counts.")).toBeVisible()
    // The window is filled in: Run answers without a date to pick.
    await expect(page.locator('[data-field="window"] input').nth(0)).not.toHaveValue("")
    await expect(page.locator('[data-field="window"] input').nth(1)).not.toHaveValue("")
    await page.screenshot({ path: info.outputPath(`metrics-builder-before-${scheme}.png`) })
    await page.getByRole("button", { name: "Run", exact: true }).click()
    await expect(page.getByText("Checked by the server")).toBeVisible()
    // One unit group (grams on hand): its total, then its answer per week with the chart.
    const lead = page.locator('[data-slot="answer-lead"]')
    await expect(lead).toBeVisible()
    await expect(lead.locator(".recharts-area")).toBeVisible()
    await expect(page.locator("main")).not.toContainText(/No data yet|Nothing is run yet|Not run/)
    expect(asked.length).toBe(2)
    expect(asked[1]?.filter?.length).toBe(3)
    await page.screenshot({ path: info.outputPath(`metrics-builder-after-${scheme}.png`), fullPage: true })
  })
}

test("metrics: the rail says Metrics and its page offers Explore a metric", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "this row uses the recorded world")
  await holdingsCell(page)
  await page.goto("/questions")
  await expect(page).toHaveURL((url) => url.pathname === "/metrics")
  await expect(page.getByRole("toolbar", { name: "Applets" }).getByRole("link", { name: "Metrics" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Metrics", level: 1 })).toBeVisible()
  await expect(page.locator("main").getByRole("link", { name: "Explore a metric" })).toHaveAttribute("href", "/metrics/new")
})
