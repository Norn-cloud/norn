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

test("views: create 'Applications needing help' in Operations Space, run row & bulk actions, edit, rename, duplicate, delete and undo", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await workspaceCell(page)

  // 1. Open Workspace -> Views -> New view.
  await page.goto("/views")
  await expect(page.getByRole("heading", { name: "Views" })).toBeVisible()
  await page.getByRole("button", { name: "New view" }).first().click()
  await expect(page).toHaveURL(/\/views\/new$/)

  // 2. Configure issue #1424's exact view: Applications needing help.
  await page.getByLabel("View title").fill("Applications needing help")
  await page.getByLabel("Record type").selectOption("funnel.Application")
  await page.getByLabel("Description").fill("Stalled applications below score 60 ready for concierge outreach.")
  await page.getByLabel("Space", { exact: true }).selectOption("Operations")

  // Visual filter builder: stage = stalled AND score < 60.
  await page.getByRole("button", { name: "Add condition" }).click()
  await page.getByLabel("Condition 1 property").selectOption("stage")
  await page.getByLabel("Condition 1 value").selectOption("stalled")
  await page.getByRole("button", { name: "Add condition" }).click()
  await page.getByLabel("Condition 2 property").selectOption("score")
  await page.getByLabel("Condition 2 test").selectOption("lt")
  await page.getByLabel("Condition 2 value").fill("60")

  // Choose row and bulk offered action: Offer assistance.
  await page.getByLabel("Row action Offer assistance").check()
  await page.getByLabel("Bulk action Offer assistance").check()

  // Live preview shows Ada Reyes (app-1, 42) and Ben Park (app-2, 35) — NOT Cid Patel (score 88).
  await expect(page.getByText("Ada Reyes")).toBeVisible()
  await expect(page.getByText("Ben Park")).toBeVisible()
  await expect(page.getByText("Cid Patel")).toHaveCount(0)
  await shoot(page, "views-editor")

  // Save view -> lands on /views/$id, pinned into the Operations Space in the sidebar!
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page).toHaveURL(/\/views\/studio\.view\./)
  await expect(page.getByRole("heading", { name: "Applications needing help" })).toBeVisible()
  await expect(page.getByRole("navigation", { name: "Workspace" }).getByRole("link", { name: "Applications needing help" })).toBeVisible()

  // 3. List -> select one row to inspect bulk bar + inline row action button -> screenshot.
  await expect(page.getByRole("grid", { name: "Application records" })).toContainText("Ada Reyes")
  await expect(page.getByRole("grid", { name: "Application records" })).toContainText("Ben Park")
  await page.getByLabel("Select app-1").check()
  await expect(page.getByRole("toolbar", { name: "Bulk actions" })).toContainText("1 selected")
  await shoot(page, "views-detail")

  // Run bulk action on app-1 (Offer assistance) -> transitions app-1 out of stalled!
  await page.getByRole("toolbar", { name: "Bulk actions" }).getByRole("button", { name: /Offer assistance \(1\)/ }).click()
  const bulkDialog = page.getByRole("dialog", { name: "Offer assistance" })
  await expect(bulkDialog).toBeVisible()
  await bulkDialog.getByRole("button", { name: /Run on 1 record/ }).click()
  await expect(bulkDialog.getByText("1 committed")).toBeVisible()
  await bulkDialog.getByRole("button", { name: "Done" }).click()
  await expect(page.getByRole("grid", { name: "Application records" })).not.toContainText("Ada Reyes")

  // Click remaining row (app-2 / Ben Park) -> RecordDrawer -> offered action button -> ActionSheet!
  await page.getByRole("grid", { name: "Application records" }).getByText("Ben Park").click()
  const drawer = page.getByRole("dialog", { name: "app-2" })
  await expect(drawer).toBeVisible()
  await drawer.getByRole("button", { name: "Offer assistance" }).click()
  const actionSheet = page.getByRole("dialog", { name: "Offer assistance" })
  await expect(actionSheet).toBeVisible()
  await actionSheet.getByLabel("Outreach note").fill("Called applicant")
  await actionSheet.getByRole("button", { name: "Offer assistance" }).click()
  await expect(actionSheet.getByRole("status")).toContainText("app-2")
  await actionSheet.getByRole("button", { name: "Done" }).click()
  await page.keyboard.press("Escape")

  // 4. Edit, rename, duplicate, delete and undo.
  await page.getByRole("link", { name: "Edit" }).click()
  await page.getByLabel("Description").fill("Updated triage view description.")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByText("Updated triage view description.")).toBeVisible()

  await page.getByRole("button", { name: "Rename" }).click()
  await page.getByLabel("View name").fill("Stalled funnel triage")
  await page.locator('[data-slot="popover-content"]').getByRole("button", { name: "Rename" }).click()
  await expect(page.getByRole("heading", { name: "Stalled funnel triage" })).toBeVisible()

  await page.getByRole("button", { name: "Duplicate" }).click()
  await expect(page.getByRole("heading", { name: "Stalled funnel triage (copy)" })).toBeVisible()

  await page.getByRole("button", { name: "Delete" }).click()
  await expect(page.getByText(/Deleted “Stalled funnel triage \(copy\)”/)).toBeVisible()
  await page.getByRole("button", { name: "Undo" }).click()
  await expect(page.getByRole("heading", { name: "Stalled funnel triage (copy)" })).toBeVisible()
})

test("views (second domain): create and open an Equipment Fleet Tickets view in a custom Space", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await workspaceCell(page)

  await page.goto("/views/new")
  await page.getByLabel("View title").fill("Urgent fleet tickets")
  await page.getByLabel("Record type").selectOption("fleet.Ticket")
  await page.getByLabel("Space", { exact: true }).selectOption("__custom__")
  await page.getByLabel("Custom space name").fill("Fleet Ops")

  await page.getByRole("button", { name: "Add condition" }).click()
  await page.getByLabel("Condition 1 property").selectOption("status")
  await page.getByLabel("Condition 1 value").selectOption("waiting_parts")
  await page.getByRole("button", { name: "Add condition" }).click()
  await page.getByLabel("Condition 2 property").selectOption("downtime_hours")
  await page.getByLabel("Condition 2 test").selectOption("gt")
  await page.getByLabel("Condition 2 value").fill("24")
  await page.getByLabel("Row action Escalate ticket").check()

  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByRole("heading", { name: "Urgent fleet tickets" })).toBeVisible()
  // Custom Space appears in Workspace sidebar with the pinned view.
  const nav = page.getByRole("navigation", { name: "Workspace" })
  await expect(nav.getByText("Fleet Ops")).toBeVisible()
  await expect(nav.getByRole("link", { name: "Urgent fleet tickets" })).toBeVisible()

  // Inline row action button runs Escalate ticket on CRANE-07 (tkt-1).
  const grid = page.getByRole("grid", { name: "Ticket records" })
  await expect(grid).toContainText("CRANE-07")
  await expect(grid).not.toContainText("PUMP-12")
  await grid.getByRole("button", { name: "Escalate ticket" }).click()
  const sheet = page.getByRole("dialog", { name: "Escalate ticket" })
  await expect(sheet).toBeVisible()
  await sheet.getByRole("button", { name: "Escalate ticket" }).click()
  await expect(sheet.getByRole("status")).toContainText("tkt-1")
  await sheet.getByRole("button", { name: "Done" }).click()
})
