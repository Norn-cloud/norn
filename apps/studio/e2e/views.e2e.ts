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
  const install = "**/api/host/v1/definitions:install?*"
  const saving = Promise.withResolvers<void>()
  const releaseSave = Promise.withResolvers<void>()
  await page.route(install, async (route) => {
    saving.resolve()
    await releaseSave.promise
    await route.fallback()
  }, { times: 1 })
  await page.getByRole("button", { name: "Overwrite with mine" }).click()
  try {
    await saving.promise
    await expect(page.getByRole("button", { name: "Reload theirs" })).toBeDisabled()
  } finally {
    releaseSave.resolve()
  }

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
  await expect(page.getByText("Ada Reyes")).toBeVisible()
  await page.getByLabel("View title").fill("Scoped view edit")
  await expect(page.getByRole("status").filter({ hasText: "Saved view “Scoped view edit”" })).toBeVisible()

  // Let the preview and draft admission settle so Save is the first read to see the switch.
  cell.switchScope("org_other", "prod")
  await page.getByRole("button", { name: "Save", exact: true }).click()

  await expect(page.getByRole("alert")).toContainText("This workspace changed; reload to edit this view.")
  expect(cell.installCalls).toHaveLength(0)
})

test("views: an open detail stays in its source workspace until it is opened again", async ({ page }) => {
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.shared"
  const definition = {
    kind: "view" as const, version: 1, retired: false, updatedAtMs: Date.now(),
    payload: { title: "First workspace view", objectType: "funnel.Application" },
  }
  cell.defs.set(id, definition)
  await page.goto(`/views/${id}`)
  await expect(page.getByRole("heading", { name: "First workspace view" })).toBeVisible()
  await page.getByRole("grid", { name: "Application records" }).getByText("Ben Park").click()
  const drawer = page.getByRole("dialog")
  await expect(drawer).toBeVisible()
  const refocus = () => page.evaluate(() => {
    for (const visibility of ["hidden", "visible"]) {
      Object.defineProperty(document, "visibilityState", { configurable: true, value: visibility })
      window.dispatchEvent(new Event("visibilitychange"))
    }
  })
  await Promise.all([
    page.waitForResponse(/\/v1\/objects\/funnel\.Application\?/, { timeout: 5000 }),
    refocus(),
  ])
  await expect(drawer).toBeVisible()

  cell.defs.set(id, { ...definition, payload: { ...definition.payload, title: "Second workspace view" } })
  cell.switchScope("org_other", "prod")
  await refocus()
  await expect(page.getByRole("alert").filter({ hasText: "This workspace changed; reload to edit this view." })).toBeVisible()
  await expect(drawer).toHaveCount(0)
  await expect(page.getByRole("heading", { name: /workspace view/ })).toHaveCount(0)
  await shoot(page, "views-detail-workspace-changed")

  await page.reload()
  await expect(page.getByRole("heading", { name: "Second workspace view" })).toBeVisible()
})

test("views: undo restores the custom Space input and preview refusals show the cell's message", async ({ page }) => {
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.custom"
  cell.defs.set(id, {
    kind: "view" as const, version: 1, retired: false, updatedAtMs: Date.now(),
    payload: { title: "Custom Space view", objectType: "funnel.Application", space: "Claims" },
  })
  await page.route(/\/v1\/objects\/funnel\.Application\?/, (route) => route.fulfill({
    status: 422, json: { code: "query.refused", message: "The cell could not run this preview." },
  }))
  await page.goto(`/views/${id}/edit`)
  const space = page.getByLabel("Space", { exact: true })
  const custom = page.getByLabel("Custom space name")
  await expect(custom).toHaveValue("Claims")
  await space.selectOption("Operations")
  await expect(custom).toHaveCount(0)
  await page.getByRole("button", { name: "Undo", exact: true }).click()
  await expect(custom).toHaveValue("Claims")
  await expect(page.getByRole("alert").filter({ hasText: "The cell could not run this preview." })).toBeVisible()
  await expect(page.getByText("No records currently match this filter.")).toHaveCount(0)
  await shoot(page, "views-space-and-preview-refusal")
  await space.selectOption("Operations")
  await space.selectOption("__custom__")
  await custom.fill("Another Space")
  await expect(custom).toHaveValue("Another Space")
})

test("views: grid Updates preserve legacy columns and pending Undo preserves a newer deletion notice", async ({ page }) => {
  const cell = await workspaceCell(page)
  const first = "studio.view.funnel_application.first"
  const second = "studio.view.funnel_application.second"
  const columns = ["score", "stage", "score"]
  const definition = {
    kind: "view" as const, version: 1, retired: false, updatedAtMs: Date.now(),
    payload: { title: "First view", objectType: "funnel.Application", columns },
  }
  cell.defs.set(first, definition)
  cell.defs.set(second, { ...definition, payload: { ...definition.payload, title: "Second view" } })
  await page.goto("/objects/funnel.Application")
  await page.getByRole("tab", { name: "First view" }).click()
  const update = page.getByRole("button", { name: "Update view" })
  await expect(update).toBeDisabled()
  await page.getByRole("columnheader").getByRole("button", { name: "score" }).click()
  await update.click()
  await expect.poll(() => cell.defs.get(first)?.version).toBe(2)
  expect(cell.defs.get(first)?.payload.columns).toEqual(columns)
  await expect(update).toBeDisabled()
  await page.getByRole("button", { name: "Delete", exact: true }).click()
  const undo = page.getByRole("button", { name: "Undo" })
  await expect(page.getByText("Deleted “First view”.")).toBeVisible()
  const restoring = Promise.withResolvers<void>()
  const releaseRestore = Promise.withResolvers<void>()
  await page.route("**/api/host/v1/definitions:install?*", async (route) => {
    restoring.resolve()
    await releaseRestore.promise
    await route.fallback()
  }, { times: 1 })
  await undo.click()
  try {
    await restoring.promise
    await page.getByRole("tab", { name: "Second view" }).click()
    await page.getByRole("button", { name: "Delete", exact: true }).click()
    await expect(page.getByText("Deleted “Second view”.")).toBeVisible()
  } finally {
    releaseRestore.resolve()
  }
  await expect(page.getByRole("tab", { name: "First view" })).toBeVisible()
  await expect(page.getByText("Deleted “Second view”.")).toBeVisible()
  await undo.click()
  await expect(page.getByText("Restored “Second view”.")).toBeVisible()
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

const refocus = (page: Page) => page.evaluate(() => {
  for (const visibility of ["hidden", "visible"]) {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: visibility })
    window.dispatchEvent(new Event("visibilitychange"))
  }
})

const savedView = (title: string) => ({
  kind: "view" as const, version: 1, retired: false, updatedAtMs: Date.now(),
  payload: { title, objectType: "funnel.Application", columns: ["applicant_name", "stage", "score"] },
})

// #1700 B2: the drawer shows the record as the latest read serves it, and closes when that read drops it.
test("views: an open record drawer follows the latest read and closes when the record is no longer served", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.drawer"
  cell.defs.set(id, savedView("Drawer view"))
  await page.goto(`/views/${id}`)
  const grid = page.getByRole("grid", { name: "Application records" })
  await grid.getByText("Ben Park").click()
  const drawer = page.getByRole("dialog", { name: "app-2" })
  await expect(drawer).toContainText("Ben Park")

  const ben = cell.records.find((record) => record.id === "app-2")!
  ben.values.applicant_name = "Ben Parker"
  await Promise.all([page.waitForResponse(/\/v1\/objects\/funnel\.Application\?/), refocus(page)])
  await expect(drawer).toContainText("Ben Parker")
  await shoot(page, "views-drawer-follows-read")

  cell.records.splice(cell.records.indexOf(ben), 1)
  await Promise.all([page.waitForResponse(/\/v1\/objects\/funnel\.Application\?/), refocus(page)])
  await expect(grid).not.toContainText("Ben Parker")
  await expect(page.getByRole("dialog")).toHaveCount(0)
})

// #1700 B1: an action opened from a saved view commits only in the view's workspace.
test("views: an action opened from a saved view is sent to its workspace and refused after a session switch", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.act"
  cell.defs.set(id, savedView("Action view"))
  await page.goto(`/views/${id}`)
  await page.getByRole("grid", { name: "Application records" }).getByText("Ben Park").click()
  await page.getByRole("dialog", { name: "app-2" }).getByRole("button", { name: "Offer assistance" }).click()
  const sheet = page.getByRole("dialog", { name: "Offer assistance" })
  await sheet.getByLabel("Outreach note").fill("Called applicant")

  // Another tab moves the session to another workspace just before submit.
  cell.switchScope("org_other", "prod")
  await sheet.getByRole("button", { name: "Offer assistance" }).click()
  await expect(sheet.getByRole("alert").or(sheet.getByRole("status")).first()).toBeVisible()
  expect(cell.actionCalls).toHaveLength(1)
  expect(cell.actionCalls[0]).toMatchObject({ orgId: "org_demo", expectedOrg: "org_demo", workspaceChanged: true })
  expect(cell.records.find((record) => record.id === "app-2")?.values.stage).not.toBe("assisted")
  await shoot(page, "views-action-workspace-changed")
})

// #1700 S6: only a `view` definition opens as a view.
test("views: a definition of another kind never opens as a view or offers Delete", async ({ page }) => {
  const cell = await workspaceCell(page)
  const id = "ops.automation.restock"
  cell.defs.set(id, { kind: "automation", version: 1, retired: false, updatedAtMs: Date.now(), payload: { title: "Restock", objectType: "funnel.Application" } })
  await page.goto(`/views/${id}`)
  await expect(page.getByText(`No view ${id} is saved.`)).toBeVisible()
  await expect(page.getByRole("button", { name: "Delete" })).toHaveCount(0)
})

// #1700 S7: each deletion keeps its own Undo, across navigation.
test("views: two deletions keep two Undos across navigation, and undoing one leaves the other", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  cell.defs.set("studio.view.funnel_application.first", savedView("First view"))
  cell.defs.set("studio.view.funnel_application.second", savedView("Second view"))
  await page.goto("/views")
  const panel = (title: string) => page.locator('[data-slot="panel"]').filter({ has: page.getByRole("heading", { name: title, exact: true }) })
  await panel("First view").getByRole("button", { name: "Delete" }).click()
  await expect(page.getByText(/Deleted “First view”/)).toBeVisible()
  await panel("Second view").getByRole("button", { name: "Delete" }).click()
  await expect(page.getByText(/Deleted “Second view”/)).toBeVisible()
  await expect(page.getByText(/Deleted “First view”/)).toBeVisible()

  await page.getByRole("button", { name: "New view" }).first().click()
  await expect(page).toHaveURL(/\/views\/new$/)
  await page.goBack()
  await expect(page.getByText(/Deleted “First view”/)).toBeVisible()
  await expect(page.getByText(/Deleted “Second view”/)).toBeVisible()
  await shoot(page, "views-two-deletion-receipts")

  await page.getByRole("status").filter({ hasText: "Deleted “First view”" }).getByRole("button", { name: "Undo" }).click()
  await expect(page.getByText(/Deleted “First view”/)).toHaveCount(0)
  await expect(page.getByText(/Deleted “Second view”/)).toBeVisible()
  await expect(page.getByRole("heading", { name: "First view", exact: true })).toBeVisible()
})
