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
  let release: () => void = () => {}
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

  try {
    await page.clock.install()
    await page.goto("/metrics")

    // Skeleton renders immediately while request is pending.
    await expect(page.locator('[data-load-state="loading"]')).toBeVisible()
    await expect(page.getByRole("status", { name: "Reading metrics" })).toBeVisible()

    // Past SLOW_WAIT_MS (4 s): quiet slow hint appears beneath the skeleton.
    await page.clock.runFor(4_500)
    await expect(page.locator('[data-load-state="slow"]')).toBeVisible()
    await expect(page.getByText("Taking longer than usual to reach the workspace…")).toBeVisible()

    // Past BOUNDED_WAIT_MS / CELL_FETCH_TIMEOUT_MS (12 s): stalled fetch aborts cleanly into an honest timed-out state with Retry.
    await page.clock.runFor(8_000)
    const timedOut = page.locator("[data-workspace-unavailable]")
    await expect(timedOut).toBeVisible()
    await expect(timedOut).toContainText("The workspace took too long to respond")
    await expect(timedOut).toContainText("Last reachable")
    await shoot(page, "slow-timeout")

    // Resume the clock and click Retry: sends a fresh request that resolves immediately into the honest empty state.
    await page.clock.resume()
    await timedOut.getByRole("button", { name: "Retry" }).click()
    await expect(page.locator('[data-load-state="empty"]')).toBeVisible()
    await expect(page.getByText("No metrics yet")).toBeVisible()
  } finally {
    release()
  }
})

test("headers-first stalled body: the browser cancels the hung stream and Retry recovers", async ({ page }) => {
  await session(page)
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: definitions } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
  // Replace window.fetch for metrics to exercise browser stream cancellation and retry.
  await page.addInitScript(() => {
    const nativeFetch = window.fetch.bind(window)
    const state = { attempts: 0, cancelled: false }
    ;(window as unknown as { __stalledBody: typeof state }).__stalledBody = state
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url
      if (url.includes("/v1/questions:metrics")) {
        state.attempts += 1
        if (state.attempts === 1) {
          const stream = new ReadableStream<Uint8Array>({
            start(controller) {
              controller.enqueue(new TextEncoder().encode('{"metrics":['))
            },
            cancel() {
              state.cancelled = true
            },
          })
          return Promise.resolve(new Response(stream, { status: 200, headers: { "content-type": "application/json" } }))
        }
        return Promise.resolve(new Response(JSON.stringify({ metrics: [] }), { status: 200, headers: { "content-type": "application/json" } }))
      }
      return nativeFetch(input, init)
    }) as typeof fetch
  })

  await page.clock.install()
  await page.goto("/metrics")
  const stalledBodyState = () => page.evaluate(() => (window as unknown as { __stalledBody: { attempts: number; cancelled: boolean } }).__stalledBody)
  await expect(page.locator('[data-load-state="loading"]').getByRole("status", { name: "Reading metrics" })).toBeVisible()
  // The skeleton can appear before the scoped fetch starts and arms its deadline.
  await expect.poll(stalledBodyState).toEqual({ attempts: 1, cancelled: false })

  // Past 12 s while headers have already arrived and only the body is stalled: must abort and cancel the hung stream.
  await page.clock.runFor(4_500)
  await expect(page.locator('[data-load-state="slow"]')).toBeVisible()
  await page.clock.runFor(8_000)

  const timedOut = page.locator("[data-workspace-unavailable]")
  await expect(timedOut).toBeVisible()
  await expect(timedOut).toContainText("The workspace took too long to respond")
  expect(await stalledBodyState()).toEqual({ attempts: 1, cancelled: true })
  await shoot(page, "stalled-body-timeout")

  // Clicking Retry starts request #2 (never stuck on the hung attempt #1) and recovers.
  await page.clock.resume()
  await timedOut.getByRole("button", { name: "Retry" }).click()
  await expect(page.locator('[data-load-state="empty"]')).toBeVisible()
  expect((await stalledBodyState()).attempts).toBe(2)
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
            reason: null,
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
  const alert = page.locator('[data-load-state="error"]')
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
            reason: null,
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
  await page.clock.install()
  await page.goto("/")
  await expect(page.getByText("Nothing needs you")).toBeVisible()

  // Now the cell fails with non-retryable 503 cell.unopenable on reload/navigation.
  unopenable = true
  const hitsBefore = definitionHits
  await page.getByRole("link", { name: "Sources" }).click()
  await page.reload()

  // Must NEVER render a persistent "Retrying…" banner; shows honest Workspace unavailable immediately, once:
  // the page's own full state carries the server's words, the incident, Last reachable and Retry, so the
  // main island's one-line status stays out of its way (shell v2).
  const banner = page.locator('[data-connection="unavailable"]')
  const state = page.locator("[data-workspace-unavailable]")
  await expect(state).toBeVisible()
  await expect(state).toContainText("Workspace unavailable")
  await expect(state).toContainText("Storage migration is paused for this environment.")
  await expect(state).toContainText("inc-cell-909")
  await expect(state).toContainText("Last reachable")
  await expect(banner).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Retry", exact: true }).filter({ visible: true })).toHaveCount(1)
  await expect(page.getByText("inc-cell-909", { exact: true }).filter({ visible: true })).toHaveCount(1)
  await expect(page.locator('[data-connection="retrying"]')).toHaveCount(0)
  await expect(page.getByText("Retrying…")).toHaveCount(0)
  await expect(state.getByRole("button", { name: "Retry" })).toBeEnabled()
  // Nothing can be connected to a workspace that can't be opened: the page doesn't offer it (the mode's answer).
  await expect(page.getByRole("link", { name: "Connect a source" })).toHaveCount(0)
  await page.clock.resume()
  await shoot(page, "cell-unavailable")
  await page.clock.install()

  // Advance the fake clock across the full automatic-retry backoff window (10 s > 5.6 s) to confirm zero retries fire for retryable: false.
  const hitsAfterFail = definitionHits
  await page.clock.runFor(10_000)
  expect(definitionHits).toBe(hitsAfterFail)
  expect(hitsAfterFail).toBeGreaterThan(hitsBefore)

  // Clicking Retry once the cell recovers restores both the live workspace and the Sources page.
  await page.clock.resume()
  unopenable = false
  await state.getByRole("button", { name: "Retry" }).click()
  await expect(banner).toHaveCount(0)
  await expect(page.locator("[data-workspace-unavailable]")).toHaveCount(0)
  await expect(page.getByRole("link", { name: "Connect a source" })).toBeVisible()
})

test("transient HTML 502 on /v1/definitions: retries cleanly through the bounded backoff window and settles to Workspace unavailable without falling back to recorded", async ({ page }) => {
  await session(page)
  let gatewayDown = false
  let definitionHits = 0
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) {
      definitionHits += 1
      if (gatewayDown) {
        return route.fulfill({ status: 502, contentType: "text/html", body: "<html><body>502 Bad Gateway</body></html>" })
      }
      return route.fulfill({ json: { artifacts: definitions } })
    }
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: { scanLimit: 1000, sections: [], items: [] } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })

  await page.clock.install()
  await page.goto("/")
  await expect(page.getByText("Nothing needs you")).toBeVisible()

  gatewayDown = true
  const before = definitionHits
  await page.reload()

  // First attempt fails with HTML 502: Studio enters bounded retrying (never falling back to recorded).
  await expect(page.locator('[data-connection="retrying"]')).toBeVisible()
  await expect(page.getByText("Recorded data")).toHaveCount(0)

  // Drive through the 800 ms + 1600 ms + 3200 ms mode-retry delays (3 automatic retries after the initial attempt).
  await page.clock.runFor(900)
  await page.clock.runFor(1_700)
  await page.clock.runFor(3_300)

  // Workspace unavailable shows once: Home has its own full state, so the one-line status stays away.
  const banner = page.locator('[data-connection="unavailable"]')
  const state = page.locator("[data-workspace-unavailable]")
  await expect(state).toBeVisible()
  await expect(state).toContainText("Workspace unavailable")
  await expect(state).toHaveCount(1)
  await expect(banner).toHaveCount(0)
  // Let any parallel query retries (e.g. catalog's 1 s + 2 s + 4 s backoff) finish settling, then confirm retries stop completely.
  await page.clock.runFor(5_000)
  const settledHits = definitionHits - before
  expect(settledHits).toBeGreaterThanOrEqual(4)
  expect(settledHits).toBeLessThanOrEqual(8)

  // No further retries fire once settled across a full 15 s quiet window.
  await page.clock.runFor(15_000)
  expect(definitionHits - before).toBe(settledHits)

  // Explicit Retry recovers once the gateway is back.
  await page.clock.resume()
  gatewayDown = false
  await page.getByRole("button", { name: "Retry", exact: true }).filter({ visible: true }).click()
  await expect(banner).toHaveCount(0)
  await expect(page.getByText("Nothing needs you")).toBeVisible()
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
