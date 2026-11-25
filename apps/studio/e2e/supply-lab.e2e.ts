import { expect, test, type Page } from "@playwright/test"
import { writeFileSync } from "node:fs"
import { devHost, packLabel, packWorkspace, type Workspace } from "./pack-workspace.ts"

// Exploration pass: open each generic page over the live supply-lab workspace and record what it shows.
const shots = process.env.SHOTS
async function record(page: Page, name: string) {
  if (!shots) return
  writeFileSync(`${shots}/supply-${name}.txt`, await page.locator("body").innerText().catch(() => ""))
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/supply-${name}-${scheme}.png`, fullPage: false })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.describe.configure({ mode: "serial" })
let lab: Workspace
test.beforeAll(async () => {
  test.skip(!devHost || Boolean(process.env.E2E_BASE), "needs the native dev host (NORN_DEV_HOST_BIN)")
  test.setTimeout(600_000)
  lab = await packWorkspace("supply-lab", ["clerk:hana", "planner:amal"])
})
test.afterAll(async () => { await lab?.stop() })

test("explore", async ({ page }) => {
  test.setTimeout(300_000)
  const base = lab.studio("clerk:hana")
  for (const [name, path] of [["home", "/"], ["sources", "/sources"], ["map", "/map"], ["movements", "/objects/supply.StockMovement"], ["orders", "/objects/supply.PurchaseOrder"], ["questions", "/questions"], ["dashboards", "/dashboards"], ["dashboard", "/dashboards/supply.dashboard.supply_lab"], ["decisions", "/decisions"], ["rules", "/rules"]] as const) {
    await page.goto(`${base}${path}`)
    await page.waitForTimeout(3000)
    await record(page, name)
  }
  expect(packLabel(lab.artifacts, (target) => target["kind"] === "objectType" && target["objectType"] === "supply.StockMovement")).toBe("Stock movement")
})
