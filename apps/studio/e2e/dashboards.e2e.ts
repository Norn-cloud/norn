import { createHash } from "node:crypto"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { expect, test, type Page } from "@playwright/test"

/**
 * One test per row of the dashboards verb matrix (see the PR body). Locally a stand-in answers the session, the
 * question routes, the definitions routes (CELL-DO's contract with DEFS' D59 sign-off) and :evaluate; against a
 * deployment (E2E_BASE) these rows need the cell's own data and are skipped, and the deployed run covers them.
 */
const answers = join(import.meta.dirname, "../../../fixtures/demo/answers")
const fixture = (name: string) => JSON.parse(readFileSync(join(answers, name), "utf8")) as Record<string, unknown>
const question = fixture("scrap-yield.question.json")
const answerText = readFileSync(join(answers, "scrap-yield.k1.answer.json"), "utf8")
const sha = (text: string) => createHash("sha256").update(text).digest("hex")
const press = (page: Page, combo: string) => page.keyboard.press(`${process.platform === "darwin" ? "Meta" : "Control"}+${combo}`)

const catalog = {
  objectSet: "demo.Conversion",
  properties: [{ name: "occurred_at", kind: "timestamp", unit: null }, { name: "metal", kind: "enum", unit: null }],
  metrics: [{ name: "demo.yield", label: "Yield", type: "decimal" }],
  grains: ["week"], operators: ["eq"],
  example: question,
  declarations: { metrics: fixture("scrap-yield.metrics.json"), objectType: fixture("scrap-yield.object-type.json") },
}
const window = { start: "2026-07-06T00:00:00Z", end: "2026-10-26T00:00:00Z" }
const tree = {
  basis: "From the catalogue", measures: [{ id: "demo.yield", label: "Yield", detail: "recovered ÷ input", recommended: true, value: {} }],
  groupBy: [{ id: "occurred_at", label: "occurred at", detail: "time", recommended: true, value: {} }],
  grains: { occurred_at: [{ id: "week", label: "Week", detail: "weekly", recommended: true, value: {} }] },
  conditions: [{ id: "every", label: "Every record", detail: "all", recommended: true, value: { filter: [] } }],
  filterable: [], windows: [{ id: "cell", label: "This window", detail: "dates", recommended: true, value: { window } }],
}

/** The words the stand-in cell states a dashboard in (its restater): title, then each tile by number. */
const words = (dashboard: { title: string; tiles: { title: string }[] }) => [dashboard.title.trim(), ...dashboard.tiles.map((tile, index) => `${index + 1}. ${tile.title.trim() || "Untitled question"}: the tile's question`)].join("\n")

interface Kept { version: number; payload: { title: string; tiles: { title: string }[] }; retired: boolean; signoff: string | null }

/** The stand-in cell and server; `cell.installs` records every install body, `cell.evaluations` every run. */
async function stage(page: Page, options: { mismatchOnce?: boolean; wordsOnAdmit?: boolean } = {}) {
  test.skip(Boolean(process.env.E2E_BASE), "these rows use a deterministic stand-in cell; the deployed suite runs against next")
  page.on("dialog", (dialog) => void dialog.accept())
  const cell = { dashboards: new Map<string, Kept>(), installs: [] as Record<string, unknown>[], evaluations: 0, mismatchOnce: options.mismatchOnce ?? false }
  await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u_e2e", name: "E2E", email: "admin@example.com" }, organization: { id: "org_demo", name: "Demo", role: "admin" }, environmentId: "analytics" } }))
  await page.route("**/api/questions/catalog", (route) => route.fulfill({ json: catalog }))
  await page.route("**/api/questions/recommendations", (route) => route.fulfill({ json: tree }))
  await page.route("**/api/questions/check", (route) => route.fulfill({ json: { kind: "question", admitted: true, findings: [], plan: null, restatement: { text: "Yield of Conversion records, by week.", sha256: "c".repeat(64) } } }))
  await page.route("**/api/host/v1/questions:evaluate**", (route) => { cell.evaluations += 1; return route.fulfill({ status: 200, contentType: "application/json", body: answerText }) })
  await page.route("**/api/host/v1/definitions**", async (route) => {
    const url = new URL(route.request().url())
    // Like the cell: no scope, no answer.
    if (url.searchParams.get("orgId") !== "org_demo" || url.searchParams.get("environmentId") !== "analytics") return route.fulfill({ status: 403, json: { code: "authz.denied" } })
    const body = route.request().postDataJSON() as { kind?: string; id?: string; payload?: Kept["payload"]; expectedVersion?: number; signoff?: { restatementSha256: string } } | null
    if (url.pathname.endsWith(":list")) {
      if (url.searchParams.get("kind") === "question") return route.fulfill({ json: { items: [{ id: "studio.gold_yield_weekly", title: "Gold yield, weekly", version: 2, lifecycle: "active" }], cursor: null } })
      return route.fulfill({ json: { items: [...cell.dashboards].filter(([, kept]) => !kept.retired).map(([id, kept]) => ({ id, kind: "dashboard", title: kept.payload.title, version: kept.version, lifecycle: "active", updatedAtMs: 0 })), cursor: null } })
    }
    if (url.pathname.endsWith(":admit")) {
      const title = body?.payload?.title ?? ""
      // DEFS: the check states the dashboard in the cell's own words; a cell before DEFS states none.
      const text = body?.payload ? words(body.payload) : ""
      const stated = options.wordsOnAdmit === false ? {} : { restatement: text, restatementSha256: sha(text) }
      return route.fulfill({ json: title.length > 40
        ? { admitted: false, diagnostics: [{ code: "definition.tile_title_invalid", severity: "error", message: "A title is 1 to 40 characters here.", span: { pointer: "/title" } }], ...stated }
        : { admitted: true, diagnostics: [], ...stated } })
    }
    if (url.pathname.endsWith(":install") && body?.id && body.payload) {
      cell.installs.push(body as Record<string, unknown>)
      const text = words(body.payload)
      if (cell.mismatchOnce) {
        cell.mismatchOnce = false
        const theirs = `${text}\n(as the cell reads it)`
        return route.fulfill({ status: 422, json: { code: "definition.signoff_mismatch", restatement: theirs, restatementSha256: sha(theirs) } })
      }
      if (body.signoff?.restatementSha256 !== sha(text)) return route.fulfill({ status: 422, json: { code: "definition.signoff_mismatch", restatement: text, restatementSha256: sha(text) } })
      const kept = cell.dashboards.get(body.id)
      if ((kept?.version ?? undefined) !== body.expectedVersion) return route.fulfill({ status: 409, json: { code: "definition.version_conflict" } })
      const version = (kept?.version ?? 0) + 1
      cell.dashboards.set(body.id, { version, payload: body.payload, retired: false, signoff: body.signoff.restatementSha256 })
      return route.fulfill({ json: { installed: true, id: body.id, version, receipt: { receiptHash: `r${version}`, commitSeq: version, recordedAtMs: 0 } } })
    }
    const retire = /definitions\/(.+):retire$/.exec(url.pathname)
    if (retire?.[1]) {
      const id = decodeURIComponent(retire[1]); const kept = cell.dashboards.get(id)
      if (!kept || kept.version !== body?.expectedVersion) return route.fulfill({ status: 409, json: { code: "definition.version_conflict" } })
      kept.version += 1; kept.retired = true
      return route.fulfill({ json: { installed: true, id, version: kept.version, receipt: { receiptHash: "retired", commitSeq: kept.version, recordedAtMs: 0 } } })
    }
    const get = /definitions\/([^:]+)$/.exec(url.pathname)
    if (get?.[1]) {
      const id = decodeURIComponent(get[1])
      if (id === "studio.gold_yield_weekly") return route.fulfill({ json: { id, kind: "question", version: 2, lifecycle: "active", payload: question, signoff: { restatementSha256: "c".repeat(64) } } })
      const kept = cell.dashboards.get(id)
      return kept && !kept.retired
        ? route.fulfill({ json: { id, kind: "dashboard", version: kept.version, lifecycle: "active", payload: kept.payload, signoff: kept.signoff ? { restatementSha256: kept.signoff } : null } })
        : route.fulfill({ status: 404, json: { code: "definition.not_found" } })
    }
    return route.fulfill({ status: 404, json: {} })
  })
  return cell
}

const tile = (page: Page, n: number) => page.getByTestId(`tile-${n}`)

/** From the new-dashboard page: a titled dashboard with one saved question and one built through the tree. */
async function build(page: Page, title = "Refinery weekly") {
  await page.goto("/dashboards/new")
  await page.getByPlaceholder("e.g. Refinery weekly").fill(title)
  await page.getByRole("button", { name: "Add question" }).click()
  const sheet = page.getByRole("dialog")
  await sheet.getByTestId("saved-questions").getByRole("button", { name: "Add" }).click()
  await expect(tile(page, 1)).toContainText("Gold yield, weekly")
  await page.getByRole("button", { name: "Add question" }).click()
  await sheet.getByRole("button", { name: "Build one" }).click()
  await expect(sheet.getByTestId("restatement")).toContainText("Yield of Conversion records")
  await sheet.getByRole("button", { name: "This is what I asked for" }).click()
  await sheet.getByLabel("Tile title").fill("Yield by week")
  await sheet.getByRole("button", { name: "Add to dashboard" }).click()
  await expect(tile(page, 2)).toContainText("Yield by week")
}

const signAndSave = async (page: Page) => {
  await expect(page.getByTestId("dashboard-restatement")).toBeVisible()
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await expect(page.getByText("Signed off: this is the dashboard you mean")).toBeVisible()
  await page.getByRole("button", { name: /^Save/ }).click()
}

test("dashboard: create, add a saved question and a built one, validate, sign off, save, reopen", async ({ page }) => {
  const cell = await stage(page)
  await build(page)
  // validate: the cell's finding is on the title, and sign-off waits for the cell to admit the dashboard.
  await page.getByPlaceholder("e.g. Refinery weekly").fill("A dashboard title far too long for this cell to keep")
  await expect(page.getByText("A title is 1 to 40 characters here.").first()).toBeVisible()
  await expect(page.getByRole("button", { name: "This is what I asked for" })).toBeDisabled()
  await page.getByPlaceholder("e.g. Refinery weekly").fill("Refinery weekly")
  // Save waits for the sign-off, and says why.
  await expect(page.getByRole("button", { name: /^Save/ })).toBeDisabled()
  await expect(page.getByRole("button", { name: /^Save/ })).toHaveAccessibleName(/Sign off on the dashboard in plain words first/)
  await expect(page.getByTestId("dashboard-restatement")).toHaveText("Refinery weekly\n1. Gold yield, weekly: the tile's question\n2. Yield by week: the tile's question")
  await signAndSave(page)
  await expect(page.getByText("Saved · version 1")).toBeVisible()
  // The install carried the scope (checked by the stand-in) and the sign-off of exactly the words shown.
  expect(cell.installs[0]).toMatchObject({ kind: "dashboard", id: "studio.dashboard.refinery_weekly", signoff: { restatementSha256: sha("Refinery weekly\n1. Gold yield, weekly: the tile's question\n2. Yield by week: the tile's question") } })
  // reopen: the saved dashboard comes back from the cell, tiles in order.
  await page.goto("/dashboards")
  await page.getByRole("link", { name: "Open" }).click()
  await expect(page.getByRole("heading", { name: "Refinery weekly" })).toBeVisible()
  await expect(tile(page, 1)).toContainText("Gold yield, weekly")
  await expect(tile(page, 2)).toContainText("Yield by week")
})

test("dashboard: rename and edits clear the sign-off; undo and redo; a new version saves over the one opened", async ({ page }) => {
  const cell = await stage(page)
  await build(page)
  await signAndSave(page)
  await expect(page.getByText("Saved · version 1")).toBeVisible()
  // rename: new words, so the sign-off is gone until signed again.
  await page.getByPlaceholder("e.g. Refinery weekly").fill("Refinery overview")
  await expect(page.getByTestId("dashboard-restatement")).toContainText("Refinery overview")
  await expect(page.getByText("Signed off: this is the dashboard you mean")).toHaveCount(0)
  // undo puts the old title back, redo the new one.
  await page.getByRole("button", { name: "Undo" }).click()
  await expect(page.getByPlaceholder("e.g. Refinery weekly")).toHaveValue("Refinery weekly")
  await page.getByRole("button", { name: "Redo" }).click()
  await expect(page.getByPlaceholder("e.g. Refinery weekly")).toHaveValue("Refinery overview")
  await signAndSave(page)
  await expect(page.getByText("Saved · version 2")).toBeVisible()
  // The id stays the one first saved; the new version names the version it replaced.
  expect(cell.installs.at(-1)).toMatchObject({ id: "studio.dashboard.refinery_weekly", expectedVersion: 1, payload: { title: "Refinery overview" } })
})

test("tile: reorder, resize, retitle, change view, duplicate, remove, undo", async ({ page }) => {
  await stage(page)
  await build(page)
  // reorder
  await page.getByRole("button", { name: "Move tile 1 down" }).click()
  await expect(tile(page, 1)).toContainText("Yield by week")
  await expect(tile(page, 2)).toContainText("Gold yield, weekly")
  // resize: half width and tall
  await page.getByLabel("Tile 1 width").selectOption("1")
  await page.getByLabel("Tile 1 height").selectOption("2")
  await expect(tile(page, 1)).toHaveClass(/lg:row-span-2/)
  await expect(tile(page, 1)).not.toHaveClass(/lg:col-span-2/)
  // retitle and view
  await page.getByLabel("Tile 1 title").fill("Weekly yield")
  await expect(tile(page, 1)).toContainText("Weekly yield")
  await page.getByLabel("Tile 1 view").selectOption("table")
  // duplicate, then remove the copy
  await page.getByRole("button", { name: "Duplicate tile 1" }).click()
  await expect(page.getByTestId(/^tile-\d+$/)).toHaveCount(3)
  await page.getByRole("button", { name: "Remove tile 2" }).click()
  await expect(page.getByTestId(/^tile-\d+$/)).toHaveCount(2)
  // undo brings the removed tile back; ⌘Z does the same as the button.
  await press(page, "z")
  await expect(page.getByTestId(/^tile-\d+$/)).toHaveCount(3)
  await page.getByRole("button", { name: "Undo" }).click()
  await expect(page.getByTestId(/^tile-\d+$/)).toHaveCount(2)
  // In plain words the tiles follow the new order and titles.
  await expect(page.getByTestId("dashboard-restatement")).toHaveText("Refinery weekly\n1. Weekly yield: the tile's question\n2. Gold yield, weekly: the tile's question")
})

test("dashboard: Run all runs every tile live through :evaluate", async ({ page }) => {
  const cell = await stage(page)
  await build(page)
  await page.getByRole("button", { name: "Run all" }).click()
  await expect.poll(() => cell.evaluations).toBe(2)
  for (const n of [1, 2]) await expect(tile(page, n)).toContainText("Sources:")
  // A tile runs again on its own as well.
  await tile(page, 1).getByRole("button", { name: "Run again" }).click()
  await expect.poll(() => cell.evaluations).toBe(3)
})

test("dashboard list: duplicate opens an unsaved copy to sign and save; delete, then undo", async ({ page }) => {
  const cell = await stage(page)
  await build(page)
  await signAndSave(page)
  await expect(page.getByText("Saved · version 1")).toBeVisible()
  await page.goto("/dashboards")
  // duplicate: a new dashboard in the editor, unsaved until signed off.
  await page.getByRole("button", { name: "Duplicate" }).click()
  await expect(page.getByPlaceholder("e.g. Refinery weekly")).toHaveValue("Refinery weekly (copy)")
  await expect(page.getByText("Unsaved changes")).toBeVisible()
  await signAndSave(page)
  await expect(page.getByText("Saved · version 1")).toBeVisible()
  expect(cell.installs.at(-1)).toMatchObject({ id: "studio.dashboard.refinery_weekly_copy" })
  // delete, then undo: the same payload under the sign-off the cell recorded.
  await page.goto("/dashboards")
  await expect(page.getByRole("link", { name: "Open" })).toHaveCount(2)
  await page.getByRole("button", { name: "Delete" }).first().click()
  await expect(page.getByRole("link", { name: "Open" })).toHaveCount(1)
  await page.getByRole("button", { name: "Undo" }).click()
  await expect(page.getByRole("link", { name: "Open" })).toHaveCount(2)
  expect(cell.installs.at(-1)).toMatchObject({ expectedVersion: 2 })
})

test("sign-off: when the cell reads the dashboard differently it shows its own words, and Save waits for a new sign-off", async ({ page }) => {
  const cell = await stage(page, { mismatchOnce: true })
  await build(page)
  await signAndSave(page)
  await expect(page.getByText("The cell reads this dashboard differently. Review its words and sign off again.")).toBeVisible()
  await expect(page.getByText("The cell reads your dashboard as:")).toBeVisible()
  await expect(page.getByTestId("dashboard-restatement")).toContainText("(as the cell reads it)")
  await expect(page.getByRole("button", { name: /^Save/ })).toBeDisabled()
  expect(cell.installs).toHaveLength(1)
})

test("tile: editing its question opens the tree on its picks; sign off and the tile updates in place", async ({ page }) => {
  await stage(page)
  await build(page)
  await tile(page, 2).getByRole("button", { name: "Edit question" }).click()
  const sheet = page.getByRole("dialog")
  await expect(sheet.getByTestId("question-tree")).toBeVisible()
  // No other editor: the form builder is not reachable from a dashboard.
  await expect(sheet.getByRole("button", { name: "Use this question" })).toHaveCount(0)
  await expect(sheet.getByLabel("Tile title")).toHaveValue("Yield by week")
  await sheet.getByLabel("Tile title").fill("Weekly yield, all metals")
  await expect(sheet.getByRole("button", { name: "Update tile" })).toBeDisabled()
  await sheet.getByRole("button", { name: "This is what I asked for" }).click()
  await sheet.getByRole("button", { name: "Update tile" }).click()
  await expect(tile(page, 2)).toContainText("Weekly yield, all metals")
  await expect(page.getByTestId(/^tile-\d+$/)).toHaveCount(2)
  // One undo puts the tile's question back.
  await page.getByRole("button", { name: "Undo" }).click()
  await expect(tile(page, 2)).toContainText("Yield by week")
})

test("sign-off, before the cell states words on its check: Save asks the cell for its words, then they are signed and kept", async ({ page }) => {
  const cell = await stage(page, { wordsOnAdmit: false })
  await build(page)
  await expect(page.getByText("This cell does not state dashboards in plain words on its check yet.")).toBeVisible()
  await page.getByRole("button", { name: /^Save/ }).click()
  // The cell refused the placeholder digest and stated its own words; nothing was kept.
  await expect(page.getByText("The cell reads your dashboard as:")).toBeVisible()
  expect(cell.dashboards.size).toBe(0)
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await page.getByRole("button", { name: /^Save/ }).click()
  await expect(page.getByText("Saved · version 1")).toBeVisible()
  expect(cell.installs.at(-1)).toMatchObject({ signoff: { restatementSha256: sha("Refinery weekly\n1. Gold yield, weekly: the tile's question\n2. Yield by week: the tile's question") } })
})
