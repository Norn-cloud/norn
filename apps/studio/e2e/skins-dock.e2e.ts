import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"
import { recordCell } from "./record-cell.ts"

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

const rgb = (value: string): [number, number, number] => {
  const match = value.match(/rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)/)
  if (!match) throw new Error(`unexpected color: ${value}`)
  return [Number(match[1]), Number(match[2]), Number(match[3])]
}

const luminance = ([r, g, b]: [number, number, number]) => {
  const lin = [r, g, b].map((v) => v / 255).map((c) => c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)
  return 0.2126 * lin[0]! + 0.7152 * lin[1]! + 0.0722 * lin[2]!
}

const contrastRatio = (a: [number, number, number], b: [number, number, number]) => {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (hi! + 0.05) / (lo! + 0.05)
}

test("skins and floating dock: switch luma, open, polar, glass skins via ChromeDock tray, persist across reload, and verify v6 dock AccountMenu, overflow destinations, and bottom clearance", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: "light" })
  await recorded(page)
  await page.goto("/")
  await expect(page.getByRole("heading", { name: "Home", level: 1 })).toBeVisible()

  // 1. Default skin is Luma with the three-island chrome and Geist font family (owner decision, 2026-11-25).
  await expect.poll(() => page.evaluate(() => ({
    skin: document.documentElement.dataset.skin,
    shell: document.documentElement.dataset.shell,
    font: getComputedStyle(document.documentElement).fontFamily,
  }))).toEqual({
    skin: "luma",
    shell: "sidebar",
    font: expect.stringContaining("Geist Variable"),
  })
  await capture(page, "luma-light-home")

  await page.goto("/sources")
  await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
  await capture(page, "luma-light-sources")

  // 2. Open the first-party @ui-sdk/core + @ui-sdk/react ChromeDock appearance tray and verify data-state transitions.
  const trigger = page.locator('[data-slot="chrome-dock-trigger"]').first()
  await expect(trigger).toHaveAttribute("data-state", "closed")
  await trigger.click()
  await expect(trigger).toHaveAttribute("data-state", "open")
  const tray = page.locator('[data-slot="chrome-dock-tray"][data-state="open"]')
  await expect(tray).toBeVisible()
  await expect(tray.locator('[data-skin-option="luma"]')).toHaveAttribute("data-state", "selected")

  // 3. Verify Luma: the main island is opaque white (no glass), and the CTA is near-black (chrome is monochrome, DESIGN.md 19.6).
  await expect.poll(() => page.evaluate(() => ({
    skin: document.documentElement.dataset.skin,
    pageBg: getComputedStyle(document.querySelector('[data-slot="sidebar-inset"]')!).backgroundColor,
    backdrop: getComputedStyle(document.querySelector('[data-slot="sidebar-inset"]')!).backdropFilter,
    primaryBg: getComputedStyle(document.querySelector('[data-slot="button"][data-variant="default"]')!).backgroundColor,
  }))).toEqual({
    skin: "luma",
    pageBg: "rgb(255, 255, 255)",
    backdrop: "none",
    primaryBg: "rgb(24, 24, 24)",
  })

  // Switch to Open skin and verify persistence across reload.
  await tray.locator('[data-skin-option="open"]').click()
  await expect(tray.locator('[data-skin-option="open"]')).toHaveAttribute("data-state", "selected")
  await page.keyboard.press("Escape")
  await expect(trigger).toHaveAttribute("data-state", "closed")

  // 4. Reload verifies localStorage persistence of data-skin="open".
  await page.reload()
  await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.skin)).toBe("open")

  // 5. Switch to Polar (opaque monochrome islands, near-black CTA, no blur), then Liquid Glass (a real backdrop-filter blur).
  await page.locator('[data-slot="chrome-dock-trigger"]').first().click()
  const trayAfterReload = page.locator('[data-slot="chrome-dock-tray"][data-state="open"]')
  await trayAfterReload.locator('[data-skin-option="polar"]').click()
  await expect.poll(() => page.evaluate(() => ({
    skin: document.documentElement.dataset.skin,
    pageBg: getComputedStyle(document.querySelector('[data-slot="sidebar-inset"]')!).backgroundColor,
    backdrop: getComputedStyle(document.querySelector('[data-slot="sidebar-inset"]')!).backdropFilter,
    primaryBg: getComputedStyle(document.querySelector('[data-slot="button"][data-variant="default"]')!).backgroundColor,
  }))).toEqual({ skin: "polar", pageBg: "rgb(246, 246, 248)", backdrop: "none", primaryBg: "rgb(24, 24, 27)" })

  await trayAfterReload.locator('[data-skin-option="glass"]').click()
  await expect.poll(() => page.evaluate(() => ({
    skin: document.documentElement.dataset.skin,
    backdrop: getComputedStyle(document.querySelector('[data-slot="sidebar-inset"]')!).backdropFilter,
  }))).toEqual({
    skin: "glass",
    backdrop: expect.stringContaining("blur("),
  })
  await page.keyboard.press("Escape")

  // 6. Switch back to Open skin and activate v6 Floating Dock shell chrome (zero permanent left sidebar!).
  await page.locator('[data-slot="chrome-dock-trigger"]').first().click()
  const dockSwitchTray = page.locator('[data-slot="chrome-dock-tray"][data-state="open"]')
  await dockSwitchTray.locator('[data-skin-option="open"]').click()
  await dockSwitchTray.locator('[data-shell-option="dock"]').click()
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.shell)).toBe("dock")
  await page.keyboard.press("Escape")

  await expect(page.locator('[data-slot="sidebar"]')).toHaveCount(0)
  const dock = page.locator('[data-slot="floating-dock"]')
  await expect(dock).toBeVisible()

  // Verify desktop main content keeps full clearance (>= 108px at 90% root font-size) above the floating dock.
  const bottomPadding = await page.locator('[data-slot="sidebar-inset"] > div > div').last().evaluate((el) => Number.parseFloat(getComputedStyle(el).paddingBottom))
  expect(bottomPadding).toBeGreaterThanOrEqual(108)
  await capture(page, "floating-dock-sources")

  // 7. Verify AccountMenu (sign-out, view, language) is reachable in dock mode via the minimal v6 topbar.
  await page.locator('[data-slot="dock-topbar"]').getByRole("button", { name: "Account" }).click()
  await expect(page.getByRole("menuitem", { name: "Sign out" })).toBeVisible()
  await expect(page.getByRole("menuitemradio", { name: "Business" })).toBeVisible()
  await expect(page.getByRole("menuitemradio", { name: "Technical" })).toBeVisible()
  await page.keyboard.press("Escape")

  // 8. Verify overflow destinations (>5 in Builder: Connections) are reachable via More destinations in dock mode.
  await dock.getByRole("button", { name: "More destinations" }).click()
  const connectionsItem = page.getByRole("menuitem", { name: "Connections" })
  await expect(connectionsItem).toBeVisible()
  await connectionsItem.click()
  await expect(page).toHaveURL(/\/connections$/)

  // 9. Navigate via primary icon-only dock link to Home, capture floating-dock-home, verify side="top" tray, and switch back to Sidebar.
  await dock.getByRole("link", { name: "Home", exact: true }).click()
  await expect(page).toHaveURL(/\/$/)
  await expect(page.getByRole("heading", { name: "Home", level: 1 })).toBeVisible()
  await capture(page, "floating-dock-home")

  const dockAppearanceTrigger = dock.locator('[data-slot="chrome-dock-trigger"]')
  await dockAppearanceTrigger.click()
  const dockTray = page.locator('[data-slot="chrome-dock-tray"][data-state="open"]')
  await expect(dockTray).toBeVisible()
  const triggerBox = await dockAppearanceTrigger.boundingBox()
  const trayBox = await dockTray.boundingBox()
  expect(triggerBox && trayBox).toBeTruthy()
  expect(trayBox!.y + trayBox!.height).toBeLessThanOrEqual(triggerBox!.y + 4)

  await dockTray.locator('[data-shell-option="sidebar"]').click()
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.shell)).toBe("sidebar")
  await expect(page.locator('[data-slot="floating-dock"]')).toHaveCount(0)
  await expect(page.locator('[data-slot="sidebar"]')).toBeVisible()
})

test("dark mode cascade: luma, polar monochrome, open, and glass skins render dark surfaces with WCAG AA text contrast in Chromium", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: "dark" })
  await recorded(page)
  await page.goto("/")
  await expect(page.getByRole("heading", { name: "Home", level: 1 })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(true)
  await capture(page, "luma-dark-home")

  await page.goto("/sources")
  await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
  await capture(page, "luma-dark-sources")

  for (const [skin, shotName] of [["polar", "polar-dark-sources"], ["open", "open-dark-sources"], ["glass", "glass-dark-sources"]] as const) {
    await page.locator('[data-slot="chrome-dock-trigger"]').first().click()
    const tray = page.locator('[data-slot="chrome-dock-tray"][data-state="open"]')
    await tray.locator(`[data-skin-option="${skin}"]`).click()
    await page.keyboard.press("Escape")
    await page.mouse.move(10, 10)
    await expect.poll(() => page.evaluate(() => document.documentElement.dataset.skin)).toBe(skin)
    if (skin === "polar") {
      await expect.poll(() => page.evaluate(() =>
        getComputedStyle(document.querySelector<HTMLElement>('[data-slot="button"][data-variant="default"]')!).backgroundColor,
      )).toBe("rgb(244, 244, 245)")
    }

    const sample = await page.evaluate(() => {
      const inset = document.querySelector<HTMLElement>('[data-slot="sidebar-inset"]')!
      const shell = document.querySelector<HTMLElement>('[data-slot="sidebar-wrapper"]')!
      const h1 = document.querySelector<HTMLElement>("h1")!
      const primary = document.querySelector<HTMLElement>('[data-slot="button"][data-variant="default"]')!
      const canvas = document.createElement("canvas")
      canvas.width = 1
      canvas.height = 1
      const ctx = canvas.getContext("2d", { willReadFrequently: true })!
      const read = () => {
        const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data
        return `rgb(${r}, ${g}, ${b})`
      }
      ctx.fillStyle = getComputedStyle(shell).backgroundColor
      ctx.fillRect(0, 0, 1, 1)
      const shellRgb = read()
      ctx.fillStyle = getComputedStyle(inset).backgroundColor
      ctx.fillRect(0, 0, 1, 1)
      const pageRgb = read()
      return {
        shellRgb,
        pageRgb,
        textRgb: getComputedStyle(h1).color,
        primaryRgb: getComputedStyle(primary).backgroundColor,
        backdrop: getComputedStyle(inset).backdropFilter,
      }
    })
    expect(luminance(rgb(sample.shellRgb)), `${skin} dark shell luminance`).toBeLessThan(0.08)
    expect(luminance(rgb(sample.pageRgb)), `${skin} dark page luminance`).toBeLessThan(0.08)
    expect(contrastRatio(rgb(sample.textRgb), rgb(sample.pageRgb)), `${skin} dark heading contrast`).toBeGreaterThanOrEqual(4.5)
    if (skin === "polar") {
      expect(sample.primaryRgb).toBe("rgb(244, 244, 245)")
      await capture(page, shotName)
      await page.goto("/")
      await expect(page.getByRole("heading", { name: "Home", level: 1 })).toBeVisible()
      await capture(page, "polar-dark-home")
      await page.goto("/sources")
      await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
      continue
    }
    if (skin === "glass") {
      expect(sample.backdrop).toContain("blur(")
    }
    await capture(page, shotName)
  }
})

test("single-select ToggleGroup preserves radiogroup/radio ARIA semantics and arrow-key navigation without aria-pressed", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "uses deterministic record-cell stand-in")
  await page.setViewportSize({ width: 1440, height: 900 })
  await recordCell(page, "admin")
  await page.goto("/sources/canon.Party/demo/party/0001")
  await page.getByRole("tab", { name: "Lineage" }).click()

  const group = page.getByRole("radiogroup", { name: "Show as" })
  await expect(group).toBeVisible()
  const graph = group.getByRole("radio", { name: "Graph" })
  const list = group.getByRole("radio", { name: "List" })
  await expect(graph).toHaveAttribute("aria-checked", "true")
  await expect(list).toHaveAttribute("aria-checked", "false")
  await expect(graph).not.toHaveAttribute("aria-pressed", /.+/)
  await expect(list).not.toHaveAttribute("aria-pressed", /.+/)

  await graph.focus()
  // A no-op cross-axis arrow key or modified same-axis arrow key must not arm selection.
  await page.keyboard.press("ArrowUp")
  await list.focus()
  await expect(graph).toHaveAttribute("aria-checked", "true")
  await expect(list).toHaveAttribute("aria-checked", "false")

  await graph.focus()
  await page.keyboard.press("Shift+ArrowRight")
  await list.focus()
  await expect(graph).toHaveAttribute("aria-checked", "true")
  await expect(list).toHaveAttribute("aria-checked", "false")

  await graph.focus()
  await page.keyboard.press("ArrowRight")
  await expect(list).toBeFocused()
  await expect(list).toHaveAttribute("aria-checked", "true")
  await expect(graph).toHaveAttribute("aria-checked", "false")

  await page.keyboard.press("ArrowLeft")
  await expect(graph).toBeFocused()
  await expect(graph).toHaveAttribute("aria-checked", "true")
  await expect(list).toHaveAttribute("aria-checked", "false")
})

test("custom StudioTheme JSON dialog validates WCAG AA contrast, applies custom tokens across reload, and resets cleanly", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: "dark" })
  await recorded(page)
  await page.goto("/sources")
  await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()

  await page.locator('[data-slot="chrome-dock-trigger"]').first().click()
  await page.locator('[data-slot="custom-theme-trigger"]').click()
  const dialog = page.getByRole("dialog", { name: "Custom theme JSON" })
  await expect(dialog).toBeVisible()
  await capture(page, "polar-dark-custom-theme-dialog")

  // Low-contrast JSON is rejected with WCAG AA contrast feedback before mutating :root.
  const editor = dialog.getByRole("textbox", { name: "Theme JSON definition" })
  await editor.fill(JSON.stringify({
    version: 1,
    name: "Too Faint",
    baseSkin: "polar",
    variants: { dark: { background: "#111113", foreground: "#222226" } },
  }))
  await dialog.getByRole("button", { name: "Apply theme" }).click()
  await expect(dialog.locator('[data-slot="theme-validation-errors"]')).toContainText("WCAG AA")

  // Selecting and applying the Graphite Terminal preset writes custom tokens and persists across reload.
  await dialog.getByRole("button", { name: "Graphite Terminal" }).click()
  await dialog.getByRole("button", { name: "Apply theme" }).click()
  await expect(dialog).toBeHidden()
  await expect.poll(() => page.evaluate(() => ({
    customTheme: document.documentElement.dataset.customTheme,
    bgVar: document.documentElement.style.getPropertyValue("--background").trim(),
  }))).toEqual({
    customTheme: "Graphite Terminal",
    bgVar: "#0f1117",
  })

  await page.reload()
  await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.customTheme)).toBe("Graphite Terminal")

  // Resetting clears every inline CSS variable cleanly.
  await page.locator('[data-slot="chrome-dock-trigger"]').first().click()
  await page.locator('[data-slot="custom-theme-trigger"]').click()
  await page.getByRole("button", { name: "Reset to built-in skin" }).click()
  await expect.poll(() => page.evaluate(() => ({
    customTheme: document.documentElement.dataset.customTheme,
    bgVar: document.documentElement.style.getPropertyValue("--background"),
  }))).toEqual({
    customTheme: undefined,
    bgVar: "",
  })
})

test("Semantic Map (/map) renders monochrome islands, bundled bridges, zoom-in expansion, lenses, and provenance side sheet", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: "dark" })
  await recorded(page)
  await page.goto("/map")
  await expect(page.getByRole("heading", { name: "Semantic map", level: 1 })).toBeVisible()

  const islands = page.locator('[data-slot="semantic-island"]')
  await expect(islands.first()).toBeVisible()
  await capture(page, "polar-dark-semantic-map")

  // Expand the first island (Zoom 1) and open a node's provenance side sheet (Zoom 2).
  await islands.first().getByRole("button", { name: "Expand island" }).click()
  await expect(page.locator('[data-slot="island-expansion"]')).toBeVisible()
  await page.locator('[data-slot="semantic-node"]').first().click()
  const sheet = page.locator('[data-slot="provenance-sheet"]')
  await expect(sheet).toBeVisible()
  await expect(sheet.getByRole("link", { name: "Open records" })).toBeVisible()
  await expect(sheet.getByRole("link", { name: "Definition" })).toBeVisible()
  await capture(page, "polar-dark-semantic-map-dossier")

  // Switch lenses and return to Zoom 0 overview.
  await page.getByRole("radio", { name: "Governance" }).click()
  await page.getByRole("button", { name: "All islands" }).click()
  await expect(islands.first()).toBeVisible()
})

test("boneyard-js renders UI-shape bone skeletons inside container bounds with zero horizontal overflow while pending", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: "dark" })
  await page.route("**/api/**", async (route) => {
    const url = route.request().url()
    if (url.includes("/v1/definitions")) {
      await new Promise((resolve) => setTimeout(resolve, 2500))
      return route.fulfill({ status: 404, json: { error: "not found" } })
    }
    return route.fulfill({ status: 404, json: { error: "not found" } })
  })
  // Register session AFTER the catch-all **/api/** route so /api/me serves environmentId: "analytics".
  await session(page)
  await page.goto("/map")
  const skeleton = page.locator('[data-slot="bone-skeleton"][data-bone="semantic-map"]')
  await expect(skeleton).toBeVisible()
  await expect(skeleton).toHaveAttribute("aria-busy", "true")
  const overlay = skeleton.locator('[data-boneyard-overlay="true"]')
  await expect(overlay).toBeVisible()
  await capture(page, "polar-dark-semantic-map-bones")

  // Verify real boneyard-js bones rendered from the viewport breakpoint and stay strictly within container bounds.
  const geometry = await page.evaluate(() => {
    const overlayEl = document.querySelector<HTMLElement>('[data-bone="semantic-map"] [data-boneyard-overlay="true"]')
    if (!overlayEl) return null
    const host = overlayEl.getBoundingClientRect()
    const bones = [...overlayEl.querySelectorAll<HTMLElement>('[data-boneyard-bone="true"]')].map((el) => {
      const r = el.getBoundingClientRect()
      return { left: r.left, right: r.right, width: r.width, height: r.height }
    })
    return {
      hostLeft: host.left,
      hostRight: host.right,
      hostWidth: host.width,
      count: bones.length,
      overflowing: bones.filter((b) => b.left < host.left - 1 || b.right > host.right + 1 || b.width <= 0 || b.height <= 0).length,
    }
  })
  expect(geometry).not.toBeNull()
  expect(geometry!.count).toBeGreaterThanOrEqual(10)
  expect(geometry!.overflowing).toBe(0)
})
