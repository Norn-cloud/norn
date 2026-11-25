import { readdirSync, readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { catalogOf } from "../src/lib/catalog.ts"
import type { Artifact } from "../src/lib/transport.ts"
import { session } from "./object-cell.ts"

// WORKFLOWS (#1705 audit): a step's condition reads in full, headings use the names the server
// serves (never a raw type id), a description's code spans render as code (no literal backticks), a
// state card with nothing else to show stays compact, and the page never scrolls sideways at 390px.

const root = new URL("../../../", import.meta.url)
const read = (path: string) => JSON.parse(readFileSync(new URL(path, root), "utf8")) as { artifactId: string }
const base = JSON.parse(readFileSync(new URL("fixtures/demo/explorer/three-systems.definitions.json", root), "utf8")) as { artifactId: string }[]
const seen = new Set(base.map(({ artifactId }) => artifactId))
// The scans pack declares the DocumentLine review lifecycle; the three systems declare the Restatement one.
const scans = readdirSync(new URL("fixtures/packs/scans/artifacts/", root)).filter((name) => name.endsWith(".json"))
  .map((name) => read(`fixtures/packs/scans/artifacts/${name}`))
  .filter(({ artifactId }) => !seen.has(artifactId))
const definitions = [...base, ...scans]

/** The heading the catalog serves for one lifecycle: the type's title and the property's label. */
const catalog = catalogOf(definitions as unknown as readonly Artifact[])
const served = (objectType: string, property: string) => {
  const concept = catalog.byName.get(objectType)
  if (concept === undefined) throw new Error(`${objectType} is not in the fixture`)
  const label = concept.fields.find(({ id }) => id === property)?.label
  return { title: concept.title, heading: `${concept.title}: ${label ?? property}` }
}

async function cell(page: Page) {
  await session(page)
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: definitions } })
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: { scanLimit: 500, sections: [], items: [] } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
}

const section = (page: Page, objectType: string, property: string) => page.locator(`section[id="workflow-${objectType}-${property}"]`)

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays a stubbed cell`) })

test("workflows: a step's condition reads in full, never ending a clause on 'is not'", async ({ page }) => {
  await cell(page)
  await page.goto("/workflows")
  const restatement = section(page, "core.analytics.Restatement", "status")
  // A step card by its exact name: "Approve" must not match the "Approved" state.
  const step = (name: string) => restatement.locator(".react-flow__node").filter({ has: page.getByText(name, { exact: true }) })
  const stage = step("Stage")
  await expect(stage).toContainText("Requires: candidate and candidate digest are not empty")
  await expect(step("Approve"))
    .toContainText("Requires: reason, slice ids, candidate and candidate digest are not empty")
  for (const text of await restatement.locator(".react-flow__node").allInnerTexts()) {
    expect(text).not.toMatch(/\bis not(\s+and\b|\s*$)/m)
  }
})

test("workflows: headings use the titles the catalog serves, not raw type ids", async ({ page }) => {
  await cell(page)
  await page.goto("/workflows")
  for (const [objectType, property] of [["core.analytics.Restatement", "status"], ["docs.DocumentLine", "review"]] as const) {
    const { heading } = served(objectType, property)
    await expect(section(page, objectType, property).locator("h2[data-workflow-heading]")).toHaveText(heading)
  }
  await expect(page.getByRole("heading", { level: 2, name: /DocumentLine/ })).toHaveCount(0)
  expect(served("docs.DocumentLine", "review").title).not.toBe("DocumentLine")
})

test("workflows: a description's code spans render as code, with no literal backticks", async ({ page }) => {
  await cell(page)
  await page.goto("/workflows")
  const line = section(page, "docs.DocumentLine", "review")
  const about = line.locator("p").first()
  await expect(about).toContainText("canon.LineItem")
  await expect(about.locator("code").filter({ hasText: "canon.LineItem" })).toBeVisible()
  for (const text of await page.locator("section[id^='workflow-'] p").allInnerTexts()) expect(text).not.toContain("`")
})

test("workflows: a state card with nothing else to show stays compact", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 })
  await cell(page)
  await page.goto("/workflows")
  const restatement = section(page, "core.analytics.Restatement", "status")
  for (const state of ["ready", "approved"]) {
    const card = restatement.locator(`.react-flow__node[data-id="${state}"]`)
    await expect(card).toBeVisible()
    const box = await card.boundingBox()
    expect(box?.height ?? Infinity).toBeLessThanOrEqual(60)
  }
})

test("workflows: no horizontal page scroll at 390px", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await cell(page)
  await page.goto("/workflows")
  await expect(section(page, "docs.DocumentLine", "review").locator("h2[data-workflow-heading]")).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})
