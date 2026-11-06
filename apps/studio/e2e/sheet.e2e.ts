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
