import { expect, test, type Locator, type Page, type Route } from "@playwright/test"
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

/** A saved view's other verbs sit behind its "More" menu, on a list card and in the open view's header alike. */
async function viewAction(page: Page, title: string, verb: "Edit" | "Rename" | "Duplicate" | "Delete", within: Page | Locator = page) {
  await within.getByRole("button", { name: `More actions for ${title}`, exact: true }).click()
  await page.getByRole("menuitem", { name: verb, exact: true }).click()
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
  await expect(page.getByRole("navigation", { name: "Studio navigation" }).getByRole("tree", { name: "Workspace pages" }).getByRole("link", { name: "Applications needing help" })).toBeVisible()

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

  await viewAction(page, "Applications needing help", "Rename")
  const renaming = page.getByRole("dialog", { name: "Rename view" })
  await renaming.getByLabel("View name").fill("Stalled funnel triage")
  await renaming.getByRole("button", { name: "Rename" }).click()
  await expect(renaming).toHaveCount(0)
  await expect(page.getByRole("heading", { name: "Stalled funnel triage" })).toBeVisible()

  await viewAction(page, "Stalled funnel triage", "Duplicate")
  await expect(page.getByRole("heading", { name: "Stalled funnel triage (copy)" })).toBeVisible()

  await viewAction(page, "Stalled funnel triage (copy)", "Delete")
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
  const nav = page.getByRole("navigation", { name: "Studio navigation" }).getByRole("tree", { name: "Workspace pages" })
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
  await expect(page.getByRole("button", { name: /^More actions for / })).toHaveCount(0)
})

// #1700 S7: each deletion keeps its own Undo, across navigation.
test("views: two deletions keep two Undos across navigation, and undoing one leaves the other", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  cell.defs.set("studio.view.funnel_application.first", savedView("First view"))
  cell.defs.set("studio.view.funnel_application.second", savedView("Second view"))
  await page.goto("/views")
  const panel = (title: string) => page.locator('[data-slot="panel"]').filter({ has: page.getByRole("heading", { name: title, exact: true }) })
  await viewAction(page, "First view", "Delete", panel("First view"))
  await expect(page.getByText(/Deleted “First view”/)).toBeVisible()
  await viewAction(page, "Second view", "Delete", panel("Second view"))
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

// #1700 R1: a refetch refused for this reader wins over the cached page: the rows and the drawer
// go, and the next successful read does not reopen the drawer.
test("views: a refused refetch hides the cached rows and closes the drawer; the next read keeps it closed", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.refused"
  cell.defs.set(id, savedView("Refused view"))
  await page.goto(`/views/${id}`)
  const grid = page.getByRole("grid", { name: "Application records" })
  await grid.getByText("Ben Park").click()
  await expect(page.getByRole("dialog", { name: "app-2" })).toContainText("Ben Park")

  // The cell now refuses this reader (a bare 403, as a proxy or a revoked session answers).
  const list = /\/api\/host\/v1\/objects\/funnel\.Application\?/
  const refuse = (route: Route) => route.fulfill({ status: 403, contentType: "text/plain", body: "Forbidden" })
  await page.route(list, refuse)
  await Promise.all([page.waitForResponse(list), refocus(page)])
  await expect(page.getByRole("dialog")).toHaveCount(0)
  await expect(page.getByText("Ben Park")).toHaveCount(0)
  await shoot(page, "views-refused-refetch")

  await page.unroute(list, refuse)
  await Promise.all([page.waitForResponse(list), refocus(page)])
  await expect(grid.getByText("Ben Park")).toBeVisible()
  await expect(page.getByRole("dialog")).toHaveCount(0)
})

// #1700 R2: a deletion's receipt and Undo belong to the reader who pressed Delete; a role change
// while the deletion is pending leaves the new role neither.
test("views: a deletion answered after a role change leaves no receipt or Undo for the new role", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.clock.install()
  const cell = await workspaceCell(page)
  let role = "admin"
  let sessionReads = 0
  await page.route("**/api/me", (route) => {
    sessionReads += 1
    return route.fulfill({ json: { user: { id: "u_ops", name: "Operator Admin", email: "ops@example.com" }, organization: { id: "org_demo", name: "Workspace Demo", role }, environmentId: "prod" } })
  })
  let release = () => {}
  const held = new Promise<void>((resolve) => { release = resolve })
  await page.route(/\/v1\/definitions\/.+:retire/, async (route) => { await held; await route.fallback() })
  cell.defs.set("studio.view.funnel_application.held", savedView("Held view"))
  await page.goto("/views")
  const panel = page.locator('[data-slot="panel"]').filter({ has: page.getByRole("heading", { name: "Held view", exact: true }) })
  const retire = page.waitForRequest(/:retire/)
  await viewAction(page, "Held view", "Delete", panel)
  await retire

  // The same person's role drops in this workspace while the deletion is out; the session is read again.
  role = "viewer"
  const before = sessionReads
  await page.clock.setSystemTime(Date.now() + 120_000)
  await refocus(page)
  await expect.poll(() => sessionReads - before).toBeGreaterThanOrEqual(2)
  release()
  await expect(page.getByRole("heading", { name: "Held view", exact: true })).toHaveCount(0)
  await expect(page.getByText(/Deleted “Held view”/)).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Undo" })).toHaveCount(0)
  expect(cell.defs.get("studio.view.funnel_application.held")?.retired).toBe(true)
})

// #1700 R4: a view restored elsewhere after this reader deleted it opens as the cell serves it.
test("views: a view restored elsewhere drops the stale Deleted notice on the list and the view", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.restored"
  cell.defs.set(id, savedView("Restored view"))
  await page.goto(`/views/${id}`)
  await viewAction(page, "Restored view", "Delete")
  await expect(page.getByText(/Deleted “Restored view”/)).toBeVisible()

  // Someone else restores it: a newer active revision.
  const stored = cell.defs.get(id)!
  stored.retired = false
  stored.version += 1
  stored.updatedAtMs = Date.now()
  await page.getByRole("link", { name: "Back to views" }).click()
  await expect(page.getByRole("heading", { name: "Restored view", exact: true })).toBeVisible()
  await expect(page.getByText(/Deleted “Restored view”/)).toHaveCount(0)
  await page.locator('[data-slot="panel"]').filter({ has: page.getByRole("heading", { name: "Restored view", exact: true }) }).getByRole("link", { name: "Open" }).click()
  await expect(page).toHaveURL(new RegExp(`/views/${id.replaceAll(".", "\\.")}$`))
  await expect(page.getByRole("grid", { name: "Application records" })).toBeVisible()
  await expect(page.getByText(/Deleted “Restored view”/)).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Undo" })).toHaveCount(0)
})

// #1700 R7: a 200 the cell sent without a readable definition is an unavailable read, not "No view".
test("views: an unreadable definition answer says so and never shows No view", async ({ page }) => {
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.garbled"
  cell.defs.set(id, savedView("Garbled view"))
  await page.route(new RegExp(`/v1/definitions/${id.replaceAll(".", "\\.")}(\\?|$)`), (route) => route.fulfill({ status: 200, contentType: "application/json", body: "" }))
  await page.goto(`/views/${id}`)
  await expect(page.getByText("The cell's answer for this view could not be read; nothing was opened.")).toBeVisible()
  await expect(page.getByText(`No view ${id} is saved.`)).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Delete" })).toHaveCount(0)
})

// UI polish: a list card and the open view show one primary action each; Edit, Rename, Duplicate and
// Delete sit in one "More" menu, so no verb wraps onto a line of its own at phone width.
test("views: each card and the open view show one primary action and keep the rest in one More menu, which never wraps at 390px", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.menu"
  cell.defs.set(id, savedView("Menu view"))
  await page.goto("/views")
  await expect(page.getByText("Saved lists of records, filtered and sorted the way you need them, grouped by Space.")).toBeVisible()
  const card = page.locator('[data-slot="panel"]').filter({ has: page.getByRole("heading", { name: "Menu view", exact: true }) })
  await expect(card.getByRole("link", { name: "Open" })).toBeVisible()
  for (const verb of ["Edit", "Rename", "Duplicate", "Delete"]) await expect(card.getByRole("button", { name: verb, exact: true })).toHaveCount(0)
  await card.getByRole("button", { name: "More actions for Menu view" }).click()
  await expect(page.getByRole("menuitem")).toHaveText(["Edit", "Rename", "Duplicate", "Delete"])
  await shoot(page, "views-more-menu")
  await page.keyboard.press("Escape")
  await expect(page.getByRole("menuitem")).toHaveCount(0)
  // Edit from the menu opens the editor.
  await viewAction(page, "Menu view", "Edit", card)
  await expect(page).toHaveURL(new RegExp(`/views/${id.replaceAll(".", "\\.")}/edit$`))

  // The open view: Edit is the one primary action, and More sits on the same line at 390px.
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(`/views/${id}`)
  const edit = page.getByRole("link", { name: "Edit", exact: true })
  const more = page.getByRole("button", { name: "More actions for Menu view" })
  await expect(edit).toBeVisible()
  await expect(more).toBeVisible()
  const [a, b] = [await edit.boundingBox(), await more.boundingBox()]
  expect(Math.abs((a?.y ?? 0) - (b?.y ?? 100))).toBeLessThan(4)
  expect(b?.height ?? 0).toBeGreaterThanOrEqual(44)
  await more.click()
  await expect(page.getByRole("menuitem")).toHaveText(["Rename", "Duplicate", "Delete"])
  await page.keyboard.press("Escape")
})

// UI polish: the open view's rail counts the view's own choice (stage) and text (channel) columns, as the
// cell's facets route serves them, and narrowing by one value narrows the grid.
test("views: the open view's Narrow by rail lists its choice and text columns with the cell's counts, and a value narrows the grid", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const cell = await workspaceCell(page)
  const id = "studio.view.funnel_application.facets"
  cell.defs.set(id, { ...savedView("Facet view"), payload: { ...savedView("Facet view").payload, filter: { all: [{ property: "stage", op: "eq", value: "stalled" }] } } })
  await page.goto(`/views/${id}`)
  const rail = page.locator('aside [aria-label="Narrow by"]')
  await expect(rail).toContainText("3 records, counted by the cell")
  await expect(rail.getByRole("heading", { name: "stage" })).toBeVisible()
  await expect(rail.getByRole("heading", { name: "channel" })).toBeVisible()
  await expect(page.getByText("No text, choice or yes/no values to narrow by.")).toHaveCount(0)
  // A choice's own counts leave out its own choice: every stage the type holds.
  await expect(rail.getByRole("button", { name: /stalled\s*3/ })).toHaveAttribute("aria-pressed", "true")
  await expect(rail.getByRole("button", { name: /in review|in_review/ })).toBeVisible()
  await expect(rail.getByRole("button", { name: /web\s*2/ })).toBeVisible()
  await rail.getByRole("button", { name: /partner\s*1/ }).click()
  const grid = page.getByRole("grid", { name: "Application records" })
  await expect(grid).toContainText("Ben Park")
  await expect(grid).not.toContainText("Ada Reyes")
  await shoot(page, "views-facets-rail")
})
