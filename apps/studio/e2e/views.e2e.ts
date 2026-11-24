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

test("views: create 'Applications needing help' in Operations Space, keep column order, act through record drawer, edit, rename, duplicate, delete and undo", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await workspaceCell(page)

  // 1. Open Workspace -> Views -> New view.
  await page.goto("/views")
  await expect(page.getByRole("heading", { name: "Views" })).toBeVisible()
  await page.getByRole("button", { name: "New view" }).first().click()
  await expect(page).toHaveURL(/\/views\/new$/)

  // 2. Configure issue #1424's view: Applications needing help.
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

  // Move `score` ahead of `stage` to verify saved column order.
  await page.getByRole("button", { name: "Move column score up" }).click()

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

  // 3. List -> saved column order puts `score` before `stage`, and row actions run through RecordDrawer's per-record /v1/actions read.
  const grid = page.getByRole("grid", { name: "Application records" })
  await expect(grid).toContainText("Ada Reyes")
  await expect(grid).toContainText("Ben Park")
  const headers = await grid.getByRole("columnheader").allInnerTexts()
  expect(headers.indexOf("score")).toBeGreaterThan(-1)
  expect(headers.indexOf("stage")).toBeGreaterThan(headers.indexOf("score"))
  await shoot(page, "views-detail")

  // Click row (app-2 / Ben Park) -> RecordDrawer -> offered action button -> ActionSheet!
  await grid.getByText("Ben Park").click()
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
  await expect(grid).not.toContainText("Ben Park")
  await expect(grid).toContainText("Ada Reyes")
  await shoot(page, "views-after-action")

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

test("views: Overwrite with mine sends the confirmed current revision as expectedVersion", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.cas"
  cell.defs.set(id, {
    kind: "view",
    version: 1,
    retired: false,
    updatedAtMs: Date.now(),
    payload: {
      title: "CAS view",
      space: "Operations",
      objectType: "funnel.Application",
      columns: ["applicant_name", "stage", "score"],
    },
  })

  await page.goto(`/views/${id}/edit`)
  await expect(page.getByLabel("View title")).toHaveValue("CAS view")

  // Another operator advances the definition to version 2 before we save.
  cell.defs.set(id, {
    kind: "view",
    version: 2,
    retired: false,
    updatedAtMs: Date.now(),
    payload: {
      title: "CAS view (remote edit)",
      space: "Operations",
      objectType: "funnel.Application",
      columns: ["applicant_name", "stage", "score"],
    },
  })

  await page.getByLabel("Description").fill("My local edit")
  await page.getByRole("button", { name: "Save", exact: true }).click()

  await expect(page.getByRole("alert")).toContainText("Someone saved version 2 of this view since you opened it.")
  await page.getByRole("button", { name: "Overwrite with mine" }).click()

  await expect(page).toHaveURL(new RegExp(`/views/${id}$`))
  await expect(page.getByText("My local edit")).toBeVisible()
  expect(cell.installCalls.map((call) => call.expectedVersion)).toEqual([1, 2])
})

test("views: refuses to write when the active workspace changed after loading the draft", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.scoped"
  cell.defs.set(id, {
    kind: "view",
    version: 1,
    retired: false,
    updatedAtMs: Date.now(),
    payload: {
      title: "Scoped view",
      space: "Operations",
      objectType: "funnel.Application",
      columns: ["applicant_name", "stage", "score"],
    },
  })

  await page.goto(`/views/${id}/edit`)
  await expect(page.getByLabel("View title")).toHaveValue("Scoped view")

  // Switch active workspace in the server session (as if switched in another tab).
  cell.switchScope("org_other", "prod")
  await page.getByLabel("Description").fill("Should not write to org_other")
  await page.getByRole("button", { name: "Save", exact: true }).click()

  await expect(page.getByRole("alert")).toContainText("This workspace changed; reload to edit this view.")
  expect(cell.installCalls).toHaveLength(0)
})

test("views: Narrow by discards counts when the active workspace changes mid-read", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.scoped_facets"
  cell.defs.set(id, {
    kind: "view",
    version: 1,
    retired: false,
    updatedAtMs: Date.now(),
    payload: { title: "Scoped facets", objectType: "funnel.Application" },
  })

  const facetURL = "**/api/host/v1/objects/funnel.Application:facets?*"
  let releaseCounts: () => void = () => {}
  const delayed = new Promise<void>((resolve) => { releaseCounts = resolve })
  await page.route(facetURL, async (route) => {
    await delayed
    await route.fulfill({
      json: {
        rows: 1, withheldRows: 0, lowerBound: false,
        facets: [{ property: "stage", values: [{ value: "late workspace value", count: 1 }], otherRows: 0, nulls: 0 }],
      },
    })
  })
  try {
    const [request] = await Promise.all([page.waitForRequest(facetURL), page.goto(`/views/${id}`)])
    const { searchParams } = new URL(request.url())
    expect(searchParams.get("orgId")).toBe("org_demo")
    expect(searchParams.get("environmentId")).toBe("prod")
    cell.switchScope("org_other", "prod")
  } finally {
    releaseCounts()
  }

  const rail = page.locator("aside").filter({ hasText: "This workspace changed; reload to edit this view." })
  await expect(rail.getByRole("alert")).toBeVisible()
  await expect(page.getByText("late workspace value")).toHaveCount(0)
  await shoot(page, "views-facets-workspace-changed")
})

test("views: conflict after deletion offers Save as new view and Discard my changes without a dead Reload button", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.deleted_conflict"
  cell.defs.set(id, {
    kind: "view",
    version: 1,
    retired: false,
    updatedAtMs: Date.now(),
    payload: {
      title: "Conflict after delete",
      space: "Operations",
      objectType: "funnel.Application",
      columns: ["applicant_name", "stage", "score"],
    },
  })

  await page.goto(`/views/${id}/edit`)
  await expect(page.getByLabel("View title")).toHaveValue("Conflict after delete")

  // Another operator deletes (retires) the view while we edit it.
  cell.defs.set(id, {
    kind: "view",
    version: 2,
    retired: true,
    updatedAtMs: Date.now(),
    payload: {
      title: "Conflict after delete",
      space: "Operations",
      objectType: "funnel.Application",
      columns: ["applicant_name", "stage", "score"],
    },
  })

  await page.getByLabel("Description").fill("Edited before noticing delete")
  await page.getByRole("button", { name: "Save", exact: true }).click()

  await expect(page.getByRole("alert")).toContainText("This view was deleted by someone else.")
  await expect(page.getByRole("button", { name: "Reload theirs" })).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Overwrite with mine" })).toHaveCount(0)

  // Choice 1: Save as new view creates a fresh view preserving the draft.
  await page.getByRole("button", { name: "Save as new view" }).click()
  await expect(page).toHaveURL(/\/views\/studio\.view\./)
  await expect(page.getByText("Edited before noticing delete")).toBeVisible()

  // Choice 2: Discard my changes drops the draft and closes the editor to /views.
  const id2 = "studio.view.funnel_application.deleted_discard"
  cell.defs.set(id2, {
    kind: "view",
    version: 1,
    retired: false,
    updatedAtMs: Date.now(),
    payload: {
      title: "Discard after delete",
      space: "Operations",
      objectType: "funnel.Application",
      columns: ["applicant_name", "stage", "score"],
    },
  })
  await page.goto(`/views/${id2}/edit`)
  await expect(page.getByLabel("View title")).toHaveValue("Discard after delete")
  cell.defs.set(id2, {
    kind: "view",
    version: 2,
    retired: true,
    updatedAtMs: Date.now(),
    payload: {
      title: "Discard after delete",
      space: "Operations",
      objectType: "funnel.Application",
      columns: ["applicant_name", "stage", "score"],
    },
  })
  await page.getByLabel("Description").fill("Discardable edit")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByRole("alert")).toContainText("This view was deleted by someone else.")
  await page.getByRole("button", { name: "Discard my changes" }).click()
  await expect(page).toHaveURL(/\/views$/)
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

  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByRole("heading", { name: "Urgent fleet tickets" })).toBeVisible()
  // Custom Space appears in Workspace sidebar with the pinned view.
  const nav = page.getByRole("navigation", { name: "Workspace" })
  await expect(nav.getByText("Fleet Ops")).toBeVisible()
  await expect(nav.getByRole("link", { name: "Urgent fleet tickets" })).toBeVisible()

  // Open CRANE-07 (tkt-1) in the record drawer and run Escalate ticket via /v1/actions.
  const grid = page.getByRole("grid", { name: "Ticket records" })
  await expect(grid).toContainText("CRANE-07")
  await expect(grid).not.toContainText("PUMP-12")
  await grid.getByText("CRANE-07").click()
  const drawer = page.getByRole("dialog", { name: "tkt-1" })
  await expect(drawer).toBeVisible()
  await drawer.getByRole("button", { name: "Escalate ticket" }).click()
  const sheet = page.getByRole("dialog", { name: "Escalate ticket" })
  await expect(sheet).toBeVisible()
  await sheet.getByRole("button", { name: "Escalate ticket" }).click()
  await expect(sheet.getByRole("status")).toContainText("tkt-1")
  await sheet.getByRole("button", { name: "Done" }).click()
})
