import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
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

async function openMisfit(page: Page, mode: "fixture" | "unserved" | "refused" = "fixture") {
  await session(page)
  const actionCalls: { actionId: string; input: Record<string, unknown> }[] = []
  const episodeQueries: URL[] = []

  await page.route("**/api/host/v1/questions/learning.misfit_by_law/answer**", (route) => {
    if (mode === "unserved") return route.fulfill({ status: 404, body: "not found" })
    if (mode === "refused") return route.fulfill({ status: 403, json: { code: "authz.denied", message: "Your role may not read misfit metrics." } })
    return route.fulfill({ json: fixture.misfitByLaw })
  })

  await page.route("**/api/host/v1/questions/learning.triage_over_time/answer**", (route) => {
    if (mode === "unserved") return route.fulfill({ status: 404, body: "not found" })
    if (mode === "refused") return route.fulfill({ status: 403, json: { code: "authz.denied", message: "Your role may not read triage trends." } })
    return route.fulfill({ json: fixture.triageOverTime })
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
        total: { count: 1, exact: true },
        next: null,
        quarantineCount: 0,
      },
    })
  })

  await page.goto("/learning/misfit")
  return { actionCalls, episodeQueries }
}

test("misfit board & learning charts: renders both charts, exact tables, D53 suppression, deep-link to episodes, and draft proposal", async ({ page }) => {
  const { actionCalls, episodeQueries } = await openMisfit(page)

  // Both charts mount with SVG surfaces.
  await expect(page.locator('[aria-label="Misfit by law chart"] svg').first()).toBeVisible()
  await expect(page.locator('[aria-label="Triage verdicts over time chart"] svg').first()).toBeVisible()

  // Exact figures and D53 suppression in the board table.
  const misfitTable = page.getByRole("table", { name: "Misfit by law" })
  const creditRow = misfitTable.locator('tr[data-misfit-law="demo.credit.hold_over_limit"]')
  await expect(creditRow).toContainText("41.7%")
  await expect(creditRow).toContainText("(5/12)")
  await expect(creditRow).toContainText("25%")
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

  // Clicking "Episodes" on a law navigates to /learning/episodes?law=... and sends the server filter.
  await creditRow.getByRole("link", { name: "Episodes" }).click()
  await expect(page).toHaveURL(/\/learning\/episodes\?law=demo\.credit\.hold_over_limit/)
  await expect(page.getByText("law: demo.credit.hold_over_limit")).toBeVisible()
  expect(episodeQueries.some((u) => {
    const f = u.searchParams.get("filter")
    return f !== null && f.includes('"ruleOutcomes.lawId"') && f.includes('"demo.credit.hold_over_limit"')
  })).toBe(true)
})

test("misfit board: refused and unserved answers say so honestly", async ({ page }) => {
  await openMisfit(page, "refused")
  await expect(page.getByText("Your role may not read misfit metrics.")).toBeVisible()
  await expect(page.getByText("Your role may not read triage trends.")).toBeVisible()

  await page.unrouteAll({ behavior: "ignoreErrors" })
  await openMisfit(page, "unserved")
  await expect(page.getByText("This cell doesn't serve the learning.misfit_by_law question yet.")).toBeVisible()
  await expect(page.getByText("This cell doesn't serve the learning.triage_over_time question yet.")).toBeVisible()
})
