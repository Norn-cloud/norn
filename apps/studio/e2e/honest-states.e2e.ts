import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * Browser proof rows for #1424 honest states:
 * 1. Slow response -> skeleton -> quiet slow hint -> bounded timeout with Retry that recovers.
 * 2. Server error -> plain-word reason, server explanation never hidden, raw worker noise tucked under Technical, Retry recovers.
 * 3. Broken / unreachable cell (`503 cell.unopenable`, `retryable: false`) -> never "Retrying…" forever; honest Workspace unavailable with incidentId, Last reachable, and Retry.
 * 4. Empty states with clear next steps across surfaces.
 * 5. Jargon-free copy in Business mode vs raw technical formula/conservation text under Technical mode.
 *
 * SHOTS=<dir> saves light and dark screenshots for each row.
 */

const shots = process.env.SHOTS
const definitions = JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown

async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(350)
    await page.screenshot({ path: `${shots}/honest-states-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses deterministic stand-ins for error/timeout states`) })
test.beforeEach(async ({ page }) => { await page.setViewportSize({ width: 1280, height: 900 }) })

test("slow response: shows skeleton, then slow notice, then bounded timeout with Retry that recovers", async ({ page }) => {
  await session(page)
  let release: (() => void) | null = null
  let attempt = 0
  await page.route("**/api/host/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: definitions } })
    if (path.endsWith("/v1/questions:metrics")) {
      attempt += 1
      if (attempt === 1) {
        await new Promise<void>((resolve) => { release = resolve })
        return route.abort()
      }
      return route.fulfill({ json: { metrics: [] } })
    }
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })

  await page.clock.install()
  await page.goto("/metrics")

  // Skeleton renders immediately while request is pending.
  await expect(page.locator('[data-load-state="loading"]')).toBeVisible()
  await expect(page.getByRole("status", { name: "Reading metrics" })).toBeVisible()

  // Past SLOW_WAIT_MS (4 s): quiet slow hint appears beneath the skeleton.
  await page.clock.runFor(4_500)
  await expect(page.locator('[data-load-state="slow"]')).toBeVisible()
  await expect(page.getByText("Taking longer than usual to reach the workspace…")).toBeVisible()

  // Past BOUNDED_WAIT_MS (12 s): transitions out of skeleton into an honest timed-out state with Retry.
  await page.clock.runFor(8_000)
  const timedOut = page.locator('[data-load-state="timed-out"]')
  await expect(timedOut).toBeVisible()
  await expect(timedOut).toContainText("Taking too long to respond")
  await expect(timedOut).toContainText("Last reachable")
  await shoot(page, "slow-timeout")

  // Retry unblocks the stalled request and resolves to the honest empty state.
  release?.()
  await timedOut.getByRole("button", { name: "Retry" }).click()
  await expect(page.locator('[data-load-state="empty"]')).toBeVisible()
  await expect(page.getByText("No metrics yet")).toBeVisible()
})

test("server error: shows plain reason and server explanation (never hidden), tucks raw worker noise under Technical, and Retry recovers", async ({ page }) => {
  await session(page)
  let fail = true
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: definitions } })
    if (path.endsWith("/v1/questions:metrics")) {
      if (fail) {
        return route.fulfill({
          status: 500,
          json: {
            format: "norn/refusal@2",
            code: "host.internal",
            category: "internal",
            retryable: true,
            incidentId: "inc-metrics-500",
            message: "Worker failed: handler is waiting on nothing",
            explanation: "The query planner could not open the metrics catalog.",
          },
        })
      }
      return route.fulfill({ json: { metrics: [] } })
    }
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })

  await page.goto("/metrics")
  const alert = page.locator("[data-workspace-unavailable]")
  await expect(alert).toBeVisible()
  // Plain Business headline + server explanation visible + incident id present.
  await expect(alert).toContainText("The workspace hit an internal problem and couldn't finish this request.")
  await expect(alert).toContainText("The query planner could not open the metrics catalog.")
  await expect(alert).toContainText("inc-metrics-500")
  // Raw worker string stays collapsed inside Technical details in Business mode.
  const details = alert.locator("details")
  await expect(details).not.toHaveAttribute("open", "")
  await shoot(page, "server-error")

  // Opening Technical reveals the exact code and raw worker message.
  await details.locator("summary").click()
  await expect(details).toContainText("host.internal")
  await expect(details).toContainText("Worker failed: handler is waiting on nothing")

  // Retry recovers cleanly.
  fail = false
  await alert.getByRole("button", { name: "Retry" }).click()
  await expect(page.locator('[data-load-state="empty"]')).toBeVisible()
})

test("unreachable cell: 503 cell.unopenable (retryable: false) never shows Retrying forever and displays Workspace unavailable with incidentId, Last reachable and Retry", async ({ page }) => {
  await session(page)
  let unopenable = false
  let definitionHits = 0
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) {
      definitionHits += 1
      if (unopenable) {
        return route.fulfill({
          status: 503,
          json: {
            format: "norn/refusal@2",
            code: "cell.unopenable",
            category: "internal",
            retryable: false,
            incidentId: "inc-cell-909",
            message: "The workspace cell could not be opened.",
            explanation: "Storage migration is paused for this environment.",
          },
        })
      }
      return route.fulfill({ json: { artifacts: definitions } })
    }
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: { scanLimit: 1000, sections: [], items: [] } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })

  // First load reaches the cell once so Last reachable is recorded.
  await page.goto("/")
  await expect(page.getByText("Nothing needs you")).toBeVisible()

  // Now the cell fails with non-retryable 503 cell.unopenable on reload/navigation.
  unopenable = true
  const hitsBefore = definitionHits
  await page.getByRole("link", { name: "Sources" }).click()
  await page.reload()

  // Must NEVER render a persistent "Retrying…" banner; shows honest Workspace unavailable immediately.
  const banner = page.locator('[data-connection="unavailable"]')
  await expect(banner).toBeVisible()
  await expect(banner).toContainText("Workspace unavailable")
  await expect(banner).toContainText("Storage migration is paused for this environment.")
  await expect(banner).toContainText("inc-cell-909")
  await expect(banner).toContainText("Last reachable")
  await expect(page.locator('[data-connection="retrying"]')).toHaveCount(0)
  await expect(page.getByText("Retrying…")).toHaveCount(0)
  await expect(banner.getByRole("button", { name: "Retry" })).toBeEnabled()
  await shoot(page, "cell-unavailable")

  // Wait briefly to confirm automatic retries do NOT spin for retryable: false.
  const hitsAfterFail = definitionHits
  await page.waitForTimeout(1_500)
  expect(definitionHits).toBe(hitsAfterFail)
  expect(hitsAfterFail).toBeGreaterThan(hitsBefore)

  // Clicking Retry once the cell recovers restores the live workspace.
  unopenable = false
  await banner.getByRole("button", { name: "Retry" }).click()
  await expect(banner).toHaveCount(0)
})

test("empty states: Metrics, Dashboards and Inbox each show an honest empty state with a next step", async ({ page }) => {
  await session(page)
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: definitions } })
    if (path.endsWith("/v1/questions:metrics")) return route.fulfill({ json: { metrics: [] } })
    if (path.endsWith("/v1/definitions:list")) return route.fulfill({ json: { items: [], cursor: null } })
    if (path.endsWith("/v1/decisions:requested")) return route.fulfill({ json: { items: [], serverNowMs: Date.now() } })
    if (path.endsWith("/v1/decisions")) return route.fulfill({ json: { items: [], total: 0, nextCursor: null, serverNowMs: Date.now() } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
  await page.route("**/api/questions/catalog", (route) => route.fulfill({ status: 404, json: {} }))

  await page.goto("/metrics")
  await expect(page.locator('[data-load-state="empty"]')).toContainText("No metrics yet")
  await expect(page.getByRole("link", { name: "Open Sources" })).toBeVisible()

  await page.goto("/dashboards")
  await expect(page.locator('[data-load-state="empty"]')).toContainText("No dashboards yet")
  await expect(page.getByRole("button", { name: "New dashboard" }).first()).toBeVisible()
  await shoot(page, "empty-dashboards")

  await page.goto("/decisions")
  await expect(page.getByText("Nothing waits on you")).toBeVisible()
  await expect(page.getByRole("link", { name: "View workflows" })).toBeVisible()
})

test("jargon-free copy: derived metrics and restatements read in plain words in Business mode and keep raw formulas under Technical", async ({ page }) => {
  await session(page)
  const rawFormula = "sum(completed_count) / sum(application_count); sum(completion_rate); conservation []"
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: definitions } })
    if (path.endsWith("/v1/questions:metrics")) {
      return route.fulfill({
        json: {
          metrics: [
            {
              name: "demo.completion_rate",
              label: rawFormula,
              display: "completion rate",
              kind: "ratio",
              objectType: "demo.Conversion",
              expression: null,
              timeProperty: "occurred_at",
              numerator: "completed_count",
              denominator: "application_count",
            },
          ],
        },
      })
    }
    if (path.includes("/v1/objects/") && path.endsWith(":profile")) {
      return route.fulfill({ json: { objectType: "demo.Conversion", rows: 0, withheldRows: 0, quarantinedRows: 0, properties: [] } })
    }
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })

  await page.goto("/metrics")
  // Business mode: plain heading, no raw `sum(...)` or `conservation []`.
  await expect(page.getByText("Completion rate")).toBeVisible()
  await expect(page.locator("main")).not.toContainText("conservation []")
  await expect(page.locator("main")).not.toContainText("sum(completed_count)")
  await shoot(page, "plain-business")

  // Switch to Technical mode via Account menu: raw deriver formula stays accessible for builders.
  await page.getByRole("button", { name: "Account" }).click()
  await page.getByRole("menuitemradio", { name: "Technical" }).click()
  await expect(page.locator("main")).toContainText(rawFormula)
  await shoot(page, "technical-mode")
})
