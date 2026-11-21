import { expect, test, type Browser, type Page } from "@playwright/test"
import { actionCell } from "./action-cell.ts"
import { APPROVER, approvalCell, REQUESTER, type ApprovalCell } from "./approval-cell.ts"
import { objectCell, population } from "./object-cell.ts"

// APPROVAL-REQUESTED-1 (#1531): the requester runs their approved call. Two browsers act on one
// cell stand-in: the requester submits a gated call, it parks, an approver approves it (which runs
// nothing), and the requester's "Approved, run it" sends exactly the parked call and shows what the
// cell answered. SHOTS=<dir> saves every state in light and dark for review.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400) // past the 200ms colour transitions
    await page.screenshot({ path: `${shots}/approved-run-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the approval route stand-in`) })

const HOLDING = "demo/holding/0001"
const sheet = (page: Page) => page.getByRole("dialog")
const runIt = (page: Page) => page.getByRole("button", { name: "Approved, run it" })

/** The requester's browser on the holdings grid, with the action and approval stand-ins. */
async function requester(browser: Browser, cell: ApprovalCell) {
  const page = await (await browser.newContext()).newPage()
  const objects = await objectCell(page, population(12))
  const actions = await actionCell(page, objects)
  await cell.install(page, REQUESTER)
  await page.goto("/objects/canon.Holding")
  await expect(page.getByText("12 records", { exact: true })).toBeVisible()
  return { page, actions }
}

/** The approver's browser on the decisions inbox. */
async function approver(browser: Browser, cell: ApprovalCell) {
  const page = await (await browser.newContext()).newPage()
  await cell.install(page, APPROVER)
  await page.goto("/decisions")
  return page
}

/** The requester asks to adjust a holding: the gated call parks and the sheet says it waits. */
async function park(page: Page) {
  await page.getByRole("grid").locator('[role="row"]').filter({ hasText: HOLDING }).click()
  await page.getByRole("button", { name: "Adjust quantity" }).click()
  await sheet(page).getByLabel("Counted quantity").fill("12.500")
  await sheet(page).getByLabel("Reason").fill("Recount after audit")
  await sheet(page).getByRole("button", { name: "Adjust quantity" }).click()
  await expect(sheet(page).getByRole("region", { name: "Sent for approval" }).getByRole("status")).toContainText("Waiting for a decider's approval.")
}

/** The approver approves it in their inbox; the decide runs nothing and says it waits for the requester. */
async function approve(page: Page) {
  await page.getByRole("list", { name: "Decisions waiting on you" }).getByRole("button").filter({ hasText: "Adjust this holding's counted quantity?" }).click()
  await expect(page.locator("norn-decision-sheet .norn-sheet")).toHaveAttribute("data-engine", "ready", { timeout: 30_000 })
  await sheet(page).getByLabel("Approve").check()
  await sheet(page).getByRole("button", { name: "Decide: Approve" }).click()
  await sheet(page).getByRole("button", { name: "Yes, approve" }).click()
  await expect(sheet(page).locator('[data-region="outcome-call"]')).toHaveText("Approved; waiting for the requester to run it.")
}

test("run: submit, park, the approver approves, the requester runs it from the sheet, committed", async ({ browser }) => {
  const cell = approvalCell()
  const asker = await requester(browser, cell)
  await park(asker.page)
  // While a decider has it there is nothing to run.
  await expect(runIt(asker.page)).toHaveCount(0)
  await shoot(asker.page, "parked")

  const decider = await approver(browser, cell)
  await approve(decider)
  await shoot(decider, "approver-decided")
  // The approver is not the requester: the cell offers them nothing to run, anywhere.
  await expect(runIt(decider)).toHaveCount(0)

  // The requester's open sheet hears the approval from the cell and offers the run.
  await expect(runIt(asker.page)).toBeVisible()
  await expect(sheet(asker.page).getByRole("region", { name: "Approved call" }).getByRole("status")).toContainText("Approved. Run it to make the change.")
  await shoot(asker.page, "approved")
  await runIt(asker.page).click()
  await expect(sheet(asker.page).getByRole("region", { name: "Receipt" }).getByRole("status")).toContainText(`Holding ${HOLDING} is now counted`)
  await shoot(asker.page, "committed")

  // It sent exactly the parked call: the same body, so the same input digit for digit and the same key.
  const sent = cell.calls.filter((c) => c.by === REQUESTER.id).map((c) => c.body)
  expect(sent).toHaveLength(2)
  expect(sent[1]).toBe(sent[0])
  expect(JSON.parse(sent[0]!).input).toEqual({ id: HOLDING, quantity: "12.500", reason: "Recount after audit" })
  expect(asker.actions.calls.filter((c) => c.actionId === "canon.holding.adjust")).toHaveLength(1)
})

test("run: the requester sees the approved call in their inbox and runs it from there", async ({ browser }) => {
  const cell = approvalCell()
  const asker = await requester(browser, cell)
  await park(asker.page)
  await sheet(asker.page).getByRole("region", { name: "Sent for approval" }).getByRole("button", { name: "Close" }).click()

  await asker.page.goto("/decisions")
  const mine = asker.page.getByRole("list", { name: "Your requests" })
  await expect(mine.getByRole("listitem")).toHaveCount(1)
  await expect(mine).toContainText("Waiting for a decider's approval.")
  // The requester never decides their own call: it is not among the decisions waiting on them.
  await expect(asker.page.getByRole("list", { name: "Decisions waiting on you" })).toHaveCount(0)
  await expect(runIt(asker.page)).toHaveCount(0)
  await shoot(asker.page, "inbox-pending")

  await approve(await approver(browser, cell))
  await expect(mine.getByRole("button", { name: "Approved, run it" })).toBeVisible()
  await shoot(asker.page, "inbox-approved")
  await mine.getByRole("button", { name: "Approved, run it" }).click()
  await expect(sheet(asker.page).getByRole("region", { name: "Receipt" }).getByRole("status")).toContainText(`Holding ${HOLDING} is now counted`)
  // Once it ran it no longer waits.
  await sheet(asker.page).getByRole("button", { name: "Done" }).click()
  await expect(mine).toHaveCount(0)
})

test("never: an approver who is not the requester never sees run it", async ({ browser }) => {
  const cell = approvalCell()
  const asker = await requester(browser, cell)
  await park(asker.page)
  const decider = await approver(browser, cell)
  // Before and after deciding: no request of theirs, no offer, in the inbox or the decision sheet.
  await expect(decider.getByRole("list", { name: "Your requests" })).toHaveCount(0)
  await approve(decider)
  await expect(runIt(decider)).toHaveCount(0)
  await decider.reload()
  await expect(decider.getByRole("list", { name: "Your requests" })).toHaveCount(0)
  await expect(runIt(decider)).toHaveCount(0)
  // The cell itself serves the approver no item, even by the decision's id.
  const id = cell.parked[0]!.id
  const read = await decider.evaluate(async (decision) => (await fetch(`/api/host/v1/decisions:requested?id=${decision}&orgId=org_demo&environmentId=analytics`)).json(), id)
  expect(read).toMatchObject({ items: [] })
})

test("revoked: a requester whose role was revoked after parking gets the cell's refusal, plainly", async ({ browser }) => {
  const cell = approvalCell()
  const asker = await requester(browser, cell)
  await park(asker.page)
  await approve(await approver(browser, cell))
  cell.revoke(REQUESTER)
  await expect(runIt(asker.page)).toBeVisible()
  await runIt(asker.page).click()
  const panel = sheet(asker.page).getByRole("region", { name: "Approved call" })
  await expect(panel.getByRole("status")).toContainText("was approved, but the cell refused it when you ran it. Nothing was changed.")
  await expect(sheet(asker.page).getByRole("alert")).toContainText("Refused")
  // Nothing ran, and the approval still waits for a retry the cell will allow.
  expect(asker.actions.calls.filter((c) => c.actionId === "canon.holding.adjust")).toHaveLength(0)
  await shoot(asker.page, "revoked")
})
