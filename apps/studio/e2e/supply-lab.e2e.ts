import { expect, test, type Page } from "@playwright/test"
import { readFileSync } from "node:fs"
import { devHost, packLabel, packWorkspace, type Workspace } from "./pack-workspace.ts"

// The supply-lab pack (registry/packs/supply-lab) live in Studio: `norn init --template supply-lab`,
// served by the real native dev host, seeded by `norn seed` with the reference dataset its stress
// harness commits, and read through Studio's generic pages. Nothing is stubbed. Every expectation
// is read from the pack's own definitions (its artifacts, labels, policy, seed calls and bundled
// dashboard), so Studio stays domain-agnostic: no page here knows the pack.
//
// The seed's calls are sent by named local principals holding the pack's own roles
// (DEV-PRINCIPALS-1); one local Studio serves each, so the cell's Cedar policy decides what each
// may do. SHOTS=<dir> saves each state in light and dark.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400) // past the colour transitions
    await page.screenshot({ path: `${shots}/supply-lab-${name}-${scheme}.png` })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

const registry = (path: string) => JSON.parse(readFileSync(new URL(`../../../registry/${path}`, import.meta.url), "utf8"))
const template = JSON.parse(readFileSync(new URL("../../../tools/norn-ts/templates/supply-lab/template.json", import.meta.url), "utf8")) as { definitions: Array<{ kind: string; id?: string }> }

interface SeedCall { readonly actor: string; readonly action: string; readonly input: Record<string, unknown> }
const calls = registry("packs/supply-lab/seed/supply.calls.json").calls as readonly SeedCall[]
const seeded = (action: string) => calls.filter((call) => call.action === action)
/** The role-holders the rows act as: whoever the seed sends each action as. */
const CLERK = seeded("supply.movement.record")[0]!.actor
const PLANNER = seeded("supply.po.create")[0]!.actor

test.describe.configure({ mode: "serial" })
let lab: Workspace
test.beforeAll(async () => {
  test.skip(!devHost || Boolean(process.env.E2E_BASE), "needs the native dev host (NORN_DEV_HOST_BIN)")
  test.setTimeout(600_000)
  lab = await packWorkspace("supply-lab", [CLERK, PLANNER])
})
test.afterAll(async () => { await lab?.stop() })

const sheet = (page: Page) => page.getByRole("dialog")
const typeLabel = (type: string, locale: "en" | "ar" = "en") => packLabel(lab.artifacts, (target) => target["kind"] === "objectType" && target["objectType"] === type, locale)!
const action = (id: string) => [...lab.artifacts.values()].find((artifact) => artifact.kind === "action" && artifact.payload?.actionId === id)!
/** The object type an action creates, as its effect program names it. */
const createdBy = (id: string): string => action(id).payload.effectProgram.objectKind

async function open(page: Page, actor: string, path: string) {
  await page.goto(`${lab.studio(actor)}${path}`)
  await expect(page.locator("[data-workspace-state]")).toContainText("Live")
}

/** Pick the record `id` in a record picker field. */
async function pick(page: Page, field: string, id: string) {
  const trigger = sheet(page).getByRole("combobox", { name: field, exact: true })
  await trigger.click()
  const search = page.getByRole("combobox", { name: /^Search .* records$/ })
  await expect(search).toBeFocused()
  await search.fill(id)
  const list = page.getByRole("listbox", { name: / records$/ })
  await expect(list.getByRole("option").first()).toContainText(id)
  await page.keyboard.press("Enter")
  // One field at a time: the list closes and the pick shows in its own field before the next.
  await expect(list).toBeHidden()
  await expect(trigger).toContainText(id)
}

test("sources: each system the pack maps, and its object types, read live from the cell", async ({ page }) => {
  await open(page, CLERK, "/sources")
  const schemas = [...lab.artifacts.values()].filter((artifact) => artifact.kind === "source_schema")
  expect(schemas.length).toBeGreaterThan(0)
  for (const schema of schemas) await expect(page.getByText(`schema ${schema.payload.source.version}`)).toBeVisible()
  // Every pack entity is in the model, by the pack's own label.
  const entities = [...lab.artifacts.values()].filter((artifact) => artifact.kind === "entity" && artifact.namespace === "supply" && artifact.payload.objectType !== undefined)
  for (const entity of entities) await expect(page.locator("table").getByText(typeLabel(entity.payload.objectType.name), { exact: true }).first()).toBeAttached()
  await shoot(page, "sources")
})

test("records: a grid counts and lists the seeded records, as the cell counts them", async ({ page }) => {
  const type = createdBy("supply.movement.record")
  const movements = seeded("supply.movement.record")
  await open(page, CLERK, `/objects/${type}`)
  await expect(page.getByRole("heading", { name: typeLabel(type), exact: true })).toBeVisible()
  await expect(page.getByText(`${movements.length} records`, { exact: true })).toBeVisible()
  const grid = page.getByRole("grid")
  for (const call of [movements[0]!, movements.at(-1)!]) await expect(grid.getByText(String(call.input["movement_id"]), { exact: true }).first()).toBeVisible()
  await shoot(page, "records")
})

test("actions: the clerk records a movement; the cell commits it and Studio shows its receipt", async ({ page }) => {
  const id = "supply.movement.record"
  const type = createdBy(id)
  const movements = seeded(id)
  const like = movements[0]!.input
  await open(page, CLERK, `/objects/${type}`)
  await page.getByRole("button", { name: `New ${typeLabel(type).toLowerCase()}` }).click()
  const form = sheet(page)
  await expect(form.getByRole("heading", { name: action(id).payload.title })).toBeVisible()
  await form.getByLabel("Movement id").fill("MV-STUDIO-1")
  await form.getByLabel("Movement kind").selectOption({ value: String(like["movement_kind"]) })
  await form.getByLabel("Occurred at").fill(String(like["occurred_at"]))
  await form.getByLabel("Qty").fill("5")
  await form.getByLabel("Unit").selectOption({ value: String(like["unit"]) })
  await pick(page, "Sku id", String(like["sku_id"]))
  await pick(page, "Warehouse id", String(like["warehouse_id"]))
  await form.getByRole("button", { name: action(id).payload.title }).click()
  await expect(form.getByRole("status")).toContainText("MV-STUDIO-1")
  await shoot(page, "receipt")
  await page.keyboard.press("Escape")
  await expect(page.getByText(`${movements.length + 1} records`, { exact: true })).toBeVisible()
})

test("Cedar: the clerk may not draft a purchase order, so the cell refuses it and writes nothing; the planner may", async ({ page }) => {
  const id = "supply.po.create"
  const type = createdBy(id)
  // The pack's policy names a role for drafting that the clerk does not hold and the planner does.
  const rule = lab.artifacts.get("supply.access.policy").payload.rules.find((candidate: { actions: string[] }) => candidate.actions.includes(id))
  const allowed: readonly string[] = rule.allOf[0].role.anyOf
  const roles = (actor: string): readonly string[] => lab.ready["principals"].find((principal: { actor: string }) => principal.actor === actor).roles
  expect(roles(CLERK).some((role) => allowed.includes(role))).toBe(false)
  expect(roles(PLANNER).some((role) => allowed.includes(role))).toBe(true)
  const like = seeded(id)[0]!.input
  const draft = async (actor: string, poId: string) => {
    await open(page, actor, `/objects/${type}`)
    await page.getByRole("button", { name: `New ${typeLabel(type).toLowerCase()}` }).click()
    const form = sheet(page)
    await form.getByLabel("Po id").fill(poId)
    await form.getByLabel("Currency").selectOption({ value: String(like["currency"]) })
    await form.getByLabel("Promised at").fill(String(like["promised_at"]))
    await form.getByLabel("Supplier id").fill(String(like["supplier_id"]))
    await form.getByRole("button", { name: action(id).payload.title }).click()
    return form
  }
  const refused = await draft(CLERK, "PO-STUDIO-CLERK")
  await expect(refused.getByRole("alert")).toContainText("Not allowed")
  await shoot(page, "refusal")
  await page.keyboard.press("Escape")
  await expect(page.getByRole("grid").getByText("PO-STUDIO-CLERK")).toHaveCount(0)
  const committed = await draft(PLANNER, "PO-STUDIO-PLANNER")
  await expect(committed.getByRole("status")).toContainText("PO-STUDIO-PLANNER")
})

test("questions and dashboard: the bundled dashboard's tiles answer over the seeded data, and the metrics board answers live", async ({ page }) => {
  const [dashboard] = registry("packs/supply-lab/questions/supply.dashboard.json") as Array<{ title: string; tiles: Array<{ title: string }> }>
  const id = template.definitions.find((definition) => definition.kind === "dashboard")!.id!
  await open(page, CLERK, `/dashboards/${id}`)
  await expect(page.getByRole("heading", { name: dashboard!.title })).toBeVisible()
  for (const tile of dashboard!.tiles) await expect(page.getByText(tile.title, { exact: true })).toBeVisible()
  await expect(page.getByText("Up to date", { exact: true })).toHaveCount(dashboard!.tiles.length, { timeout: 30_000 })
  await shoot(page, "dashboard")
  await open(page, CLERK, "/metrics")
  await expect(page.getByText("Computed by the cell's exact engine").first()).toBeVisible({ timeout: 30_000 })
  await shoot(page, "metrics")
})

test("decisions inbox: the pack holds nothing back, and the inbox says nothing waits", async ({ page }) => {
  // The pack declares no approval or flow, so nothing parks for a decider.
  expect([...lab.artifacts.values()].filter((artifact) => artifact.namespace === "supply" && ["approval", "flow"].includes(artifact.kind))).toEqual([])
  await open(page, PLANNER, "/decisions")
  await expect(page.getByText("Nothing waits on you")).toBeVisible()
})

test("RTL: in Arabic the records grid mirrors and names the type by the pack's Arabic label", async ({ page }) => {
  const type = createdBy("supply.movement.record")
  await open(page, CLERK, `/objects/${type}`)
  await page.getByRole("button", { name: /^Language: .*Switch to العربية/ }).click()
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl")
  await expect(page.getByRole("heading", { name: typeLabel(type, "ar"), exact: true })).toBeVisible()
  await shoot(page, "rtl")
})
