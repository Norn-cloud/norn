import { readdirSync, readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { open as openCanvas } from "./canvas.ts"
import { importTriageCell } from "./import-triage-cell.ts"
import { session } from "./object-cell.ts"

// SCREENSHOTS ONLY (restyle 2, #1705): every builder screen and the top bar in light and dark, on a
// desktop and at 390px, for review. Runs only with SHOTS=<dir>; it asserts only that each page drew.

const shots = process.env.SHOTS
const root = new URL("../../../", import.meta.url)
const read = (path: string) => JSON.parse(readFileSync(new URL(path, root), "utf8")) as unknown
const base = read("fixtures/demo/explorer/three-systems.definitions.json") as { artifactId: string }[]
const seen = new Set(base.map(({ artifactId }) => artifactId))
const scans = readdirSync(new URL("fixtures/packs/scans/artifacts/", root)).filter((name) => name.endsWith(".json"))
  .map((name) => read(`fixtures/packs/scans/artifacts/${name}`) as { artifactId: string })
  .filter(({ artifactId }) => !seen.has(artifactId))
const transforms = JSON.parse(readFileSync(new URL("../test/fixtures/transforms.json", import.meta.url), "utf8")) as { readonly catalog: unknown; readonly plan: unknown; readonly rows: unknown }

const inbox = {
  scanLimit: 500,
  sections: [{ kind: "held_back", count: 3, shown: 1, approximate: false }, { kind: "decision", count: 0, shown: 0, approximate: false }, { kind: "source_error", count: 0, shown: 0, approximate: false }],
  items: [{ kind: "held_back", ruleRef: { kind: "law", artifactId: "canon.ItemizedDocument.total_matches_lines.law", lawId: "canon.ItemizedDocument.total_matches_lines", version: "sha256:1" }, title: "Totals match lines", rationale: null, objectType: "canon.OrderLine", count: 3, approximate: false, target: "/objects/canon.OrderLine?population=held_back", records: [] }],
}

async function cell(page: Page) {
  await session(page)
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: [...base, ...scans] } })
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: inbox })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
}

async function transformCell(page: Page) {
  await session(page)
  await page.route("**/api/host/v1/definitions**", (route) => route.fulfill({ json: transforms.catalog }))
  await page.route("**/api/host/v1/transforms/**", (route) => {
    const path = new URL(route.request().url()).pathname
    return route.fulfill({ json: path.endsWith("/plan") ? transforms.plan : transforms.rows })
  })
}

const VIEWPORTS = [{ name: "desktop", width: 1440, height: 900 }, { name: "390", width: 390, height: 844 }] as const

/** Shoot the current page in light and dark, at both widths; `ready` waits for the page to draw. */
async function shoot(page: Page, name: string, go: () => Promise<void>, ready: () => Promise<void>) {
  for (const viewport of VIEWPORTS) {
    await page.setViewportSize(viewport)
    await go()
    await ready()
    for (const scheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme: scheme })
      await page.waitForTimeout(400)
      await page.screenshot({ path: `${shots}/${name}-${viewport.name}-${scheme}.png`, fullPage: viewport.name === "390" })
    }
    await page.emulateMedia({ colorScheme: "light" })
  }
}

test.beforeEach(({ }, info) => { test.skip(!shots || Boolean(process.env.E2E_BASE), `${info.title}: screenshots only (SHOTS=<dir>)`) })

const heading = (page: Page) => () => expect(page.locator("main h1, h1").first()).toBeVisible({ timeout: 20_000 })

test("shots: top bar, semantic map, rules, sources, a type page and workflows", async ({ page }) => {
  await cell(page)
  await shoot(page, "top-bar-map", () => page.goto("/map").then(() => {}), heading(page))
  await shoot(page, "rules", () => page.goto("/rules").then(() => {}), heading(page))
  await shoot(page, "sources", () => page.goto("/sources").then(() => {}), heading(page))
  await shoot(page, "type-page", () => page.goto("/sources/canon.Holding").then(() => {}), heading(page))
  await shoot(page, "workflows", () => page.goto("/workflows").then(() => {}), heading(page))
})

test("shots: transforms", async ({ page }) => {
  await transformCell(page)
  await shoot(page, "transforms", () => page.goto("/transforms").then(() => {}), heading(page))
})

test("shots: imports (aliases, review, supersession)", async ({ page }) => {
  await importTriageCell(page)
  for (const step of ["aliases", "review", "supersession"]) {
    await shoot(page, `imports-${step}`, () => page.goto(`/imports?step=${step}`).then(() => {}), heading(page))
  }
})

test("shots: canvas, the automation decision tree and its decision preview", async ({ page }) => {
  for (const tab of [undefined, "Automation"] as const) {
    await shoot(page, tab ? "decision-tree" : "canvas", () => openCanvas(page, tab), async () => {})
  }
  await shoot(page, "decision-sheet", () => page.goto("/sheet.html?state=decision%2Fdefault").then(() => {}), async () => { await page.waitForTimeout(500) })
})
