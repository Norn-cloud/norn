import { expect, test, type Page } from "@playwright/test"
import { actionCell } from "./action-cell.ts"
import { objectCell, population, session } from "./object-cell.ts"

// One test per row of the wizard's verb matrix (see the PR). "Open a holding" declares two
// x-norn.groups, so it is a wizard: What is held, Where it is, More, then Review.
// SHOTS=<dir> saves every state in light and dark for review.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400) // past the 200ms colour transitions
    await page.screenshot({ path: `${shots}/wizard-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the object and action route stand-ins`) })

async function start(page: Page) {
  await session(page)
  const objects = await objectCell(page, population(20))
  const actions = await actionCell(page, objects)
  await page.goto("/objects/canon.Holding")
  await expect(page.getByText("20 records", { exact: true })).toBeVisible()
  await page.getByRole("button", { name: "New holding" }).click()
  await page.getByRole("menuitem", { name: "Open a holding" }).click()
  return { objects, actions }
}

/** The action sheet (the Discard dialog is only open while a test answers it at page level). */
const sheet = (page: Page) => page.getByRole("dialog")
const steps = (page: Page) => sheet(page).getByRole("navigation", { name: "Steps" }).getByRole("button")

test("view: one step per group, Back keeps what was typed, steps not reached are closed, the review shows every value with Edit", async ({ page }) => {
  await start(page)
  const form = sheet(page)
  // A compact stepper: "Step 1 of 4 · What is held" over one bar segment per step.
  await expect(form.getByText("Step 1 of 4")).toBeVisible()
  await expect(form.getByText("What is held", { exact: true }).first()).toBeVisible()
  const names = ["What is held", "Where it is", "More", "Review"]
  for (const [i, name] of names.entries()) await expect(steps(page).nth(i)).toHaveAccessibleName(name)
  await expect(steps(page).nth(0)).toHaveAttribute("aria-current", "step")
  await expect(steps(page).nth(2)).toHaveAttribute("aria-disabled", "true")
  await steps(page).nth(2).click({ force: true })
  await expect(steps(page).nth(0)).toHaveAttribute("aria-current", "step") // a step not reached stays closed
  // Focus (or hover) names a step; one not reached yet says so.
  await steps(page).nth(3).focus()
  await expect(page.getByRole("tooltip").filter({ hasText: /^Review \(not reached yet\)$/ })).not.toHaveCount(0)
  await expect(form.getByLabel("Location › Site")).toHaveCount(0) // one step at a time
  await form.getByLabel("Holding id").fill("demo/holding/7001")
  await form.getByLabel("Quantity").fill("4.250")
  await form.getByLabel("Valuation method").selectOption("fifo")
  await form.getByRole("button", { name: "Next" }).click()
  await expect(steps(page).nth(1)).toHaveAttribute("aria-current", "step")
  await expect(form.getByText("Step 2 of 4")).toBeVisible()
  await expect(form.getByLabel("Location › Site")).toBeFocused() // each step starts in its first field
  await steps(page).nth(0).focus()
  await expect(page.getByRole("tooltip").filter({ hasText: /^What is held$/ })).not.toHaveCount(0)
  await form.getByLabel("Location › Site").fill("vault-b")
  await shoot(page, "step")
  await form.getByRole("button", { name: "Back" }).click()
  await expect(form.getByLabel("Quantity")).toHaveValue("4.250")
  await form.getByRole("button", { name: "Next" }).click()
  await expect(form.getByLabel("Location › Site")).toHaveValue("vault-b")
  await form.getByRole("button", { name: "Next" }).click()
  await form.getByLabel("No value").last().check() // note: an explicit no value
  await form.locator('button[type="submit"]', { hasText: "Review" }).click()
  const review = form.getByRole("region", { name: "Review" })
  await expect(review).toContainText("4.250 g")
  await expect(review).toContainText("vault-b")
  await expect(review.locator("dd").filter({ hasText: "Not given" })).not.toHaveCount(0) // resource code was left out
  await expect(review).toContainText("No value")
  await shoot(page, "review")
  await review.getByRole("button", { name: "Edit What is held" }).click()
  await expect(steps(page).nth(0)).toHaveAttribute("aria-current", "step")
  await expect(form.getByLabel("Holding id")).toHaveValue("demo/holding/7001")
  // Every step has been reached, so any can be opened directly.
  await steps(page).nth(3).click()
  await expect(form.getByRole("region", { name: "Review" })).toBeVisible()
})

test("validate: the cell checks on submit; its input refusal names no field, so the wizard stays on the review", async ({ page }) => {
  await start(page)
  const form = sheet(page)
  for (let i = 0; i < 3; i += 1) await form.locator('button[type="submit"]').click() // Next, Next, Review
  await form.getByRole("button", { name: "Open a holding" }).click()
  await expect(form.getByRole("alert").first()).toContainText("The input doesn't fit this action")
  await expect(steps(page).nth(3)).toHaveAttribute("aria-current", "step")
  await expect(steps(page).nth(0)).not.toHaveAccessibleName(/problem/)
  await expect(form.locator('[aria-invalid="true"]')).toHaveCount(0)
  await shoot(page, "refused")
})

test("reopen: Keep draft closes and restores the draft later in this session; Start over clears it; a commit forgets it", async ({ page }) => {
  const { actions } = await start(page)
  const form = sheet(page)
  await form.getByLabel("Holding id").fill("demo/holding/7002")
  await form.getByLabel("Quantity").fill("9.000")
  await page.keyboard.press("Escape")
  await page.getByRole("button", { name: "Keep draft" }).click()
  await expect(page.getByRole("dialog")).toHaveCount(0)
  await page.getByRole("button", { name: "New holding" }).click()
  await page.getByRole("menuitem", { name: "Open a holding" }).click()
  await expect(form.getByText("Restored your draft from earlier in this session.")).toBeVisible()
  await expect(form.getByLabel("Holding id")).toHaveValue("demo/holding/7002")
  await shoot(page, "restored")
  // A restored draft is still unsent work: closing asks again.
  await page.keyboard.press("Escape")
  await expect(page.getByRole("dialog", { name: "Discard this draft?" })).toBeVisible()
  await page.getByRole("button", { name: "Keep editing" }).click()
  await form.getByRole("button", { name: "Start over" }).click()
  await expect(form.getByLabel("Holding id")).toHaveValue("")
  await form.getByLabel("Holding id").fill("demo/holding/7003")
  await form.getByLabel("Resource code").fill("R-7003")
  await form.getByLabel("Quantity").fill("1.000")
  await form.getByLabel("Valuation method").selectOption("fifo")
  await form.getByRole("button", { name: "Next" }).click()
  await form.getByLabel("Location › Site").fill("vault-c")
  await page.keyboard.press("Escape")
  await page.getByRole("button", { name: "Keep draft" }).click()
  await page.getByRole("button", { name: "New holding" }).click()
  await page.getByRole("menuitem", { name: "Open a holding" }).click()
  await steps(page).nth(3).click()
  await form.getByRole("button", { name: "Open a holding" }).click()
  await expect(form.getByRole("status")).toHaveText("Holding demo/holding/7003 opened · just now")
  expect(actions.calls).toHaveLength(1)
  await form.getByRole("button", { name: "Done" }).click()
  await page.getByRole("button", { name: "New holding" }).click()
  await page.getByRole("menuitem", { name: "Open a holding" }).click()
  await expect(form.getByText("Restored your draft")).toHaveCount(0)
  await expect(form.getByLabel("Holding id")).toHaveValue("")
})

test("edit: from a record's drawer the wizard carries its id, locked, through to the review", async ({ page }) => {
  await session(page)
  const objects = await objectCell(page, population(20))
  await actionCell(page, objects)
  await page.goto("/objects/canon.Holding")
  await page.getByRole("grid").locator('[role="row"]').filter({ hasText: "demo/holding/0005" }).click()
  await page.getByRole("button", { name: "Open a holding (edit)" }).click()
  const form = sheet(page)
  await expect(form.getByLabel("Holding id")).toHaveValue("demo/holding/0005")
  await expect(form.getByLabel("Holding id")).toHaveAttribute("readonly", "")
  await form.getByLabel("Resource code").fill("R-0005")
  await form.getByLabel("Quantity").fill("185.035")
  await form.getByLabel("Valuation method").selectOption("moving_average")
  await form.getByRole("button", { name: "Next" }).click()
  await form.getByLabel("Location › Site").fill("vault-a")
  await form.getByRole("button", { name: "Next" }).click()
  await form.locator('button[type="submit"]', { hasText: "Review" }).click()
  await expect(form.getByRole("region", { name: "Review" }).getByLabel("This record")).toBeVisible()
  await form.getByRole("button", { name: "Open a holding" }).click()
  await expect(form.getByRole("status")).toHaveText("Holding demo/holding/0005 opened · just now")
})
