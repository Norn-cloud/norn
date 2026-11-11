import { expect, test, type Page } from "@playwright/test"
import { actionCell } from "./action-cell.ts"
import { objectCell, population, session } from "./object-cell.ts"

// One test per row of the action forms' verb matrix (see the PR), plus each state the forms show.
// They use stand-ins for CELL-DO's object and action routes, so they skip on a deployed stage
// until it serves them. SHOTS=<dir> saves every state in light and dark for review.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400) // past the 200ms colour transitions
    await page.screenshot({ path: `${shots}/form-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the object and action route stand-ins`) })

async function open(page: Page, count = 40) {
  await session(page)
  const objects = await objectCell(page, population(count))
  const actions = await actionCell(page, objects)
  await page.goto("/objects/canon.Holding")
  await expect(page.getByText(`${count} records`)).toBeVisible()
  return { objects, actions }
}

const sheet = (page: Page) => page.getByRole("dialog")
const rowFor = (page: Page, id: string) => page.getByRole("grid").locator('[role="row"]').filter({ hasText: id })

/** "New holding" is a menu once the cell offers more than one whole-record action. */
async function newRecord(page: Page, title: string) {
  await page.getByRole("button", { name: "New holding" }).click()
  await page.getByRole("menuitem", { name: title }).click()
}

/** "Open a holding" declares two groups, so it is a wizard: fill each step from the first, ending on the review. */
async function fillHolding(page: Page, id: string, quantity: string) {
  const form = sheet(page)
  await form.getByLabel("Holding id").fill(id)
  await form.getByLabel("Resource code").fill("R-9001")
  await form.getByLabel("Quantity").fill(quantity)
  await form.getByLabel("Valuation method").selectOption("moving_average")
  await form.getByRole("button", { name: "Next" }).click()
  await form.getByLabel("Location › Site").fill("vault-a")
  await form.getByRole("button", { name: "Next" }).click()
  await form.locator('button[type="submit"]', { hasText: "Review" }).click()
}

test("view: a single-group schema is one page: its titled section, the record's id locked first, the submit bar in view", async ({ page }) => {
  await open(page)
  await page.getByRole("button", { name: "New holding" }).click()
  await expect(page.getByRole("menuitem")).toHaveText(["Open a holding", "Restock holding"])
  await page.keyboard.press("Escape")
  await rowFor(page, "demo/holding/0001").click()
  await page.getByRole("button", { name: "Adjust quantity" }).click()
  const form = sheet(page)
  await expect(form.getByRole("navigation", { name: "Steps" })).toHaveCount(0)
  await expect(form.locator("section h3")).toHaveText(["The count"])
  await expect(form.locator("section").first().locator("label").first()).toHaveText(/^Holding id/)
  await expect(form.getByLabel("Counted quantity")).toHaveAttribute("placeholder", "e.g. 12.500")
  await expect(form.getByText("(g)")).toBeVisible()
  await expect(form.getByRole("button", { name: "Adjust quantity" })).toBeInViewport()
  // Business view: no action id under the title.
  await expect(form.getByText("canon.holding.adjust")).toHaveCount(0)
  await shoot(page, "sections")
})

test("create: a new holding commits, shows its receipt, and appears in the grid", async ({ page }) => {
  const { actions } = await open(page)
  await newRecord(page, "Open a holding")
  await fillHolding(page, "demo/holding/9001", "10.500")
  await sheet(page).getByRole("button", { name: "Open a holding" }).click()
  await expect(sheet(page).getByRole("status")).toHaveText("Holding demo/holding/9001 opened · just now")
  // Receipt internals sit under Details, closed in the business view.
  await expect(sheet(page).getByText("Commit")).toBeHidden()
  await sheet(page).getByText("Details").click()
  await expect(sheet(page).getByText("Commit")).toBeVisible()
  // The exact decimal went as typed, digit for digit.
  expect(actions.calls.at(-1)?.body).toContain('"quantity":"10.500"')
  await shoot(page, "receipt")
  // View record: the sheet closes, the grid reads again, and the new record's drawer opens.
  await sheet(page).getByRole("button", { name: "View record" }).click()
  await expect(page.getByRole("dialog").getByRole("heading", { name: "demo/holding/9001" })).toBeVisible()
  await expect(page.getByRole("dialog")).toContainText("10.500")
  await page.keyboard.press("Escape")
  await expect(page.getByText("41 records")).toBeVisible()
})

test("validate: the cell's input refusal shows on the form, which stays on the review; fixing it commits", async ({ page }) => {
  await open(page)
  await newRecord(page, "Open a holding")
  const form = sheet(page)
  await form.getByLabel("Quantity").fill("12,5")
  for (let i = 0; i < 3; i += 1) await form.locator('button[type="submit"]').click() // Next, Next, Review
  await form.getByRole("button", { name: "Open a holding" }).click()
  // The served route names no field in an input refusal yet, so the refusal shows on the review.
  await expect(form.getByRole("alert").first()).toContainText("the request is invalid")
  await expect(form.getByRole("region", { name: "Review" })).toBeVisible()
  await expect(form.locator('[aria-invalid="true"]')).toHaveCount(0)
  await shoot(page, "refused")
  await form.getByRole("navigation", { name: "Steps" }).getByRole("button").first().click()
  await fillHolding(page, "demo/holding/9002", "12.500")
  await form.getByRole("button", { name: "Open a holding" }).click()
  await expect(form.getByRole("status")).toHaveText(/^Holding demo\/holding\/\d+ opened · just now$/)
})

test("edit: a transition from the record drawer carries the record's id and moves its version", async ({ page }) => {
  await open(page)
  await rowFor(page, "demo/holding/0001").click()
  await page.getByRole("button", { name: "Adjust quantity" }).click()
  const form = sheet(page)
  // The record's id is locked: read-only and muted, not an editable field.
  await expect(form.getByLabel("Holding id")).toHaveValue("demo/holding/0001")
  await expect(form.getByLabel("Holding id")).toHaveAttribute("readonly", "")
  await form.getByLabel("Counted quantity").fill("40.000")
  await form.getByLabel("Reason").fill("Recount")
  await form.getByRole("button", { name: "Adjust quantity" }).click()
  await expect(form.getByRole("status")).toHaveText("Holding demo/holding/0001 adjusted · just now")
  await form.getByText("Details").click()
  await expect(form.locator("dd").nth(3)).toHaveText("2") // version
  await form.getByRole("button", { name: "Done" }).click()
  await expect(rowFor(page, "demo/holding/0001")).toContainText("40.000")
})

test("delete: a destructive, irreversible action confirms first, then later actions on it are refused by the rule", async ({ page }) => {
  await open(page)
  await rowFor(page, "demo/holding/0002").click()
  await page.getByRole("button", { name: "Retire holding" }).click()
  const form = sheet(page)
  await form.getByRole("button", { name: "Retire holding" }).click()
  await expect(form.getByRole("alertdialog")).toContainText("This can't be undone.")
  await shoot(page, "confirm")
  await form.getByRole("button", { name: "Back" }).click()
  await expect(form.getByRole("alertdialog")).toHaveCount(0)
  await form.getByRole("button", { name: "Retire holding" }).click()
  await form.getByRole("button", { name: "Yes, retire holding" }).click()
  await expect(form.getByRole("status")).toHaveText("Holding demo/holding/0002 retired · just now")
  await form.getByRole("button", { name: "Done" }).click()
  await rowFor(page, "demo/holding/0002").click()
  await page.getByRole("button", { name: "Adjust quantity" }).click()
  await sheet(page).getByLabel("Counted quantity").fill("1.000")
  await sheet(page).getByLabel("Reason").fill("Late count")
  await sheet(page).getByRole("button", { name: "Adjust quantity" }).click()
  await expect(sheet(page).getByRole("alert").first()).toContainText("This holding is retired; it can't change any more.")
  await expect(sheet(page).getByRole("alert").first()).toContainText("The holding is retired.")
})

test("reopen: a dirty draft asks before it is discarded; keep editing keeps it; a retry after an outage commits once", async ({ page }) => {
  const { actions } = await open(page)
  await newRecord(page, "Open a holding")
  await sheet(page).getByLabel("Holding id").fill("demo/holding/9003")
  await expect(sheet(page).getByText("Not sent yet")).toBeVisible()
  await page.keyboard.press("Escape")
  await expect(page.getByRole("dialog", { name: "Discard this draft?" })).toBeVisible()
  await shoot(page, "discard")
  await page.getByRole("button", { name: "Keep editing" }).click()
  await expect(sheet(page).getByLabel("Holding id")).toHaveValue("demo/holding/9003")
  await fillHolding(page, "demo/holding/9003", "3.000")
  actions.failNext()
  await sheet(page).getByRole("button", { name: "Open a holding" }).click()
  await expect(sheet(page).getByRole("button", { name: "Retry" })).toBeVisible()
  await shoot(page, "outage")
  await sheet(page).getByRole("button", { name: "Retry" }).click()
  await expect(sheet(page).getByRole("status")).toHaveText(/^Holding demo\/holding\/\d+ opened · just now$/)
  const [failed, retried] = actions.calls.slice(-2).map((c) => JSON.parse(c.body).idempotencyKey)
  expect(retried).toBe(failed)
  await sheet(page).getByRole("button", { name: "Another" }).click()
  await expect(sheet(page).getByLabel("Holding id")).toHaveValue("")
  await page.keyboard.press("Escape")
  await expect(sheet(page)).toHaveCount(0)
  await expect(page.getByText("41 records")).toBeVisible()
})

test("create: a parent with owned rows takes a list of records; the cell's input refusal keeps them for fixing", async ({ page }) => {
  const { actions } = await open(page)
  await newRecord(page, "Restock holding")
  const form = sheet(page)
  await form.getByLabel("Holding id").fill("demo/holding/0003")
  await form.getByRole("button", { name: "Add lot" }).click()
  await form.getByRole("button", { name: "Add lot" }).click()
  await form.getByRole("textbox", { name: "Lot", exact: true }).nth(0).fill("L-1")
  await form.getByLabel("Quantity").nth(0).fill("1.250")
  await form.getByLabel("Quantity").nth(1).fill("2,0")
  await form.getByRole("button", { name: "Restock holding" }).click()
  await expect(form.getByRole("alert").first()).toContainText("the request is invalid")
  await expect(form.locator('[aria-invalid="true"]')).toHaveCount(0)
  await expect(form.getByRole("button", { name: "Remove lot 2" })).toBeVisible()
  await shoot(page, "rows")
  await form.getByRole("textbox", { name: "Lot", exact: true }).nth(1).fill("L-2")
  await form.getByLabel("Quantity").nth(1).fill("2.000")
  await form.getByRole("button", { name: "Restock holding" }).click()
  await expect(form.getByRole("status")).toHaveText("Holding demo/holding/0003 restocked · just now")
  expect(actions.calls.at(-1)?.body).toContain('"lots":[{"lot":"L-1","quantity":"1.250"},{"lot":"L-2","quantity":"2.000"}]')
})

test("while the cell takes no actions, New and the drawer's actions say why", async ({ page }) => {
  await session(page)
  await objectCell(page, population(10))
  await page.route("**/api/host/v1/actions**", (route) => route.fulfill({ status: 404, json: { code: "route.not_found", message: "No such route." } }))
  await page.goto("/objects/canon.Holding")
  await expect(page.getByRole("button", { name: "New holding" })).toBeDisabled()
  await expect(page.getByRole("button", { name: "New holding" })).toHaveAttribute("title", "This cell doesn't take actions from Studio yet.")
  await page.getByRole("grid").locator('[role="row"]').nth(1).click()
  await expect(page.getByRole("dialog").getByLabel("Actions")).toContainText("This cell doesn't take actions from Studio yet.")
  await shoot(page, "unavailable")
})
