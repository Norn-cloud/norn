import { expect, test } from "@playwright/test"
import { SHEET } from "../src/sheet-states.ts"

// The contact sheet: every listed state has a real specimen, and each is captured for the milestone report.
for (const state of SHEET) {
  test(`sheet: ${state.id}`, async ({ page }) => {
    await page.goto(`/sheet.html?state=${encodeURIComponent(state.id)}`)
    const cell = page.locator(`[data-state="${state.id}"]`)
    await expect(cell).toBeVisible()
    await expect(cell).not.toContainText("MISSING specimen")
    await page.waitForTimeout(500)
    await cell.screenshot({ path: `test-results/sheet/${state.id.replace("/", "--")}.png` })
  })
}

test("sheet: all states on one page", async ({ page }) => {
  await page.goto("/sheet.html")
  await expect(page.locator("[data-state]")).toHaveCount(SHEET.length)
  await page.waitForTimeout(800)
  await page.screenshot({ path: "test-results/sheet/all.png", fullPage: true })
})
