import { expect, test } from "@playwright/test"
import { SHEET } from "../src/sheet-states.ts"

// The contact sheet: every listed state has a real specimen, and each is captured, light and dark, for the milestone report.
for (const [scheme, suffix] of [["light", ""], ["dark", ".dark"]] as const) for (const state of SHEET) {
  test(`sheet${suffix}: ${state.id}`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" })
    await page.goto(`/sheet.html?state=${encodeURIComponent(state.id)}`)
    const cell = page.locator(`[data-state="${state.id}"]`)
    await expect(cell).toBeVisible()
    await expect(cell).not.toContainText("MISSING specimen")
    await page.waitForTimeout(500)
    await cell.screenshot({ path: `test-results/sheet/${state.id.replace("/", "--")}${suffix}.png` })
  })
}

for (const [scheme, suffix] of [["light", ""], ["dark", ".dark"]] as const) test(`sheet${suffix}: all states on one page`, async ({ page }) => {
  await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" })
  await page.goto("/sheet.html")
  await expect(page.locator("[data-state]")).toHaveCount(SHEET.length)
  await page.waitForTimeout(800)
  // The sheet element, not a full-page capture: Chrome can refuse a very tall page capture under load.
  await page.locator("main").screenshot({ path: `test-results/sheet/all${suffix}.png` })
})

// The active path's flow dash moves only when motion is allowed; under reduced motion it is a still accent line.
for (const [motion, name] of [["no-preference", "canvas-flow"], ["reduce", "none"]] as const) test(`sheet: active path animation with motion ${motion}`, async ({ page }) => {
  await page.emulateMedia({ reducedMotion: motion })
  await page.goto("/sheet.html?state=automation%2Froute-selected")
  const path = page.locator(".canvas-edge-active .react-flow__edge-path").first()
  await expect(path).toBeVisible()
  expect(await path.evaluate((element) => getComputedStyle(element).animationName)).toBe(name)
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
    await page.waitForTimeout(500)
    await cell.screenshot({ path: `test-results/sheet/zoom75-${id.replace("/", "--")}.png` })
  })
})
