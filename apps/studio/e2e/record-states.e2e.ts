import { expect, test, type Page, type Route } from "@playwright/test"
import { readFileSync } from "node:fs"
import { actionCell } from "./action-cell.ts"
import { objectCell, population, session } from "./object-cell.ts"
import { RECORD_PAGE_WIDGET } from "./record-page-widget.ts"

// Studio's record page is the cell's (#1762 C2): the admitted widget `norn.studio.record.widget`,
// drawn by `<NornAppRenderer>` inside the shell. One row per state its machine declares for the
// record read (loading, showing, refreshing, absent, failed with Retry), the workspace's own states
// before it (unavailable with Retry), a cell that serves no record page, the record's actions
// (pending, committed, refused), and the page in Arabic, right to left. SHOTS=<dir> saves the page
// light, dark and RTL.

const RECORD = "demo/holding/0001"
const URL_OF = `/sources/canon.Holding/${RECORD}`
const shots = process.env.SHOTS

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the object and action route stand-ins`) })
test.beforeEach(async ({ page }) => { await page.setViewportSize({ width: 1440, height: 1000 }) })

type Answer = "ok" | "hold" | "refused" | "absent" | "outage"

/**
 * The live cell, its canon.Holding records and actions (`objectCell`, `actionCell`), and the record
 * page's read of one holding: the object, its links and its lineage, answered as `answer` says.
 */
async function holdingCell(page: Page) {
  await session(page)
  const objects = await objectCell(page, population(4))
  const actions = await actionCell(page, objects)
  const state = { answer: "ok" as Answer, release: (): void => {}, reads: 0 }
  await page.route(/\/api\/host\/v1\/objects\/canon\.Holding\/[^:?]+(\?.*)?$/, async (route: Route) => {
    const path = decodeURIComponent(new URL(route.request().url()).pathname.split("/v1/objects/canon.Holding/")[1] ?? "")
    const leaf = ["links", "lineage", "findings", "history"].find((name) => path.endsWith(`/${name}`))
    const id = leaf === undefined ? path : path.slice(0, -leaf.length - 1)
    if (leaf === "findings" || leaf === "history" || leaf === "lineage") return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
    if (leaf === "links") return route.fulfill({ json: { objectType: "canon.Holding", id, items: [], next: null } })
    state.reads += 1
    if (state.answer === "hold") await new Promise<void>((resolve) => { state.release = resolve })
    if (state.answer === "refused") return route.fulfill({ status: 403, json: { format: "norn/refusal@2", code: "authz.denied", category: "permission", retryable: false, reason: null, message: "the read request is not authorized", explanation: "" } })
    if (state.answer === "outage") return route.fulfill({ status: 500, body: "upstream reset" })
    const row = objects.rows.find((candidate) => candidate.id === id)
    if (state.answer === "absent" || row === undefined) return route.fulfill({ status: 404, body: "" })
    const values = Object.entries(row.values).map(([key, value]) => `${JSON.stringify(key)}:${/^-?\d+(\.\d+)?$/.test(value) ? value : JSON.stringify(value)}`).join(",")
    return route.fulfill({ status: 200, contentType: "application/json", body: `{"objectType":"canon.Holding","id":${JSON.stringify(id)},"scope":"org_demo/analytics","version":${row.version ?? 1},"values":{${values}}}` })
  })
  return { objects, actions, state }
}

const pageOf = (page: Page) => page.locator('[data-norn-page="record"]')
const heading = (page: Page) => page.getByRole("heading", { level: 1, name: RECORD })

async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/record-page-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test("loading: the first read shows the page's skeleton in the shell, then the record", async ({ page }) => {
  const { state } = await holdingCell(page)
  state.answer = "hold"
  await page.goto(URL_OF)
  await expect(pageOf(page)).toHaveAttribute("data-state", "loading")
  await expect(page.locator('[data-island="record-reading"]').getByRole("status", { name: "Reading the record" })).toBeVisible()
  await expect(heading(page)).toHaveCount(0)
  state.answer = "ok"
  state.release()
  await expect(pageOf(page)).toHaveAttribute("data-state", "showing")
  await expect(heading(page)).toBeVisible()
  // The page is inside Studio's shell: its rail and menus are there beside it.
  await expect(page.locator('[data-slot="topbar"]')).toBeVisible()
})

test("ok: the record shows with its facts; Read again keeps it on screen while the cell answers", async ({ page }) => {
  const { state } = await holdingCell(page)
  await page.goto(URL_OF)
  await expect(pageOf(page)).toHaveAttribute("data-state", "showing")
  await expect(page.locator('[data-fact="resource_code"]')).toBeVisible()
  await expect(page.getByRole("region", { name: "Record", exact: true })).toBeVisible()
  // The page's catalog islands are content, not shell islands: no island edge or depth around them.
  for (const entry of ["record-header", "record-body"]) {
    await expect(page.locator(`[data-island="${entry}"]`)).toHaveCSS("border-top-width", "0px")
    await expect(page.locator(`[data-island="${entry}"]`)).toHaveCSS("box-shadow", "none")
  }
  await shoot(page, "showing")
  state.answer = "hold"
  await page.getByRole("button", { name: "Read again" }).click()
  await expect(pageOf(page)).toHaveAttribute("data-state", "refreshing")
  await expect(heading(page)).toBeVisible()
  await expect(page.getByRole("button", { name: "Read again" })).toBeDisabled()
  state.answer = "ok"
  state.release()
  await expect(pageOf(page)).toHaveAttribute("data-state", "showing")
  await expect(page.getByRole("button", { name: "Read again" })).toBeEnabled()
  expect(state.reads).toBe(2)
})

test("refused: the cell's refusal shows in its own words with Retry, which reads again", async ({ page }) => {
  const { state } = await holdingCell(page)
  state.answer = "refused"
  await page.goto(URL_OF)
  await expect(pageOf(page)).toHaveAttribute("data-state", "failed")
  const failed = page.locator('[data-island="record-failed"]')
  await expect(failed).toContainText("The record could not be read")
  await expect(failed).toContainText("the read request is not authorized")
  await expect(failed.locator('[data-record-read="refused"]')).toBeVisible()
  await shoot(page, "refused")
  state.answer = "ok"
  await failed.getByRole("button", { name: "Retry" }).click()
  await expect(pageOf(page)).toHaveAttribute("data-state", "showing")
  await expect(heading(page)).toBeVisible()
})

test("unavailable: a cell that did not answer the read says so with Retry; Retry reads again and shows the record", async ({ page }) => {
  const { state } = await holdingCell(page)
  state.answer = "outage"
  await page.goto(URL_OF)
  await expect(pageOf(page)).toHaveAttribute("data-state", "failed")
  await expect(page.locator('[data-record-read="unavailable"]')).toBeVisible()
  state.answer = "ok"
  await page.locator('[data-island="record-failed"]').getByRole("button", { name: "Retry" }).click()
  await expect(heading(page)).toBeVisible()
  expect(state.reads).toBe(2)
})

test("not served: a record the cell does not serve is said to be not available, never an error", async ({ page }) => {
  const { state } = await holdingCell(page)
  state.answer = "absent"
  await page.goto(URL_OF)
  await expect(pageOf(page)).toHaveAttribute("data-state", "absent")
  await expect(page.getByText("This record is not available: it does not exist, or you may not read it.")).toBeVisible()
  await expect(page.getByRole("button", { name: "Retry" })).toHaveCount(0)
})

test("workspace unavailable: the shell's Workspace unavailable with its one Retry, before the page reads anything", async ({ page }) => {
  const { state } = await holdingCell(page)
  let failing = true
  const definitions = JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown
  await page.route("**/api/host/v1/definitions**", (route) => failing
    ? route.fulfill({ status: 503, json: { code: "cell.unopenable", message: "The workspace cell could not be opened.", retryable: false } })
    : route.fulfill({ json: { artifacts: definitions } }))
  await page.goto(URL_OF)
  const unavailable = page.locator("[data-workspace-unavailable]")
  await expect(unavailable).toHaveCount(1)
  await expect(unavailable).toContainText("Workspace unavailable")
  await expect(pageOf(page)).toHaveCount(0)
  expect(state.reads).toBe(0)
  await shoot(page, "workspace-unavailable")
  failing = false
  await unavailable.getByRole("button", { name: "Retry" }).click()
  await expect(heading(page)).toBeVisible()
  await expect(page.locator("[data-workspace-unavailable]")).toHaveCount(0)
})

test("page not served: a cell whose definitions carry no record page says so plainly", async ({ page }) => {
  await holdingCell(page)
  const definitions = (JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as { artifactId: string }[])
    .filter(({ artifactId }) => artifactId !== RECORD_PAGE_WIDGET.artifactId)
  await page.route("**/api/host/v1/definitions**", (route) => route.fulfill({ json: { artifacts: definitions } }))
  await page.goto(URL_OF)
  await expect(page.locator('[data-record-page="not-served"]')).toContainText("serves no record page")
  await expect(pageOf(page)).toHaveCount(0)
})

test("actions: pending locks the form, accepted shows the receipt and reads the record again, refused shows the cell's refusal", async ({ page }) => {
  const { actions, objects, state } = await holdingCell(page)
  await page.goto(URL_OF)
  await expect(heading(page)).toBeVisible()
  const sheet = page.getByRole("dialog")
  await page.getByRole("button", { name: "Adjust quantity" }).click()
  await sheet.getByLabel("Counted quantity").fill("40.000")
  await sheet.getByLabel("Reason").fill("Recount")
  // Pending: the call is out and the form is locked until it answers.
  const release = actions.holdNext()
  await sheet.getByRole("button", { name: "Adjust quantity" }).click()
  await expect(sheet.getByRole("button", { name: "Sending…" })).toBeDisabled()
  await shoot(page, "action-pending")
  release()
  // Accepted: the receipt; Done reads the record again through the page machine.
  await expect(sheet.getByRole("status")).toHaveText(`Holding ${RECORD} is now counted · just now`)
  const before = state.reads
  await sheet.getByRole("button", { name: "Done" }).click()
  await expect.poll(() => state.reads).toBeGreaterThan(before)
  await expect(pageOf(page)).toHaveAttribute("data-state", "showing")
  await expect(page.locator('[data-fact="quantity"]')).toContainText("40")
  // Refused: a retired holding fails the transition's precondition, and the form says the cell's words.
  const at = objects.rows.findIndex((row) => row.id === RECORD)
  objects.rows[at] = { ...objects.rows[at]!, values: { ...objects.rows[at]!.values, retired: "true" } }
  await page.getByRole("button", { name: "Adjust quantity" }).click()
  await sheet.getByLabel("Counted quantity").fill("1.000")
  await sheet.getByLabel("Reason").fill("Late count")
  await sheet.getByRole("button", { name: "Adjust quantity" }).click()
  await expect(sheet.getByRole("alert").first()).toContainText("A condition for this action does not hold")
})

test("RTL: in Arabic the page draws right to left, its region and skeleton named in Arabic", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("norn.studio.locale", "ar"))
  const { state } = await holdingCell(page)
  state.answer = "hold"
  await page.goto(URL_OF)
  await expect(pageOf(page)).toHaveAttribute("dir", "rtl")
  await expect(pageOf(page)).toHaveAttribute("lang", "ar")
  await expect(page.getByRole("status", { name: "جارٍ قراءة السجل" })).toBeVisible()
  state.answer = "ok"
  state.release()
  await expect(page.getByRole("region", { name: "السجل", exact: true })).toBeVisible()
  await expect(heading(page)).toBeVisible()
  // The page mirrors: the header's actions sit to the left of the title.
  const title = await heading(page).boundingBox()
  const again = await page.getByRole("button", { name: "Read again" }).boundingBox()
  expect(again!.x).toBeLessThan(title!.x)
  if (shots) {
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/record-page-rtl-light.png`, fullPage: true })
  }
})
