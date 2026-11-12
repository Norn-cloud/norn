import { expect, test, type Page } from "@playwright/test"
import { decisionCell } from "./decision-cell.ts"
import { session } from "./object-cell.ts"

// One test per row of the decisions inbox and run view's verb matrix (see the PR), plus each state.
// They use a stand-in for RUNS's decision and run routes; they skip on a stage that does not serve them.
// SHOTS=<dir> saves every state in light and dark for review.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/decisions-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the decision and run route stand-ins`) })

async function inbox(page: Page, options: Parameters<typeof decisionCell>[1] = {}) {
  await session(page)
  const cell = await decisionCell(page, options)
  await page.goto("/decisions")
  return cell
}

const list = (page: Page) => page.getByRole("list", { name: "Decisions waiting on you" }).getByRole("button")
const sheet = (page: Page) => page.getByRole("dialog")
const open = async (page: Page, text: string) => { await list(page).filter({ hasText: text }).click(); await expect(sheet(page)).toBeVisible() }

test("view: the inbox lists what waits on you, soonest deadline first, with the nav's count, risk, suggestion and countdown", async ({ page }) => {
  await inbox(page)
  await expect(list(page)).toHaveCount(3)
  await expect(list(page).nth(0)).toContainText("Release the held shipment")
  await expect(list(page).nth(0)).toContainText(/4[45] min left/)
  await expect(list(page).nth(0)).toContainText("High risk")
  await expect(list(page).nth(0)).toContainText("Suggested: Keep holding")
  await expect(list(page).nth(0)).toContainText("Restock gold · Ask manager") // a step with no label reads its id in words
  await expect(list(page).nth(1)).toContainText("Refund review · Approve refund") // a flow with no title likewise
  await expect(list(page).nth(2)).toContainText(/(6 days|5 days 23 h) left/)
  await expect(page.locator('a[href="/decisions"]').first()).toContainText("3")
  await shoot(page, "inbox")
})

test("view: a capped count reads as a lower bound; nothing waiting reads as such", async ({ page }) => {
  await inbox(page, { capped: true })
  await expect(page.getByText("500+ waiting on you")).toBeVisible()
  await expect(page.locator('a[href="/decisions"]').first()).toContainText("500+")
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await inbox(page, { empty: true })
  await expect(page.getByText("Nothing waits on you")).toBeVisible()
  await shoot(page, "empty")
})

test("edit (decide): a low-risk choice is recorded at once, with its receipt under Details, and leaves the inbox", async ({ page }) => {
  const cell = await inbox(page)
  await open(page, "Recount it?")
  await sheet(page).getByLabel("Recount").check()
  await sheet(page).getByLabel(/^Note/).fill("Scale was recalibrated this morning.")
  await sheet(page).getByRole("button", { name: "Decide: Recount" }).click()
  await expect(sheet(page).getByRole("status")).toHaveText("Decided: Recount · by you")
  await expect(sheet(page)).toContainText("Scale was recalibrated this morning.")
  await sheet(page).getByText("Details").click()
  await expect(sheet(page).getByText("Commit")).toBeVisible()
  expect(JSON.parse(cell.calls.at(-1)!.body)).toMatchObject({ choice: "recount", note: "Scale was recalibrated this morning." })
  await shoot(page, "decided")
  await sheet(page).getByRole("button", { name: "Back to inbox" }).click()
  await expect(list(page)).toHaveCount(2)
})

test("edit (decide): a high-risk choice that starts an action confirms first; the run goes on and is read live", async ({ page }) => {
  await inbox(page)
  await open(page, "Release the held shipment")
  await expect(sheet(page).getByText("Suggested")).toBeVisible()
  await sheet(page).getByLabel("Release it").check()
  await sheet(page).getByRole("button", { name: "Decide: Release it" }).click()
  await expect(sheet(page).getByRole("alertdialog")).toContainText("This is a high-risk decision.")
  await shoot(page, "confirm")
  await sheet(page).getByRole("button", { name: "Back" }).click()
  await sheet(page).getByRole("button", { name: "Decide: Release it" }).click()
  await sheet(page).getByRole("button", { name: "Yes, release it" }).click()
  await expect(sheet(page).getByRole("status")).toHaveText("Decided: Release it · by you")
  await sheet(page).getByRole("link", { name: "See the run go on" }).click()
  await expect(page).toHaveURL(/\/runs\/run_restock_7$/)
  const steps = page.getByRole("list", { name: "Steps" }).getByRole("listitem")
  await expect(steps.nth(1)).toContainText("Done")
  await expect(steps.nth(1)).toContainText("Done by you")
  await expect(page.getByText("Live")).toBeVisible()
  // The run is read again while it works, until it succeeds.
  await expect(page.getByText("Succeeded")).toBeVisible({ timeout: 20_000 })
  await expect(page.getByText("Live")).toHaveCount(0)
  await shoot(page, "run-succeeded")
})

test("the race: another decider got there first; the sheet says who and when, and nothing of yours was sent", async ({ page }) => {
  const cell = await inbox(page)
  await open(page, "Release the held shipment")
  cell.resolveElsewhere("dec_UmVsZWFzZS1nb2xk", "Mona Ali", "hold")
  await sheet(page).getByLabel("Keep holding").check()
  await sheet(page).getByRole("button", { name: "Decide: Keep holding" }).click()
  await sheet(page).getByRole("button", { name: "Yes, keep holding" }).click() // high risk: every choice confirms
  await expect(sheet(page).getByRole("status")).toHaveText(/^Decided by Mona Ali at \d{2}:\d{2}( [AP]M)?: Keep holding$/)
  await expect(sheet(page)).toContainText("Nothing you chose was sent.")
  await shoot(page, "race")
  await sheet(page).getByRole("button", { name: "Back to inbox" }).click()
  await expect(list(page)).toHaveCount(2)
})

test("validate: the cell's refusals read where they point: a law at the top, a note at the note", async ({ page }) => {
  await inbox(page)
  await open(page, "Refund 1,240.00 USD")
  await sheet(page).getByLabel("Reject").check()
  await sheet(page).getByLabel(/^Note/).fill("x".repeat(501))
  await sheet(page).getByRole("button", { name: "Decide: Reject" }).click()
  // The refusal order puts the law (403) before the note (422): the law reads first.
  await expect(sheet(page).getByRole("alert").first()).toContainText("Refunds over 1,000.00 USD need a finance manager's approval.")
  await shoot(page, "refused")
})

test("validate: a note the cell refuses lands on the note field", async ({ page }) => {
  await inbox(page)
  await open(page, "Recount it?")
  await sheet(page).getByLabel("Accept the count").check()
  await sheet(page).getByLabel(/^Note/).fill("y".repeat(501))
  await sheet(page).getByRole("button", { name: "Decide: Accept the count" }).click()
  await expect(sheet(page).getByLabel(/^Note/)).toHaveAttribute("aria-invalid", "true")
  await expect(sheet(page).getByRole("alert").filter({ hasText: "A note is at most 500 characters." })).toBeVisible()
})

test("expiry: past the deadline the cell refuses, and the sheet says it expired", async ({ page }) => {
  const cell = await inbox(page)
  await open(page, "Release the held shipment")
  cell.advance(46 * 60_000)
  await sheet(page).getByLabel("Keep holding").check()
  await sheet(page).getByRole("button", { name: "Decide: Keep holding" }).click()
  await sheet(page).getByRole("button", { name: "Yes, keep holding" }).click() // high risk: every choice confirms
  await expect(sheet(page).getByRole("status")).toHaveText("Expired")
  await shoot(page, "expired")
  await sheet(page).getByRole("button", { name: "Back to inbox" }).click()
  await expect(list(page)).toHaveCount(2)
})

test("reopen: leaving with a choice asks first; an outage retries with the same key and decides once", async ({ page }) => {
  const cell = await inbox(page)
  await open(page, "Recount it?")
  await sheet(page).getByLabel("Recount").check()
  await page.keyboard.press("Escape")
  await expect(sheet(page).getByRole("alertdialog", { name: "Leave without deciding?" })).toBeVisible()
  await page.getByRole("button", { name: "Keep deciding" }).click()
  await expect(sheet(page).getByLabel("Recount")).toBeChecked()
  cell.failNext()
  await sheet(page).getByRole("button", { name: "Decide: Recount" }).click()
  await expect(sheet(page).getByRole("button", { name: "Retry" })).toBeVisible()
  await shoot(page, "outage")
  await sheet(page).getByRole("button", { name: "Retry" }).click()
  await expect(sheet(page).getByRole("status")).toHaveText("Decided: Recount · by you")
  const keys = cell.calls.map((c) => JSON.parse(c.body).idempotencyKey)
  expect(new Set(keys).size).toBe(1)
})

test("run view: a waiting run links to its decision; Run again starts a new run and reads it until it ends; an unknown run says so", async ({ page }) => {
  await session(page)
  await decisionCell(page)
  await page.goto("/runs/run_restock_7")
  await expect(page.getByRole("heading", { name: "Restock gold" })).toBeVisible()
  await expect(page.getByText("Waiting on a decision")).toBeVisible()
  await expect(page.getByRole("list", { name: "Steps" }).getByRole("listitem").nth(1)).toContainText("Waiting on a person")
  await shoot(page, "run-waiting")
  await page.getByRole("link", { name: "Open decision" }).click()
  await expect(sheet(page).getByRole("heading", { name: /Release the held shipment/ })).toBeVisible()
  await page.goto("/runs/run_restock_7")
  await page.getByRole("button", { name: "Run again" }).click()
  await expect(page).toHaveURL(/\/runs\/run_[0-9a-f-]{36}$/)
  await expect(page.getByText("Fetch orders")).toBeVisible() // an unlabelled step reads its id in words
  await expect(page.getByText("Succeeded")).toBeVisible({ timeout: 20_000 })
  await page.goto("/runs/run_nope")
  await expect(page.getByText("There is no such run you can read.")).toBeVisible()
})

test("run view, first cut: without steps the run shows its state, flow and generation, says step details are coming, and is read again when the cell says it changed", async ({ page }) => {
  await session(page)
  const cell = await decisionCell(page, { legacy: true })
  let reads = 0
  page.on("request", (request) => { if (request.url().includes("/api/host/v1/runs/run_restock_7")) reads += 1 })
  await page.goto("/runs/run_restock_7")
  await expect(page.getByRole("heading", { name: "Restock gold" })).toBeVisible()
  const card = page.getByRole("region", { name: "Run" })
  await expect(card).toContainText("awaiting_perform_ask_manager")
  await expect(card).toContainText("Generation")
  await expect(card).toContainText("Step details are coming soon")
  await expect(page.getByRole("list", { name: "Steps" })).toHaveCount(0)
  await expect(page.getByText("In progress")).toBeVisible()
  await shoot(page, "run-first-cut")
  // Nothing polls (UI-LIVE-1): a commit that changes the run is announced on the live socket and read once.
  await page.waitForTimeout(3_000)
  expect(reads).toBe(1)
  cell.runs.get("run_restock_7")!.steps.find((s) => s.status === "waiting")!.status = "running"
  await expect.poll(() => reads, { timeout: 15_000 }).toBeGreaterThan(1)
})

test("run view, first cut: a run the cell calls terminal reads as ended and is not read again", async ({ page }) => {
  await session(page)
  await decisionCell(page, { legacy: true })
  let reads = 0
  page.on("request", (request) => { if (request.url().includes("/api/host/v1/runs/run_count_3")) reads += 1 })
  await page.goto("/runs/run_count_3")
  await expect(page.getByText("Ended", { exact: true })).toBeVisible()
  await expect(page.getByText("Live")).toHaveCount(0)
  await page.waitForTimeout(7_000) // an ended run is not watched: no socket subscription, no read
  expect(reads).toBe(1)
})

test("not served and no cell: the inbox and the run view say why, and call nothing", async ({ page }) => {
  await session(page)
  await page.route("**/api/host/v1/decisions**", (route) => route.fulfill({ status: 404, body: "" }))
  await page.route("**/api/host/v1/runs**", (route) => route.fulfill({ status: 404, body: "" }))
  await page.goto("/decisions")
  await expect(page.getByText("This cell doesn't serve decisions yet.")).toBeVisible()
  await page.goto("/runs/run_restock_7")
  await expect(page.getByText("This cell doesn't serve runs yet.")).toBeVisible()
  await shoot(page, "unavailable")
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await session(page, null)
  let calls = 0
  await page.route("**/api/host/**", (route) => { calls += 1; return route.fulfill({ status: 500 }) })
  await page.goto("/decisions")
  await expect(page.getByText("This workspace has no data cell yet.")).toBeVisible()
  expect(calls).toBe(0)
})
