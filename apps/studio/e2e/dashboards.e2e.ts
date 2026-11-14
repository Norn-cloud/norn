import { readFileSync } from "node:fs"
import { expect, test } from "@playwright/test"
import type { Page, Route } from "@playwright/test"

/**
 * The dashboard row of the verb matrix, against stand-ins for the Worker's
 * routes: ask the assistant, read the checker's verdict and words, see the
 * live preview, accept, open the board (each tile drawn as its view), edit a
 * view, delete and undo, and a refused proposal that says why.
 */
const fixture = (name: string) => JSON.parse(readFileSync(new URL(`../../../fixtures/demo/answers/${name}`, import.meta.url), "utf8"))
const weekly = fixture("scrap-yield.k1.answer.json") as { rows: Record<string, { bucket: unknown; totals: number[]; value: number; members: Record<string, unknown> }> }
const objectType = fixture("scrap-yield.object-type.json")
const metrics = fixture("scrap-yield.metrics.json")
const SHOTS = process.env.SHOTS ?? "test-results/shots"
/** The cell's own guard documents for one assistant prompt (crates/norn-api/tests/privacy.rs writes them). */
const privacy = (name: string) => JSON.parse(readFileSync(new URL(`../../../fixtures/demo/privacy/${name}`, import.meta.url), "utf8"))

const catalog = {
  objectSet: "demo.Conversion",
  properties: objectType.properties.map(({ id, type }: { id: string; type: { kind: string; unit?: { code: string } } }) => ({ name: id, kind: type.kind, unit: type.unit?.code ?? null })),
  metrics: metrics.map(({ name, label, type }: { name: string; label: string; type: string }) => ({ name, label, type })),
  grains: ["week"], operators: ["eq", "ne"], example: { irVersion: "0.1", calendar: "iso_utc", freshness: { kind: "live", maxAgeMs: 5000 } },
  declarations: { metrics, objectType }, maxGroupBy: 1,
}
const rows = Object.values(weekly.rows)
const sum = (pick: (row: (typeof rows)[number]) => number) => Number(rows.reduce((total, row) => total + pick(row), 0).toFixed(3))
/** One answer per question shape, all from the recorded weekly answer: in total, by metal, or weekly. */
function answerFor(question: { aggregate: { metric: string }[]; groupBy?: { path: { property: string } }[] }) {
  const ratio = question.aggregate[0]?.metric === "demo.yield"
  const [by] = question.groupBy ?? []
  if (by === undefined) {
    const totals = [sum((row) => row.totals[0] ?? 0), sum((row) => row.totals[1] ?? 0)]
    const row = { bucket: null, totals: ratio ? totals : [totals[0]], value: ratio ? Number(((totals[0] ?? 0) / (totals[1] ?? 1)).toFixed(6)) : totals[0], members: Object.assign({}, ...rows.map((r) => r.members)) }
    return { ...weekly, rows: { null: row } }
  }
  if (by.path.property === "metal") {
    const parts = [["gold", 0.62], ["silver", 0.28], ["copper", 0.1]] as const
    const total = sum((row) => row.totals[0] ?? 0)
    return { ...weekly, rows: Object.fromEntries(parts.map(([metal, share]) => [JSON.stringify(metal), { bucket: metal, totals: [Number((total * share).toFixed(3))], value: Number((total * share).toFixed(3)), members: {} }])) }
  }
  return weekly
}

const window = { start: "2026-07-06T00:00:00Z", end: "2026-10-26T00:00:00Z" }
const question = (name: string, metric: string, groupBy: unknown[] = []) => ({ irVersion: "0.1", name, objectSet: "demo.Conversion", aggregate: [{ metric }], groupBy, filter: [], window, calendar: "iso_utc", freshness: { kind: "live", maxAgeMs: 5000 } })
const proposed = {
  title: "Refinery overview",
  tiles: [
    { id: "t1", title: "Yield in total", question: question("studio.yield_in_total", "demo.yield"), view: { kind: "number" }, layout: { x: 0, y: 0, w: 1, h: 1 } },
    { id: "t2", title: "Recovered by metal", question: question("studio.recovered_by_metal", "demo.recovered", [{ path: { property: "metal" } }]), view: { kind: "chart", chart: "pie" }, layout: { x: 1, y: 0, w: 1, h: 1 } },
    { id: "t3", title: "Yield, weekly", question: question("studio.yield_weekly", "demo.yield", [{ path: { property: "occurred_at" }, grain: "week" }]), view: { kind: "chart", chart: "line" }, layout: { x: 0, y: 1, w: 2, h: 1 } },
  ],
  origin: { by: "assistant", proposal: "prop_1", model: "claude-sonnet-5-5", request: "" },
}
const words = "Refinery overview\nProposed by the assistant (claude-sonnet-5-5) for the request: \"%\"\n1. Yield in total: Yield (recovered ÷ input) of demo Conversion records, in total, from 6 Jul to 26 Oct 2026 (26 Oct not included), live data at most 5 seconds old\n2. Recovered by metal: Recovered (sum of recovered) of demo Conversion records, by metal, ...\n3. Yield, weekly: Yield (recovered ÷ input) of demo Conversion records, by week of occurred at, ..."

async function stage(page: Page) {
  await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u_e2e", name: "Analyst", email: "analyst@example.com" }, organization: { id: "org_demo", name: "Demo", role: "member" }, environmentId: "analytics" } }))
  await page.route("**/api/questions/catalog", (route) => route.fulfill({ json: catalog }))
  await page.route("**/api/host/v1/questions:evaluate**", (route) => route.fulfill({ json: answerFor((route.request().postDataJSON() as { question: Parameters<typeof answerFor>[0] }).question) }))
  const saved = new Map<string, { version: number; payload: unknown; retired: boolean }>()
  const proposals: Record<string, unknown>[] = []
  await page.route("**/api/agent/dashboards**", async (route: Route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.slice("/api/agent/dashboards".length)
    if (path === "" && route.request().method() === "GET") return route.fulfill({ json: { proposals: [...proposals].reverse(), spentMicroUsd: 21_000, capMicroUsd: 20_000_000, model: "claude-sonnet-5-5" } })
    if (path === "" && route.request().method() === "POST") {
      const { request } = route.request().postDataJSON() as { request: string }
      const refused = /pie of yield/i.test(request)
      const definition = refused
        ? { ...proposed, title: "Yield by metal", tiles: [{ ...proposed.tiles[1], title: "Yield by metal", question: question("studio.yield_by_metal", "demo.yield", [{ path: { property: "metal" } }]) }], origin: { ...proposed.origin, request } }
        : { ...proposed, origin: { ...proposed.origin, request } }
      // PRIVACY-GUARD-1: the Worker sent the model only the cell's safe prompt; the reply came back restored.
      const guarded = /Amira/.test(request) ? { guard: { ...privacy("guard.prompt.json"), reply: privacy("guard.reply.auditor.json").segments, model: "claude-sonnet-5-5" } } : {}
      const proposal = {
        id: `prop_${proposals.length + 1}`, request, targetId: "studio.dashboard.refinery_overview_e2e", targetVersion: null, definition, model: "claude-sonnet-5-5", installed: null, costMicroUsd: 14_000, createdAtMs: Date.now(),
        state: refused ? "refused" : "pending",
        verdict: refused
          ? { admitted: false, findings: [{ code: "definition.view_invalid", message: "a pie shows the parts of one whole, so its question sums one additive, non-ratio measure over one category without a time grain", pointer: "/tiles/0/view" }], restatement: null, restatementSha256: null, note: "Yield is a ratio, so it cannot be shown as parts of a whole." }
          : { admitted: true, findings: [], restatement: words.replace("%", request), restatementSha256: "c".repeat(64), note: "A headline yield, how recovered metal splits by metal, and the weekly yield trend.", ...guarded },
      }
      proposals.push(proposal)
      return route.fulfill({ json: proposal })
    }
    const [, id, verb] = /^\/(prop_\d+)\/(accept|decline)$/.exec(path) ?? []
    const proposal = proposals.find((candidate) => candidate.id === id)
    if (!proposal) return route.fulfill({ status: 404, json: { error: "No such proposal." } })
    if (verb === "accept") {
      saved.set(String(proposal.targetId), { version: 1, payload: proposal.definition, retired: false })
      Object.assign(proposal, { state: "accepted", installed: { id: proposal.targetId, version: 1 } })
    } else Object.assign(proposal, { state: "declined" })
    return route.fulfill({ json: proposal })
  })
  // "Show original" is a restore of the safe prompt by the cell: a logged reveal for each field.
  await page.route("**/api/host/v1/guard/**", (route: Route) => route.fulfill({ json: privacy("guard.original.auditor.json") }))
  await page.route("**/api/host/v1/definitions**", async (route: Route) => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith(":list")) return route.fulfill({ json: { items: [...saved].filter(([, entry]) => !entry.retired).map(([id, entry]) => ({ id, title: (entry.payload as { title: string }).title, version: entry.version, lifecycle: "active" })), cursor: null } })
    if (url.pathname.endsWith(":admit")) return route.fulfill({ json: { admitted: true, diagnostics: [], restatement: null, restatementSha256: null } })
    if (url.pathname.endsWith(":install")) {
      const body = route.request().postDataJSON() as { id: string; payload: unknown; expectedVersion?: number }
      const current = saved.get(body.id)
      if (current && body.expectedVersion !== current.version) return route.fulfill({ status: 409, json: { code: "definition.version_conflict" } })
      const version = (current?.version ?? 0) + 1
      saved.set(body.id, { version, payload: body.payload, retired: false })
      return route.fulfill({ json: { installed: true, id: body.id, version, receipt: { receiptHash: "r", commitSeq: version, recordedAtMs: 1 } } })
    }
    const retire = /\/v1\/definitions\/([^/:]+):retire$/.exec(url.pathname)
    if (retire) {
      const entry = saved.get(decodeURIComponent(retire[1] ?? ""))!
      entry.version += 1; entry.retired = true
      return route.fulfill({ json: { installed: true, id: retire[1], version: entry.version, receipt: { receiptHash: "r", commitSeq: entry.version, recordedAtMs: 1 } } })
    }
    const read = /\/v1\/definitions\/([^/:]+)$/.exec(url.pathname)
    const entry = read && saved.get(decodeURIComponent(read[1] ?? ""))
    if (entry && !entry.retired) return route.fulfill({ json: { id: read?.[1], kind: "dashboard", version: entry.version, payload: entry.payload } })
    return route.fulfill({ status: 404, json: { code: "definition.not_found" } })
  })
}

test("dashboard row: ask, read the checker's verdict, accept, open the board, edit a view, delete and undo; a refused proposal says why", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "this row uses deterministic stand-ins for the assistant and the cell")
  page.on("dialog", (dialog) => void dialog.accept())
  await stage(page)
  await page.goto("/dashboards")
  const ask = page.getByRole("textbox", { name: "Ask for a dashboard" })
  await expect(ask).toBeVisible()
  await page.getByRole("button", { name: /^A weekly overview of/ }).click()
  await expect(ask).toHaveValue(/A weekly overview of Input/)
  await page.getByRole("button", { name: "Propose" }).click()
  const card = page.locator("[data-proposal=prop_1]")
  await expect(card.getByText("The cell's checker admitted this dashboard")).toBeVisible()
  await expect(card.locator("[data-restatement]")).toContainText("Proposed by the assistant (claude-sonnet-5-5) for the request")
  await expect(card.locator("[data-view=number]")).toBeVisible()
  await expect(card.locator("[data-view=pie]")).toBeVisible()
  await expect(card.locator("[data-view=line]")).toBeVisible()
  await page.screenshot({ path: `${SHOTS}/1-proposal.png`, fullPage: true })

  await card.getByRole("button", { name: "Accept and save" }).click()
  await expect(page).toHaveURL(/\/dashboards\/studio\.dashboard\.refinery_overview_e2e$/)
  await expect(page.getByRole("heading", { name: "Refinery overview" })).toBeVisible()
  await expect(page.getByText("Proposed by the assistant", { exact: true })).toBeVisible()
  await expect(page.locator("[data-view=number]")).toContainText("%")
  await page.screenshot({ path: `${SHOTS}/2-board.png`, fullPage: true })

  // Edit: the weekly tile becomes bars; the save is a new version authored in Studio.
  await page.getByRole("link", { name: "Edit" }).click()
  await page.getByLabel("Tile 3 view").selectOption("chart:bar")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByText("Version 2")).toBeVisible()
  await page.getByRole("link", { name: "View" }).click()
  await expect(page.locator("[data-view=bar]")).toBeVisible()

  // Delete, then undo: the same definition is installed over the retired version.
  await page.getByRole("button", { name: "Delete" }).click()
  await expect(page.getByText(/Deleted “Refinery overview”/)).toBeVisible()
  await page.getByRole("button", { name: "Undo" }).click()
  await expect(page.getByRole("heading", { name: "Refinery overview" })).toBeVisible()
  await expect(page.getByText("Version 4")).toBeVisible()

  // A request the checker refuses: nothing is saved, and the finding is shown in its words.
  await page.goto("/dashboards")
  await ask.fill("A pie of yield by metal")
  await page.getByRole("button", { name: "Propose" }).click()
  const refused = page.locator("[data-proposal=prop_2]")
  await expect(refused.getByText("The cell's checker refused this proposal")).toBeVisible()
  await expect(refused.getByText(/a pie shows the parts of one whole/)).toBeVisible()
  await expect(refused.getByRole("button", { name: "Accept and save" })).toHaveCount(0)
  await page.screenshot({ path: `${SHOTS}/3-refused.png`, fullPage: true })
})

test("the assistant's model reads a safe prompt: typed placeholder chips, the guard's path, a restored reply, and Show original as a logged reveal", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "this row uses deterministic stand-ins for the assistant and the cell")
  await stage(page)
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto("/dashboards")
  const ask = page.getByRole("textbox", { name: "Ask for a dashboard" })
  await ask.fill("Show weekly refunds for Amira Haddad: she was charged twice on card 4539 1488 0343 6467. Reach her at amira.haddad@fastmail.com or +20 100 555 0132.")
  await page.getByRole("button", { name: "Propose" }).click()
  const safe = page.locator("[data-safe-prompt]")
  await expect(safe).toBeVisible()
  const prompt = safe.locator("[data-prompt]")
  for (const token of ["[CUSTOMER_1]", "[CARD_1]", "[EMAIL_1]", "[PHONE_1]"]) await expect(prompt.locator(`[data-token="${token}"]`)).toBeVisible()
  await expect(prompt).not.toContainText("Amira")
  await expect(prompt).not.toContainText("4539")
  await expect(safe.getByText("Privacy guard")).toBeVisible()
  await expect(safe.locator("[data-reply-state]")).toHaveText("Restored")
  await expect(safe.locator("[data-reply]")).toContainText("Amira Haddad")
  await safe.scrollIntoViewIfNeeded()
  await safe.screenshot({ path: `${SHOTS}/privacy-guard-safe-prompt.png` })
  await page.emulateMedia({ colorScheme: "dark" })
  await page.waitForTimeout(400)
  await safe.screenshot({ path: `${SHOTS}/privacy-guard-safe-prompt-dark.png` })
  await page.emulateMedia({ colorScheme: "light" })
  await safe.locator("[data-show-original]").click()
  await expect(prompt).toContainText("Amira Haddad")
  await expect(prompt).toContainText("4539 1488 0343 6467")
  await page.waitForTimeout(300)
  await safe.screenshot({ path: `${SHOTS}/privacy-guard-original.png` })
})

test("the analytics engine answers one call at a time: Studio never sends a second while one is out, and a 'pending' refusal is asked again, not shown", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "this row uses deterministic stand-ins for the assistant and the cell")
  await stage(page)
  // The cell's one DuckDB slot: a call that overlaps another is refused as pending (500), and so is the very first one.
  let inFlight = 0
  let most = 0
  let first = true
  let refusedPending = 0
  await page.route("**/api/host/v1/questions:evaluate**", async (route) => {
    inFlight += 1
    most = Math.max(most, inFlight)
    if (inFlight > 1 || first) {
      first = false
      inFlight -= 1
      refusedPending += 1
      return route.fulfill({ status: 500, json: { format: "norn/refusal@2", code: "sql.engine_busy", message: "sql engine: an analytics engine call is pending" } })
    }
    await new Promise((resolve) => setTimeout(resolve, 150))
    inFlight -= 1
    return route.fulfill({ json: answerFor((route.request().postDataJSON() as { question: Parameters<typeof answerFor>[0] }).question) })
  })
  await page.goto("/dashboards")
  await page.getByRole("button", { name: /^A weekly overview of/ }).click()
  await page.getByRole("button", { name: "Propose" }).click()
  const card = page.locator("[data-proposal=prop_1]")
  await expect(card.locator("[data-view=number]")).toBeVisible({ timeout: 20_000 })
  await expect(card.locator("[data-view=pie]")).toBeVisible({ timeout: 20_000 })
  await expect(card.locator("[data-view=line]")).toBeVisible({ timeout: 20_000 })
  await expect(card).not.toContainText("pending")
  await expect(card).not.toContainText("did not answer")
  expect(most).toBe(1)
  expect(refusedPending).toBe(1)
})
