import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

const SHOTS = process.env.CUTOVER_SHOTS

async function recorded(page: Page) {
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
}

async function capture(page: Page, name: string) {
  if (!SHOTS) return
  await page.waitForTimeout(200)
  await page.screenshot({ path: `${SHOTS}/${name}.png`, fullPage: false })
}

test("skins and floating dock: switch open, polar, luma, glass skins via ChromeDock tray, persist across reload, and switch to floating dock chrome", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: "light" })
  await recorded(page)
  await page.goto("/sources")
  await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()

  // 1. Default skin is open-canvas with sidebar chrome and Geist font family.
  await expect.poll(() => page.evaluate(() => ({
    skin: document.documentElement.dataset.skin,
    shell: document.documentElement.dataset.shell,
    font: getComputedStyle(document.documentElement).fontFamily,
  }))).toEqual({
    skin: "open",
    shell: "sidebar",
    font: expect.stringContaining("Geist Variable"),
  })
  await capture(page, "open-light")

  // 2. Open the first-party @ui-sdk/core + @ui-sdk/react ChromeDock appearance tray and verify data-state transitions.
  const trigger = page.locator('[data-slot="chrome-dock-trigger"]').first()
  await expect(trigger).toHaveAttribute("data-state", "closed")
  await trigger.click()
  await expect(trigger).toHaveAttribute("data-state", "open")
  const tray = page.locator('[data-slot="chrome-dock-tray"]')
  await expect(tray).toBeVisible()
  await expect(tray).toHaveAttribute("data-state", "open")
  await expect(tray.locator('[data-skin-option="open"]')).toHaveAttribute("data-state", "selected")

  // 3. Switch to Polar Carbon skin: deep carbon surface (#151518) and precision hairlines.
  await tray.locator('[data-skin-option="polar"]').click()
  await expect(tray.locator('[data-skin-option="polar"]')).toHaveAttribute("data-state", "selected")
  await expect.poll(() => page.evaluate(() => ({
    skin: document.documentElement.dataset.skin,
    pageBg: getComputedStyle(document.querySelector('[data-slot="sidebar-inset"]')!).backgroundColor,
    shellBg: getComputedStyle(document.querySelector('[data-slot="sidebar-wrapper"]')!).backgroundColor,
  }))).toEqual({
    skin: "polar",
    pageBg: "rgb(21, 21, 24)",
    shellBg: "rgb(12, 12, 14)",
  })
  await page.keyboard.press("Escape")
  await expect(trigger).toHaveAttribute("data-state", "closed")
  await capture(page, "polar-carbon")

  // 4. Reload verifies localStorage persistence of data-skin="polar".
  await page.reload()
  await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.skin)).toBe("polar")

  // 5. Switch to Luma skin, then Liquid Glass skin (verifying backdrop-filter blur).
  await page.locator('[data-slot="chrome-dock-trigger"]').first().click()
  const trayAfterReload = page.locator('[data-slot="chrome-dock-tray"]')
  await trayAfterReload.locator('[data-skin-option="luma"]').click()
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.skin)).toBe("luma")

  await trayAfterReload.locator('[data-skin-option="glass"]').click()
  await expect.poll(() => page.evaluate(() => ({
    skin: document.documentElement.dataset.skin,
    backdrop: getComputedStyle(document.querySelector('[data-slot="sidebar-inset"]')!).backdropFilter,
  }))).toEqual({
    skin: "glass",
    backdrop: expect.stringContaining("blur("),
  })
  await page.keyboard.press("Escape")
  await capture(page, "liquid-glass")

  // 6. Switch back to Open skin and activate Floating Dock shell chrome (zero permanent left sidebar!).
  await page.locator('[data-slot="chrome-dock-trigger"]').first().click()
  const dockSwitchTray = page.locator('[data-slot="chrome-dock-tray"]')
  await dockSwitchTray.locator('[data-skin-option="open"]').click()
  await dockSwitchTray.locator('[data-shell-option="dock"]').click()
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.shell)).toBe("dock")

  await expect(page.locator('[data-slot="sidebar"]')).toHaveCount(0)
  const dock = page.locator('[data-slot="floating-dock"]')
  await expect(dock).toBeVisible()
  await capture(page, "floating-dock")

  // 7. Navigate using the Floating Dock, then switch back to Sidebar chrome cleanly.
  await dock.getByRole("link", { name: "Home", exact: true }).click()
  await expect(page).toHaveURL(/\/$/)
  await expect(page.getByRole("heading", { name: "Home", level: 1 })).toBeVisible()

  await dock.locator('[data-slot="chrome-dock-trigger"]').click()
  await page.locator('[data-slot="chrome-dock-tray"]').locator('[data-shell-option="sidebar"]').click()
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.shell)).toBe("sidebar")
  await expect(page.locator('[data-slot="floating-dock"]')).toHaveCount(0)
  await expect(page.locator('[data-slot="sidebar"]')).toBeVisible()
})
