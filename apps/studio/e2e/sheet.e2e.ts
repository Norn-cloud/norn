import { expect, test, type Locator } from "@playwright/test"
import { SHEET } from "../src/sheet-states.ts"

/** Every canvas in the cell is shown (it fades in once framed; a transparent canvas still counts as visible). */
const shown = async (cell: Locator) => { for (const canvas of await cell.locator("[data-canvas-surface]").all()) await expect(canvas).toHaveCSS("opacity", "1") }

// The contact sheet: every listed state has a real specimen, and each is captured, light and dark, for the milestone report.
for (const [scheme, suffix] of [["light", ""], ["dark", ".dark"]] as const) for (const state of SHEET) {
  test(`sheet${suffix}: ${state.id}`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" })
    await page.goto(`/sheet.html?state=${encodeURIComponent(state.id)}`)
    const cell = page.locator(`[data-state="${state.id}"]`)
    await expect(cell).toBeVisible()
    await expect(cell).not.toContainText("MISSING specimen")
    await shown(cell)
    await page.waitForTimeout(500)
    await cell.screenshot({ path: `test-results/sheet/${state.id.replace("/", "--")}${suffix}.png` })
  })
}

for (const [scheme, suffix] of [["light", ""], ["dark", ".dark"]] as const) test(`sheet${suffix}: all states on one page`, async ({ page }) => {
  await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" })
  await page.goto("/sheet.html")
  await expect(page.locator("[data-state]")).toHaveCount(SHEET.length)
  await shown(page.locator("main"))
  await page.waitForTimeout(800)
  // The sheet element, not a full-page capture: Chrome can refuse a very tall page capture under load.
  await page.locator("main").screenshot({ path: `test-results/sheet/all${suffix}.png` })
})

// The active path's flow dash moves only when motion is allowed; under reduced motion it is a still accent line.
for (const [motion, name] of [["no-preference", "canvas-flow"], ["reduce", "none"]] as const) test(`sheet: active path animation with motion ${motion}`, async ({ page }) => {
  await page.emulateMedia({ reducedMotion: motion })
  await page.goto("/sheet.html?state=automation%2Froute-selected")
  const path = page.locator(".react-flow__edge.canvas-edge-active path.react-flow__edge-path").first()
  await path.waitFor({ state: "attached" })
  await expect.poll(() => path.evaluate((element) => getComputedStyle(element).animationName)).toBe(name)
  await page.locator('[data-state="automation/route-selected"]').screenshot({ path: `test-results/sheet/motion-${motion}.png`, animations: "allow" })
})

// Labels at 75% browser zoom: a 1680px screen at 75% is a 2240px CSS viewport drawn at 0.75 device pixels per CSS pixel.
test.describe("at 75% zoom", () => {
  test.use({ viewport: { width: 2240, height: 1400 }, deviceScaleFactor: 0.75 })
  for (const id of ["frame/default", "automation/route-selected", "decision/path"]) test(`sheet 75%: ${id}`, async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" })
    await page.goto(`/sheet.html?state=${encodeURIComponent(id)}`)
    const cell = page.locator(`[data-state="${id}"]`)
    await expect(cell).toBeVisible()
    await shown(cell)
    await page.waitForTimeout(500)
    await cell.screenshot({ path: `test-results/sheet/zoom75-${id.replace("/", "--")}.png` })
  })
})

// An exit label never runs under a card: each label's box is clear of every node at the default view.
test("sheet: exit labels stay in the gap before the next card", async ({ page }) => {
  await page.goto("/sheet.html?state=automation%2Froute-selected")
  await expect(page.locator("[data-exit-label]").first()).toBeVisible()
  const [labels, nodes] = await Promise.all([".react-flow__edgelabel-renderer [data-exit-label]", ".react-flow__node"].map((selector) =>
    page.locator(selector).evaluateAll((all) => all.map((one) => one.getBoundingClientRect().toJSON() as DOMRect))))
  for (const label of labels!) for (const node of nodes!) {
    const overlaps = label.x < node.x + node.width - 0.5 && label.x + label.width > node.x + 0.5 && label.y < node.y + node.height - 0.5 && label.y + label.height > node.y + 0.5
    expect(overlaps).toBe(false)
  }
})

// Sibling exits of one step read differently at a glance: distinct text, none cut short, at the default view.
test("sheet: sibling exit labels are distinct and drawn whole", async ({ page }) => {
  await page.goto("/sheet.html?state=automation%2Froute-selected")
  await expect(page.locator("[data-exit-label]").first()).toBeVisible()
  const labels = await page.locator("[data-exit-label]").evaluateAll((all) => all.map((one) => ({ source: one.getAttribute("data-exit-source"), text: one.textContent ?? "", cut: one.scrollWidth > one.clientWidth + 1 })))
  for (const source of new Set(labels.map(({ source }) => source))) {
    const siblings = labels.filter((label) => label.source === source)
    // A lone exit may be cut at the 200px cap (in full on hover); siblings must be told apart, so they are drawn whole.
    if (siblings.length < 2) continue
    expect(new Set(siblings.map(({ text }) => text)).size).toBe(siblings.length)
    for (const { text, cut } of siblings) expect(cut, `"${text}" is cut short`).toBe(false)
  }
})

// Lifecycle step labels never overlap one another or a state at the default view, however many steps share a lane.
test("sheet: lifecycle step labels are clear of each other and of every state", async ({ page }) => {
  await page.goto("/sheet.html?state=frame%2Fdefault")
  // Every label is in, bundles included (a bundle draws once its sources are measured), before the one read.
  await expect(page.locator('[data-edge-label^="bundle:"]').first()).toBeVisible()
  const total = () => page.locator("[data-edge-label]").count()
  await expect.poll(async () => { const before = await total(); await page.waitForTimeout(100); return before === await total() && before > 1 }).toBe(true)
  const boxes = (selector: string) => page.locator(selector).evaluateAll((all) => all.map((one) => ({ id: one.getAttribute("data-edge-label") ?? one.getAttribute("data-id") ?? "", ...one.getBoundingClientRect().toJSON() as DOMRect })))
  const [labels, nodes] = await Promise.all([boxes("[data-edge-label]"), boxes(".react-flow__node")])
  const overlap = (a: DOMRect, b: DOMRect) => a.x < b.x + b.width - 0.5 && a.x + a.width > b.x + 0.5 && a.y < b.y + b.height - 0.5 && a.y + a.height > b.y + 0.5
  labels.forEach((label, index) => {
    for (const other of labels.slice(index + 1)) expect(overlap(label, other), `${label.id} over ${other.id}`).toBe(false)
    for (const state of nodes) expect(overlap(label, state), `${label.id} over ${state.id}`).toBe(false)
  })
})
