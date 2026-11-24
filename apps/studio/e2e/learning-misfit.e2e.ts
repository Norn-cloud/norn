import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { advertiseLearning } from "./learning-cell.ts"
import { session } from "./object-cell.ts"

// Misfit board and learning charts (#1220 L-4): learning.misfit_by_law and
// learning.triage_over_time through the generic question route, plus deep links to filtered
// episodes and server-offered gap-proposal drafting.

const fixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-misfit.json", import.meta.url), "utf8")) as {
  readonly misfitByLaw: object
  readonly triageOverTime: object
}
const episodesFixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-episode.json", import.meta.url), "utf8")) as {
  readonly records: readonly object[]
}
const served = JSON.parse(readFileSync(new URL("../test/fixtures/action-call.json", import.meta.url), "utf8")) as {
  readonly committed: { readonly receipt: object }
}

const DRAFT_PROPOSAL = {
  actionId: "learning.draft_gap_proposal",
  title: "Draft gap proposal",
  description: "Draft a definition diff citing episodes for review and replay.",
  objectKind: "learning.GapProposal",
  effect: "create_object",
  objectIdField: "proposalId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["target", "title", "summary"],
    properties: {
      target: { type: "string", title: "Target law" },
      title: { type: "string", title: "Proposal title" },
      summary: { type: "string", title: "Summary" },
    },
  },
}

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/learning-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => {
  test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the learning misfit/triage question stand-in`)
})

interface MisfitOpenOptions {
  readonly mode?: "fixture" | "unserved" | "refused"
  readonly misfitByLaw?: object
  readonly triageOverTime?: object
}

async function openMisfit(page: Page, options: MisfitOpenOptions = {}) {
  const mode = options.mode ?? "fixture"
  await session(page)
  await advertiseLearning(page, {
    objectTypes: ["learning.Episode", "learning.GapProposal"],
    publishedQuestions: ["learning.misfit_by_law", "learning.triage_over_time"],
  })
  const actionCalls: { actionId: string; input: Record<string, unknown> }[] = []
  const episodeQueries: URL[] = []

  await page.route("**/api/host/v1/questions/learning.misfit_by_law/answer**", (route) => {
    if (mode === "unserved") return route.fulfill({ status: 404, body: "not found" })
    if (mode === "refused") return route.fulfill({ status: 403, json: { code: "authz.denied", message: "Your role may not read misfit metrics." } })
    return route.fulfill({ json: options.misfitByLaw ?? fixture.misfitByLaw })
  })

  await page.route("**/api/host/v1/questions/learning.triage_over_time/answer**", (route) => {
    if (mode === "unserved") return route.fulfill({ status: 404, body: "not found" })
    if (mode === "refused") return route.fulfill({ status: 403, json: { code: "authz.denied", message: "Your role may not read triage trends." } })
    return route.fulfill({ json: options.triageOverTime ?? fixture.triageOverTime })
  })

  await page.route("**/api/host/v1/actions**", (route) => {
    if (mode !== "fixture") return route.fulfill({ status: 404, body: "not found" })
    const request = route.request()
    if (request.method() === "GET") return route.fulfill({ json: { items: [DRAFT_PROPOSAL] } })
    const actionId = decodeURIComponent(new URL(request.url()).pathname.split("/v1/actions/")[1] ?? "")
    const { input } = JSON.parse(request.postData() ?? "{}") as { input: Record<string, unknown> }
    actionCalls.push({ actionId, input })
    return route.fulfill({
      json: { ...served.committed, objectType: "learning.GapProposal", objectId: "prop_from_misfit", receipt: { ...served.committed.receipt, commitSeq: 8001, recordedAtMs: Date.now() } },
    })
  })

  await page.route("**/api/host/v1/objects/learning.Episode**", (route) => {
    const url = new URL(route.request().url())
    episodeQueries.push(url)
    const rawFilter = url.searchParams.get("filter")
    const filter = rawFilter ? JSON.parse(rawFilter) : null
    return route.fulfill({
      json: {
        objectType: "learning.Episode",
        records: [episodesFixture.records[0]],
        query: { sort: [], filter },
        total: { count: 1, approximate: false },
        next: null,
        quarantineCount: 0,
      },
    })
  })

  await page.goto("/learning/misfit")
  return { actionCalls, episodeQueries }
}

test("misfit board & learning charts: renders both charts, exact tables, D53 suppression, link to episodes, and draft proposal", async ({ page }) => {
  const { actionCalls, episodeQueries } = await openMisfit(page)

  // Both charts mount with SVG surfaces.
  await expect(page.locator('[aria-label="Misfit by law chart"] svg').first()).toBeVisible()
  await expect(page.locator('[aria-label="Triage verdicts over time chart"] svg').first()).toBeVisible()

  // Exact figures and D53 suppression in the board table.
  const misfitTable = page.getByRole("table", { name: "Misfit by law" })
  const creditRow = misfitTable.locator('tr[data-misfit-law="demo.credit.hold_over_limit"]')
  await expect(creditRow).toContainText("41.70%")
  await expect(creditRow).toContainText("(5/12)")
  await expect(creditRow).toContainText("25.00%")
  await expect(creditRow).toContainText("(3/12)")

  const suppressedRow = misfitTable.locator('tr[data-misfit-law="demo.refund.window_days"]')
  await expect(suppressedRow.getByText("Suppressed")).toHaveCount(2)

  // Weekly triage table totals.
  const triageTable = page.getByRole("table", { name: "Triage verdicts per week" })
  await expect(triageTable.locator('tr[data-triage-week="2026-11-02"]')).toContainText("12")
  await expect(triageTable.locator('tr[data-triage-week="2026-11-16"]')).toContainText("14")

  await shoot(page, "misfit")

  // Drafting a gap proposal prefills the law target and commits through the cell.
  await creditRow.getByRole("button", { name: "Draft gap proposal" }).click()
  const sheet = page.getByRole("dialog")
  await expect(sheet.getByLabel("Target law")).toHaveValue("demo.credit.hold_over_limit")
  await sheet.getByLabel("Proposal title").fill("Add phone-confirmed remittance exception")
  await sheet.getByLabel("Summary").fill("Reduce 41.7% override rate on credit holds.")
  await sheet.getByRole("button", { name: "Draft gap proposal", exact: true }).click()
  await expect(sheet.getByRole("status")).toContainText("prop_from_misfit")
  await sheet.getByRole("button", { name: "Done" }).click()
  expect(actionCalls).toEqual([{
    actionId: "learning.draft_gap_proposal",
    input: {
      target: "demo.credit.hold_over_limit",
      title: "Add phone-confirmed remittance exception",
      summary: "Reduce 41.7% override rate on credit holds.",
    },
  }])

  // Clicking "Episodes" navigates to /learning/episodes without sending a dotted law filter (S2).
  await creditRow.getByRole("link", { name: "Episodes" }).click()
  await expect(page).toHaveURL(/\/learning\/episodes$/)
  await expect.poll(() => episodeQueries.length).toBeGreaterThan(0)
  expect(episodeQueries.every((u) => !(u.searchParams.get("filter") ?? "").includes("."))).toBe(true)
})

test("misfit board (S5 & S6): ranks close rates by exact decimal, marks omitted triage buckets Unknown, and fails closed on duplicate buckets", async ({ page }) => {
  const closeMisfit = structuredClone(fixture.misfitByLaw) as {
    rows: Record<string, unknown>
  }
  closeMisfit.rows = {
    '["demo.law.low","override"]': {
      bucket: ["demo.law.low", "override"],
      totals: [41701, 100000],
      value: 0.41701,
      members: { ep_1: { version: 1, amounts: [1, 1] } },
    },
    '["demo.law.high","override"]': {
      bucket: ["demo.law.high", "override"],
      totals: [41704, 100000],
      value: 0.41704,
      members: { ep_2: { version: 1, amounts: [1, 1] } },
    },
  }
  const partialTriage = structuredClone(fixture.triageOverTime) as {
    rows: Record<string, unknown>
  }
  // Omit the `noise` bucket in week 2026-11-02 — Studio must render Unknown, never 0.
  delete partialTriage.rows['["2026-11-02","noise"]']

  await openMisfit(page, { misfitByLaw: closeMisfit, triageOverTime: partialTriage })

  const misfitRows = page.getByRole("table", { name: "Misfit by law" }).locator("tbody tr")
  await expect(misfitRows.nth(0)).toHaveAttribute("data-misfit-law", "demo.law.high")
  await expect(misfitRows.nth(1)).toHaveAttribute("data-misfit-law", "demo.law.low")
  const highestOverrideKpi = page.locator('[data-slot="kpi"]', { hasText: "Highest override rate" })
  await expect(highestOverrideKpi).toContainText("41.70%")
  await expect(highestOverrideKpi).toContainText("High")

  const triageRow = page.getByRole("table", { name: "Triage verdicts per week" }).locator('tr[data-triage-week="2026-11-02"]')
  await expect(triageRow.locator('[data-missing-bucket="true"]')).toHaveText("Unknown")
  await expect(triageRow).toContainText("Unknown")
  await expect(page.locator('[data-slot="kpi"]', { hasText: "Triaged in window" })).toContainText("Unknown")

  // Duplicate [weekStart, verdict] bucket fails closed instead of overwriting silently (S6).
  const dupTriage = structuredClone(fixture.triageOverTime) as { rows: Record<string, unknown> }
  dupTriage.rows["dup_bucket"] = {
    bucket: ["2026-11-02", "exception"],
    totals: [9],
    value: 9,
    members: { ep_99: { version: 1, amounts: [1] } },
  }
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await openMisfit(page, { triageOverTime: dupTriage })
  await expect(page.getByText("Can't read triage trend")).toBeVisible()

  // S5: Total ordering across mixed rows with and without totals at equal decimal 0.333333 (z 1/3 > a 333333/1000000 > m no totals).
  const mixedTotals = structuredClone(fixture.misfitByLaw) as { rows: Record<string, unknown> }
  mixedTotals.rows = {
    '["demo.law.m","override"]': { bucket: ["demo.law.m", "override"], totals: [], value: 0.333333, members: {} },
    '["demo.law.a","override"]': { bucket: ["demo.law.a", "override"], totals: [333333, 1000000], value: 0.333333, members: {} },
    '["demo.law.z","override"]': { bucket: ["demo.law.z", "override"], totals: [1, 3], value: 0.333333, members: {} },
  }
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await openMisfit(page, { misfitByLaw: mixedTotals })
  const mixedRows = page.getByRole("table", { name: "Misfit by law" }).locator("tbody tr")
  await expect(mixedRows.nth(0)).toHaveAttribute("data-misfit-law", "demo.law.z")
  await expect(mixedRows.nth(1)).toHaveAttribute("data-misfit-law", "demo.law.a")
  await expect(mixedRows.nth(2)).toHaveAttribute("data-misfit-law", "demo.law.m")
  await expect(page.locator('[data-slot="kpi"]', { hasText: "Highest override rate" })).toContainText("Z")

  // S6: Fractional memberCount on a rate with denominator and extra triage totals ([0, 8] with value 7) both fail closed.
  const badMemberCount = structuredClone(fixture.misfitByLaw) as { rows: Record<string, unknown> }
  badMemberCount.rows = {
    '["demo.law.bad","override"]': { bucket: ["demo.law.bad", "override"], totals: [5, 12], value: 0.417, memberCount: 1.5, members: {} },
  }
  const extraTotalsTriage = structuredClone(fixture.triageOverTime) as { rows: Record<string, unknown> }
  extraTotalsTriage.rows = {
    '["2026-11-02","gap"]': { bucket: ["2026-11-02", "gap"], totals: [0, 8], value: 7, members: {} },
  }
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await openMisfit(page, { misfitByLaw: badMemberCount, triageOverTime: extraTotalsTriage })
  await expect(page.getByText("Can't read misfit answer")).toBeVisible()
  await expect(page.getByText("Can't read triage trend")).toBeVisible()
})

test("misfit board: refused and unserved answers say so honestly", async ({ page }) => {
  await openMisfit(page, { mode: "refused" })
  await expect(page.getByText("Your role may not read misfit metrics.").first()).toBeVisible()
  await expect(page.getByText("Your role may not read triage trends.").first()).toBeVisible()

  await page.unrouteAll({ behavior: "ignoreErrors" })
  await openMisfit(page, { mode: "unserved" })
  await expect(page.getByText("This cell doesn't serve the learning.misfit_by_law question yet.")).toBeVisible()
  await expect(page.getByText("This cell doesn't serve the learning.triage_over_time question yet.")).toBeVisible()
})
