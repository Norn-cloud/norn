import { expect, test, type Page } from "@playwright/test"
import { workspaceCell } from "./workspace-cell.ts"

const shots = process.env.SHOTS

async function shoot(page: Page, name: string) {
  if (!shots) return
  await page.setViewportSize({ width: 1440, height: 900 })
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(300)
    await page.screenshot({ path: `${shots}/${name}-${scheme}.png`, fullPage: false })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => {
  test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses deterministic workspace stand-in cell`)
})

test("automations: create When/If/Then rule, preview without firing, fire to Inbox, decide, inspect run history & refusal, disable, delete and undo", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)

  // 1. Open Builder -> Automations -> New automation.
  await page.goto("/automations")
  await expect(page.getByRole("heading", { name: "Automations" })).toBeVisible()
  await page.getByRole("button", { name: "New automation" }).first().click()
  await expect(page).toHaveURL(/\/automations\/new$/)

  // 2. Build issue #1424's exact automation:
  //    When stage enters stalled, If score < 60, Then propose Offer assistance to Inbox.
  await page.getByLabel("Automation title").fill("Assist stalled low-score applications")
  await page.getByLabel("Record type").selectOption("funnel.Application")
  await page.getByLabel("Description").fill("Propose concierge outreach whenever an application stalls below score 60.")

  await page.getByLabel("Lifecycle property").selectOption("stage")
  await page.getByLabel("Target lifecycle state").selectOption("stalled")

  await page.getByRole("button", { name: "Add guard condition" }).click()
  await page.getByLabel("Guard 1 property").selectOption("score")
  await page.getByLabel("Guard 1 operator").selectOption("lt")
  await page.getByLabel("Guard 1 value").fill("60")

  await page.getByLabel("Target action").selectOption("funnel.application.offer_assistance")
  await page.getByLabel("Inbox question").fill("Offer assistance to stalled application {id}?")

  // 3. Click "Test against current records": matches app-1 & app-2 WITHOUT firing or creating Inbox items!
  await page.getByRole("button", { name: "Test against current records" }).click()
  const preview = page.locator("[data-preview-results]")
  await expect(preview).toContainText("2 matches")
  await expect(preview).toContainText("app-1")
  await expect(preview).toContainText("app-2")
  await expect(preview).not.toContainText("app-3")
  expect(cell.decisions.size).toBe(0)
  await shoot(page, "automations-editor")

  // 4. Save -> lands on /automations/$id -> click "Evaluate now" -> proposals land in Inbox!
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page).toHaveURL(/\/automations\/studio\.automation\./)
  await expect(page.getByRole("heading", { name: "Assist stalled low-score applications" })).toBeVisible()

  await page.getByRole("button", { name: "Evaluate now" }).click()
  await expect(page.getByRole("status")).toContainText("2 proposed")
  await expect(page.getByRole("table", { name: "Automation run history" })).toContainText("app-1")
  await expect(page.getByRole("table", { name: "Automation run history" })).toContainText("Proposed")

  // 5. Follow "Open in Inbox" link for app-1 -> see automation proposal in Inbox -> screenshot & approve!
  await page.getByRole("row", { name: /app-1/ }).getByRole("link", { name: "Open in Inbox" }).click()
  await expect(page).toHaveURL(/\/decisions/)
  await expect(page.getByRole("dialog").getByRole("heading", { name: "Offer assistance to stalled application app-1?" })).toBeVisible()
  await shoot(page, "automations-inbox")

  // Wait for DecisionSheet engine to settle and approve!
  await expect(page.locator("norn-decision-sheet .norn-sheet")).toHaveAttribute("data-engine", "ready", { timeout: 30_000 })
  const sheet = page.getByRole("dialog")
  await sheet.getByLabel("Run Offer assistance").check()
  await sheet.getByRole("button", { name: "Decide: Run Offer assistance" }).click()
  await sheet.getByRole("button", { name: "Yes, run offer assistance" }).click()
  await expect(sheet.getByRole("status")).toContainText("Decided")

  // 6. Also create a "Run automatically" automation hitting the governing law refusal (score >= 80 -> app-3)
  //    to prove Run history clearly shows Refused outcomes with refusal code & message!
  await page.goto("/automations/new")
  await page.getByLabel("Automation title").fill("Auto-assist high-score check")
  await page.getByLabel("Record type").selectOption("funnel.Application")
  await page.getByLabel("Target lifecycle state").selectOption("stalled")
  await page.getByRole("button", { name: "Add guard condition" }).click()
  await page.getByLabel("Guard 1 property").selectOption("score")
  await page.getByLabel("Guard 1 operator").selectOption("gte")
  await page.getByLabel("Guard 1 value").fill("80")
  await page.getByRole("button", { name: "Run action automatically" }).click()
  await page.getByLabel("Target action").selectOption("funnel.application.offer_assistance")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByRole("heading", { name: "Auto-assist high-score check" })).toBeVisible()

  await page.getByRole("button", { name: "Evaluate now" }).click()
  await expect(page.getByRole("status")).toContainText("1 refused")
  const history = page.getByRole("table", { name: "Automation run history" })
  await expect(history).toContainText("Refused")
  await expect(history).toContainText("law.violated")
  await expect(history).toContainText("High-scoring applications")
  await shoot(page, "automations-detail")

  // 7. Disable, edit, delete and undo!
  await page.getByRole("button", { name: "Disable" }).click()
  await expect(page.getByText("Disabled", { exact: true })).toBeVisible()
  await expect(page.getByRole("button", { name: "Evaluate now" })).toBeDisabled()

  await page.getByRole("link", { name: "Edit" }).click()
  await page.getByLabel("Automation title").fill("Auto-assist high-score check v2")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByRole("heading", { name: "Auto-assist high-score check v2" })).toBeVisible()

  await page.getByRole("button", { name: "Delete" }).click()
  await expect(page.getByText(/Deleted “Auto-assist high-score check v2”/)).toBeVisible()
  await page.getByRole("button", { name: "Undo" }).click()
  await expect(page.getByRole("heading", { name: "Auto-assist high-score check v2" })).toBeVisible()
})

test("automations (second domain): create and preview an Equipment Fleet Tickets escalation rule", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await workspaceCell(page)

  await page.goto("/automations/new")
  await page.getByLabel("Automation title").fill("Escalate overdue parts tickets")
  await page.getByLabel("Record type").selectOption("fleet.Ticket")
  await page.getByLabel("Lifecycle property").selectOption("status")
  await page.getByLabel("Target lifecycle state").selectOption("waiting_parts")

  await page.getByRole("button", { name: "Add guard condition" }).click()
  await page.getByLabel("Guard 1 property").selectOption("downtime_hours")
  await page.getByLabel("Guard 1 operator").selectOption("gt")
  await page.getByLabel("Guard 1 value").fill("24")

  await page.getByLabel("Target action").selectOption("fleet.ticket.escalate")
  await page.getByLabel("Inbox question").fill("Escalate overdue fleet ticket {id}?")

  await page.getByRole("button", { name: "Test against current records" }).click()
  const preview = page.locator("[data-preview-results]")
  await expect(preview).toContainText("1 match")
  await expect(preview).toContainText("tkt-1")
  await expect(preview).not.toContainText("tkt-2")

  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByRole("heading", { name: "Escalate overdue parts tickets" })).toBeVisible()
  await page.getByRole("button", { name: "Evaluate now" }).click()
  await expect(page.getByRole("status")).toContainText("1 proposed")
})
