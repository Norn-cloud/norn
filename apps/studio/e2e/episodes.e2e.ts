import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { decisionCell } from "./decision-cell.ts"
import { advertiseLearning } from "./learning-cell.ts"
import { session } from "./object-cell.ts"

/** Choose a view (Business, Technical) or a language (English, العربية) from the account menu, where they live. */
async function chooseFromAccountMenu(page: Page, name: string) {
  await page.getByRole("button", { name: /Account$/ }).click()
  await page.getByRole("menuitemradio", { name, exact: true }).click()
}


// One test per state of the episode timeline (#1220 L-1). The object route is stubbed with the
// contract-checked fixture (test/learning-episode.test.ts checks it against the served envelope and
// the decoder), so these skip on a stage until the learning pack serves learning.Episode.
// SHOTS=<dir> saves every state in light and dark for review.

const fixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-episode.json", import.meta.url), "utf8")) as { readonly records: readonly { readonly id: string; readonly values: Record<string, unknown> }[] }
/** The served receipt and refusal documents the action route's own test writes (#1203, #1215). */
const served = JSON.parse(readFileSync(new URL("../test/fixtures/action-call.json", import.meta.url), "utf8")) as { readonly committed: { readonly receipt: object }; readonly conflict: object }
/** The served run and decision envelopes from #1630. */
const servedDecisionRun = JSON.parse(readFileSync(new URL("../test/fixtures/decision-run.json", import.meta.url), "utf8")) as typeof import("../test/fixtures/decision-run.json")

/**
 * The learning pack's triage actions as the catalog would list them for learning.Episode: the names
 * and inputs requested on #1208, in the served ActionSummary shape. They go when the pack serves its
 * own catalog. A triage receipt offers its undo (the action's compensation), as requested there too.
 */
const TRIAGE = {
  actionId: "learning.triage_episode", title: "Triage", description: "Say what this decision teaches: a one-off, noise, a reusable pattern or a gap in the model.",
  objectKind: "learning.Episode", effect: "fire_transition", objectIdField: "episodeId", uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object", additionalProperties: false, required: ["episodeId", "verdict", "reason"],
    properties: {
      episodeId: { type: "string", title: "Episode" },
      verdict: { type: "string", title: "Verdict", enum: ["exception", "noise", "pattern", "gap"] },
      reason: { type: "string", title: "Reason" },
      rememberAsContext: { type: "boolean", title: "Remember this for next time" },
    },
  },
}
const RETRACT = {
  actionId: "learning.retract_triage", title: "Retract triage", description: "Withdraw the current verdict; the episode reads as not triaged.",
  objectKind: "learning.Episode", effect: "fire_transition", objectIdField: "episodeId", uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: { type: "object", additionalProperties: false, required: ["episodeId"], properties: { episodeId: { type: "string", title: "Episode" } } },
}
const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/episode-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the episode route stand-in`) })

type Serve = "fixture" | "unserved" | "malformed"
interface Actions { readonly serve: "catalog" | "unserved"; readonly refuseNext?: boolean }
async function open(
  page: Page,
  id: string,
  serve: Serve = "fixture",
  actions: Actions = { serve: "unserved" },
  extraArtifacts?: readonly object[],
) {
  await session(page)
  await advertiseLearning(page, { objectTypes: ["learning.Episode"], ...(extraArtifacts ? { extraArtifacts } : {}) })
  const records = structuredClone(fixture.records) as { id: string; values: Record<string, unknown> }[]
  const calls: { actionId: string; input: Record<string, unknown> }[] = []
  let refuse = actions.refuseNext === true
  let seq = 6000
  await page.route("**/api/host/v1/actions**", (route) => {
    if (actions.serve === "unserved") return route.fulfill({ status: 404, body: "not found" })
    const request = route.request()
    if (request.method() === "GET") return route.fulfill({ json: { definitionsVersion: 1, items: [TRIAGE, RETRACT] } })
    const actionId = decodeURIComponent(new URL(request.url()).pathname.split("/v1/actions/")[1] ?? "")
    const { input } = JSON.parse(request.postData() ?? "{}") as { input: Record<string, unknown> }
    calls.push({ actionId, input })
    if (refuse) { refuse = false; return route.fulfill({ status: 409, json: served.conflict }) }
    const record = records.find((r) => r.id === input["episodeId"])!
    record.values["triage"] = actionId === TRIAGE.actionId
      ? { verdict: input["verdict"], reason: input["reason"], curator: "you", atMs: 1794600000000 + seq }
      : null
    seq += 1
    return route.fulfill({
      json: {
        ...served.committed, objectType: "learning.Episode", objectId: record.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() },
        ...actionId === TRIAGE.actionId ? { undo: { actionId: RETRACT.actionId, input: { episodeId: record.id } } } : {},
      },
    })
  })
  await page.route("**/api/host/v1/objects/learning.Episode/**", (route) => {
    if (serve === "unserved") return route.fulfill({ status: 404, body: "not found" })
    const asked = decodeURIComponent(new URL(route.request().url()).pathname.split("/").pop() ?? "")
    const record = records.find((r) => r.id === asked)
    if (record === undefined) return route.fulfill({ status: 404, json: { code: "object.not_found", message: "No object with that id is readable here." } })
    return route.fulfill({ json: serve === "malformed" ? { ...record, values: { ...record.values as object, agreement: "accepted" } } : record })
  })
  await page.goto(`/episodes/${id}`)
  return { calls }
}
const band = (page: Page, title: string) => page.getByRole("region", { name: title })

test("view: an overridden suggestion reads as the strongest signal, with the blocked rule's waiver and the recorded outcome", async ({ page }) => {
  await open(page, "ep_demo_overridden")
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Release the held shipment?")
  await expect(page.getByText("Release a credit hold · Ask manager")).toBeVisible()
  await expect(page.getByText("Measured", { exact: true })).toBeVisible()
  await expect(band(page, "What was known").getByRole("link")).toHaveText(["North branch customer", "Delivery 0042"])
  await expect(band(page, "What was suggested")).toContainText("Suggested Keep holding")
  await expect(band(page, "What was suggested")).toContainText("Active")
  await expect(band(page, "What was decided")).toContainText("Overrode the suggestion")
  await expect(band(page, "What was decided")).toContainText("Release it · a manager")
  await expect(band(page, "What was decided")).toContainText("Customer paid the overdue invoice by phone")
  const rules = band(page, "Rules that applied").getByRole("listitem")
  await expect(rules).toHaveCount(2)
  await expect(rules.nth(0)).toContainText("Blocked · waived")
  await expect(rules.nth(0)).toContainText("Waived by a manager: Payment confirmed by phone")
  await expect(rules.nth(1)).toContainText("Allowed")
  await expect(band(page, "What happened")).toContainText("Recorded")
  await expect(band(page, "What happened")).toContainText("Invoice paid · within 14 days")
  await expect(band(page, "What happened")).toContainText("Days to pay2")
  const receipts = page.getByRole("complementary", { name: "Receipts" })
  await expect(receipts).toContainText("Not triaged yet")
  for (const label of ["Decision", "Suggestion", "Outcome", "Waiver · Hold shipments over the credit limit"]) await expect(receipts.getByText(label, { exact: true })).toBeVisible()
  await expect(receipts.getByRole("link", { name: "Open the run" })).toHaveAttribute("href", "/runs/run_restock_7")
  await shoot(page, "overridden")
})

test("view: technical shows the pins, generation and law ids; business keeps them out of the way", async ({ page }) => {
  await open(page, "ep_demo_overridden")
  await expect(page.getByText("demo.credit.hold_over_limit")).toHaveCount(0)
  await chooseFromAccountMenu(page, "Technical")
  await expect(page.getByText("demo.credit.hold_over_limit")).toBeVisible()
  await expect(band(page, "What was known")).toContainText("generation 12 · as of commit 4806")
  await expect(page.getByRole("complementary", { name: "Receipts" })).toContainText("Decision · commit 4812")
  await shoot(page, "technical")
  await chooseFromAccountMenu(page, "Business")
})

test("view: an accepted suggestion whose outcome is still open stays context only", async ({ page }) => {
  await open(page, "ep_demo_accepted")
  await expect(page.getByText("Context only", { exact: true })).toBeVisible()
  await expect(page.getByText("it can't become a curated case or a score")).toBeVisible()
  await expect(band(page, "What was suggested")).toContainText("Shadow")
  await expect(band(page, "What was decided")).toContainText("Accepted the suggestion")
  await expect(band(page, "What happened")).toContainText("Not measured yet")
  await expect(band(page, "What happened")).toContainText("No outcome has been recorded yet.")
  await expect(page.getByRole("complementary", { name: "Receipts" }).getByText("Outcome", { exact: true })).toHaveCount(0)
  await shoot(page, "open")
})

test("view: no suggestion, an outcome not measured in time, and an exception verdict from triage", async ({ page }) => {
  await open(page, "ep_demo_unsuggested")
  await expect(band(page, "What was known")).toContainText("No records were named")
  await expect(band(page, "What was suggested")).toContainText("Nothing was suggested")
  await expect(band(page, "What was decided")).toContainText("No suggestion")
  await expect(band(page, "Rules that applied")).toContainText("Would have blocked")
  await expect(band(page, "What happened")).toContainText("Not measured in time")
  const receipts = page.getByRole("complementary", { name: "Receipts" })
  await expect(receipts).toContainText("Exception")
  await expect(receipts).toContainText("Long-standing supplier")
  await expect(receipts.getByText("Suggestion", { exact: true })).toHaveCount(0)
  await shoot(page, "overdue")
})

test("while the cell does not serve episodes, the page says so and guesses nothing", async ({ page }) => {
  await open(page, "ep_demo_overridden", "unserved")
  await expect(page.getByText("This cell doesn't serve decision episodes yet.")).toBeVisible()
  await shoot(page, "unserved")
})

test("an episode this reader can't read reads as absent, with the cell's words", async ({ page }) => {
  await open(page, "ep_missing")
  await expect(page.getByText("No such episode")).toBeVisible()
  await expect(page.getByText("No object with that id is readable here.")).toBeVisible()
})

test("an episode that contradicts itself is refused, not drawn", async ({ page }) => {
  await open(page, "ep_demo_overridden", "malformed")
  await expect(page.getByText("Can't show this episode")).toBeVisible()
  await expect(page.getByText("/agreement is accepted but the choice is not the suggestion")).toBeVisible()
  await expect(page.getByRole("region", { name: "What was decided" })).toHaveCount(0)
  await shoot(page, "malformed")
})

// By CSS, not role: while the action sheet is open the page behind it is hidden from the accessibility tree.
const triageCard = (page: Page) => page.locator('aside[aria-label="Receipts"] section').first()
const sheet = (page: Page) => page.getByRole("dialog")

async function triage(page: Page, verdict: string, reason: string) {
  await triageCard(page).getByRole("button", { name: "Triage", exact: true }).click()
  await expect(sheet(page).getByLabel("Episode")).toHaveValue("ep_demo_overridden")
  await sheet(page).getByLabel("Verdict").selectOption(verdict)
  await sheet(page).getByLabel("Reason").fill(reason)
}

test("triage: mark an episode with a verdict and reason; the card shows it, and Undo retracts it through the cell", async ({ page }) => {
  const { calls } = await open(page, "ep_demo_overridden", "fixture", { serve: "catalog" })
  await expect(triageCard(page)).toContainText("Not triaged yet")
  await triage(page, "pattern", "Phone-confirmed payments clear within days")
  await shoot(page, "triage-form")
  await sheet(page).getByRole("button", { name: "Triage", exact: true }).click()
  await expect(sheet(page).getByRole("status")).toContainText("ep_demo_overridden")
  await expect(triageCard(page)).toContainText("Reusable pattern")
  await expect(triageCard(page)).toContainText("Phone-confirmed payments clear within days")
  await shoot(page, "triaged")
  await sheet(page).getByRole("button", { name: "Undo" }).click()
  await expect(sheet(page).getByText(/^Undone · /)).toBeVisible()
  await expect(triageCard(page)).toContainText("Not triaged yet")
  expect(calls.map((c) => c.actionId)).toEqual(["learning.triage_episode", "learning.retract_triage"])
  expect(calls[1]!.input).toEqual({ episodeId: "ep_demo_overridden" })
})

test("triage: a later verdict supersedes the first, and Retract triage withdraws it", async ({ page }) => {
  await open(page, "ep_demo_overridden", "fixture", { serve: "catalog" })
  await triage(page, "exception", "One-off: the customer called the owner directly")
  await sheet(page).getByRole("button", { name: "Triage", exact: true }).click()
  await sheet(page).getByRole("button", { name: "Done" }).click()
  await expect(triageCard(page)).toContainText("Exception")
  await triage(page, "gap", "The credit rule has no path for a confirmed phone payment")
  await sheet(page).getByRole("button", { name: "Triage", exact: true }).click()
  await sheet(page).getByRole("button", { name: "Done" }).click()
  await expect(triageCard(page)).toContainText("System gap")
  await expect(triageCard(page)).not.toContainText("Exception")
  await triageCard(page).getByRole("button", { name: "Retract triage" }).click()
  await sheet(page).getByRole("button", { name: "Retract triage" }).click()
  await sheet(page).getByRole("button", { name: "Done" }).click()
  await expect(triageCard(page)).toContainText("Not triaged yet")
})

test("triage: the cell's refusal reads in the form and nothing changes", async ({ page }) => {
  await open(page, "ep_demo_overridden", "fixture", { serve: "catalog", refuseNext: true })
  await triage(page, "noise", "Wrong customer on the delivery")
  await sheet(page).getByRole("button", { name: "Triage", exact: true }).click()
  await expect(sheet(page).getByRole("alert")).toContainText("This request was already used for a different change")
  await shoot(page, "triage-refused")
  await page.keyboard.press("Escape")
  await page.getByRole("button", { name: "Discard" }).click()
  await expect(triageCard(page)).toContainText("Not triaged yet")
})

test("while the cell takes no actions on episodes, the triage card says so and offers none", async ({ page }) => {
  await open(page, "ep_demo_overridden")
  await expect(triageCard(page)).toContainText("This cell doesn't take actions from Studio yet.")
  await expect(triageCard(page).getByRole("button")).toHaveCount(0)
})

test("triage: served L-i18n enum labels show in the Verdict dropdown while sending raw enum ids", async ({ page }) => {
  const { calls } = await open(page, "ep_demo_overridden", "fixture", { serve: "catalog" }, [
    {
      id: "learning.labels.v1",
      kind: "labels",
      version: 1,
      payload: {
        entries: [
          { target: { kind: "enumValue", objectType: "learning.Episode", property: "verdict", value: "pattern" }, label: { en: "Reusable pattern (pack)" } },
          { target: { kind: "enumValue", objectType: "learning.Episode", property: "verdict", value: "gap" }, label: { en: "Model gap (pack)" } },
        ],
      },
    },
  ])
  await triageCard(page).getByRole("button", { name: "Triage", exact: true }).click()
  const verdict = sheet(page).getByLabel("Verdict")
  await expect(verdict.locator('option[value="pattern"]')).toHaveText("Reusable pattern (pack)")
  await expect(verdict.locator('option[value="gap"]')).toHaveText("Model gap (pack)")
  // Unlabelled enum value falls back to its raw id without guessing.
  await expect(verdict.locator('option[value="exception"]')).toHaveText("exception")
  await verdict.selectOption("pattern")
  await sheet(page).getByLabel("Reason").fill("Confirmed via L-i18n label")
  await sheet(page).getByRole("button", { name: "Triage", exact: true }).click()
  await expect(sheet(page).getByRole("status")).toContainText("ep_demo_overridden")
  expect(calls[0]!.input["verdict"]).toBe("pattern")
})

test("list: Workspace Learning space lists episodes, sends flat server agreement filter, renders withheld rows and opens a timeline", async ({ page }) => {
  await session(page)
  await advertiseLearning(page, { objectTypes: ["learning.Episode"] })
  const records = structuredClone(fixture.records) as { id: string; objectType: string; scope: string; version: number; values: Record<string, unknown> }[]
  const queries: URL[] = []
  let failSnapshotOnce = false
  // Mirrors crates/norn-api/src/object_query.rs:291 (`is_name`): flat `[a-z][a-z0-9_]*` property names only.
  const FLAT_PROPERTY = /^[a-z][a-z0-9_]*$/

  await page.route("**/api/host/v1/objects/learning.Episode**", (route) => {
    const url = new URL(route.request().url())
    const suffix = url.pathname.split("/v1/objects/learning.Episode")[1] ?? ""
    if (suffix.startsWith("/") && suffix.length > 1) {
      const asked = decodeURIComponent(suffix.slice(1))
      const found = records.find((r) => r.id === asked)
      return found ? route.fulfill({ json: found }) : route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such object." } })
    }
    queries.push(url)
    if (failSnapshotOnce) {
      failSnapshotOnce = false
      return route.fulfill({ status: 409, json: { code: "object.snapshot_moved", message: "Snapshot moved" } })
    }
    const rawFilter = url.searchParams.get("filter")
    const filter = rawFilter ? (JSON.parse(rawFilter) as { all?: { property: string; op: string; value?: unknown }[] }) : null
    const clauses = filter?.all ?? []
    for (const c of clauses) {
      if (!FLAT_PROPERTY.test(c.property) || c.property !== "agreement") {
        return route.fulfill({
          status: 400,
          json: { code: "query.property_unknown", message: `Unsupported filter property: ${c.property}` },
        })
      }
    }
    const matched = records.filter((r) => clauses.every((c) => c.property === "agreement" && c.op === "eq" && r.values["agreement"] === c.value))
    const rows = clauses.length === 0
      ? [...matched, { objectType: "learning.Episode", id: "ep_withheld_1", scope: "org_demo/analytics", version: null, values: null, withheld: true, withheldProperties: [] }]
      : matched
    return route.fulfill({
      json: { objectType: "learning.Episode", records: rows, query: { sort: [], filter }, total: { count: rows.length, approximate: false }, next: null, quarantineCount: 0 },
    })
  })

  // Reach Episodes through the Workspace → Learning space in the shell sidebar.
  await page.goto("/")
  const nav = page.getByRole("navigation", { name: "Workspace" })
  await expect(nav).toContainText("Learning")
  await nav.getByRole("link", { name: "Episodes" }).click()
  await expect(page).toHaveURL(/\/learning\/episodes$/)

  const table = page.getByRole("table", { name: "Decision episodes" })
  await expect(table).toContainText("Release the held shipment?")
  await expect(table).toContainText("Withheld by cell policy")
  await shoot(page, "list")

  // S2: dotted/nested filters rejected by object_query.rs stay hidden until the backend lane lands them.
  await expect(page.getByLabel("Filter by outcome")).toHaveCount(0)
  await expect(page.getByLabel("Filter by triage verdict")).toHaveCount(0)
  await expect(page.getByLabel("Law id")).toHaveCount(0)
  await expect(page.getByLabel("Producer pin")).toHaveCount(0)

  // Exercise the flat server agreement filter.
  await page.getByLabel("Filter by agreement").selectOption("overridden")
  await expect(page.getByText("agreement: overridden")).toBeVisible()
  await expect(table.locator("tbody tr")).toHaveCount(1)

  const lastFilter = JSON.parse(queries.at(-1)!.searchParams.get("filter") ?? "null")
  expect(lastFilter).toEqual({
    all: [{ property: "agreement", op: "eq", value: "overridden" }],
  })

  // Snapshot moved notice resets cleanly on refresh.
  failSnapshotOnce = true
  await page.getByRole("button", { name: "Refresh" }).click()
  await expect(page.getByRole("status")).toContainText("changed")

  // Opening an episode row navigates into the episode timeline.
  await table.getByRole("link", { name: "Release the held shipment?" }).click()
  await expect(page).toHaveURL(/\/episodes\/ep_demo_overridden$/)
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Release the held shipment?")
})

test("links: See episode renders from a run receipt and decided inbox sheet only when episodeId is returned AND learning.Episode is advertised", async ({ page }) => {
  await session(page)
  await advertiseLearning(page, { objectTypes: [] })
  await decisionCell(page)
  const records = structuredClone(fixture.records)
  await page.route("**/api/host/v1/objects/learning.Episode/**", (route) => {
    const id = decodeURIComponent(new URL(route.request().url()).pathname.split("/").pop() ?? "")
    const found = records.find((r) => r.id === id)
    return found ? route.fulfill({ json: found }) : route.fulfill({ status: 404, json: { code: "object.not_found", message: "Not found" } })
  })

  // Serve a completed run built from the route-pinned envelope with provisional `episodeId` on a step receipt,
  // and override :decide to return provisional `episodeId` on the server reply.
  const runWithEpisode = structuredClone(servedDecisionRun.runCompleted.json) as Record<string, unknown>
  runWithEpisode["id"] = "run_count_3"
  const runSteps = runWithEpisode["steps"] as { receipts: unknown[] }[]
  runSteps[0]!.receipts = [{ kind: "decision", receiptHash: "a".repeat(64), commitSeq: 4812, recordedAtMs: Date.now(), executor: "you", episodeId: "ep_demo_overridden" }]
  await page.route("**/api/host/v1/runs/run_count_3**", (route) => route.fulfill({ json: runWithEpisode }))
  await page.route("**/api/host/v1/decisions/*:decide**", (route) => route.fulfill({
    json: {
      ...servedDecisionRun.decided.json,
      outcome: "decided",
      decision: { ...servedDecisionRun.decided.json.decision, id: "dec_UmVjb3VudC1iaW4", choice: "recount", decidedBy: "you", decidedAtMs: Date.now() },
      receipt: { ...servedDecisionRun.decided.json.receipt, receiptHash: "b".repeat(64), commitSeq: 4999, recordedAtMs: Date.now() },
      run: { ...servedDecisionRun.decided.json.run, id: "run_count_4", state: "running" },
      episodeId: "ep_demo_overridden",
    },
  }))

  // S1 negative: when learning.Episode is NOT advertised, "See episode" stays hidden on both surfaces even when episodeId is returned.
  await page.goto("/runs/run_count_3")
  await page.getByText(/Done by you/).click()
  await expect(page.getByRole("link", { name: "See episode" })).toHaveCount(0)

  await page.goto("/decisions")
  await page.getByRole("list", { name: "Decisions waiting on you" }).getByRole("button", { name: /Recount it\?/ }).click()
  await expect(page.locator("norn-decision-sheet .norn-sheet")).toHaveAttribute("data-engine", "ready", { timeout: 30_000 })
  await sheet(page).getByLabel("Recount").check()
  await sheet(page).getByRole("button", { name: "Decide: Recount" }).click()
  await expect(sheet(page).getByText("Decided: Recount")).toBeVisible()
  await expect(sheet(page).getByRole("link", { name: "See episode" })).toHaveCount(0)

  // S1 positive: once the scoped cell advertises learning.Episode, "See episode" renders and navigates from both surfaces.
  await page.unroute("**/api/host/v1/definitions**")
  await advertiseLearning(page, { objectTypes: ["learning.Episode"] })

  await page.goto("/runs/run_count_3")
  await page.getByText(/Done by you/).click()
  const runLink = page.getByRole("link", { name: "See episode" })
  await expect(runLink).toHaveAttribute("href", "/episodes/ep_demo_overridden")
  await runLink.click()
  await expect(page).toHaveURL(/\/episodes\/ep_demo_overridden$/)

  await page.goto("/decisions")
  await page.getByRole("list", { name: "Decisions waiting on you" }).getByRole("button", { name: /Recount it\?/ }).click()
  await expect(page.locator("norn-decision-sheet .norn-sheet")).toHaveAttribute("data-engine", "ready", { timeout: 30_000 })
  await sheet(page).getByLabel("Recount").check()
  await sheet(page).getByRole("button", { name: "Decide: Recount" }).click()
  const sheetEpLink = sheet(page).getByRole("link", { name: "See episode" })
  await expect(sheetEpLink).toBeVisible()
  await expect(sheetEpLink).toHaveAttribute("href", "/episodes/ep_demo_overridden")
  await sheetEpLink.click()
  await expect(page).toHaveURL(/\/episodes\/ep_demo_overridden$/)
})
