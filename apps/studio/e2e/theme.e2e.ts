import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/** Recorded mode: no live host cell, so every surface renders deterministically from the bundle. */
async function recorded(page: Page) {
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
}

/** Parse `rgb(...)` / `rgba(...)` computed CSS colors into `[r, g, b]`. */
const rgb = (value: string): [number, number, number] => {
  const match = value.match(/rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)/)
  if (!match) throw new Error(`unexpected color: ${value}`)
  return [Number(match[1]), Number(match[2]), Number(match[3])]
}

const luminance = ([r, g, b]: [number, number, number]) => {
  const lin = [r, g, b].map((v) => v / 255).map((c) => c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)
  return 0.2126 * lin[0]! + 0.7152 * lin[1]! + 0.0722 * lin[2]!
}

for (const scheme of ["light", "dark"] as const) {
  test(`theme v4 (${scheme}): two-surface shell, tinted panels, knot violet accent, and layered surfaces`, async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.emulateMedia({ colorScheme: scheme })
    await recorded(page)
    await page.goto("/sources")
    await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
    await expect(page.locator('[data-slot="stat-strip"]')).toBeVisible()
    await expect(page.locator('[data-slot="panel"]').first()).toBeVisible()
    await expect(page.locator('[data-slot="table-header"]').first()).toBeVisible()

    // HTML scheme class matches system color scheme.
    await expect.poll(() => page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(scheme === "dark")

    const metrics = await page.evaluate(() => {
      const shell = document.querySelector<HTMLElement>('[data-slot="sidebar-wrapper"]')!
      const inset = document.querySelector<HTMLElement>('[data-slot="sidebar-inset"]')!
      const panel = document.querySelector<HTMLElement>('[data-slot="panel"]')!
      const strip = document.querySelector<HTMLElement>('[data-slot="stat-strip"]')!
      const primary = document.querySelector<HTMLElement>('[data-slot="button"][data-variant="default"]')!
      const thRow = document.querySelector<HTMLElement>('[data-slot="table-header"] tr')!
      const kpiValue = document.querySelector<HTMLElement>('[data-slot="kpi"] > div')!
      const perspTrack = document.querySelector<HTMLElement>('[role="group"][aria-label="Perspective"]')!
      const perspActive = perspTrack.querySelector<HTMLElement>('button[aria-pressed="true"]')!

      const cs = (el: HTMLElement) => getComputedStyle(el)
      return {
        shellBg: cs(shell).backgroundColor,
        pageBg: cs(inset).backgroundColor,
        pageRadius: Number.parseFloat(cs(inset).borderTopLeftRadius),
        pageBorder: Number.parseFloat(cs(inset).borderTopWidth),
        pageShadow: cs(inset).boxShadow,
        panelBg: cs(panel).backgroundColor,
        panelRadius: Number.parseFloat(cs(panel).borderTopLeftRadius),
        stripBg: cs(strip).backgroundColor,
        stripRadius: Number.parseFloat(cs(strip).borderTopLeftRadius),
        primaryBg: cs(primary).backgroundColor,
        primaryRadius: Number.parseFloat(cs(primary).borderTopLeftRadius),
        thBg: cs(thRow).backgroundColor,
        kpiVariantNumeric: cs(kpiValue).fontVariantNumeric,
        perspTrackBg: cs(perspTrack).backgroundColor,
        perspActiveBg: cs(perspActive).backgroundColor,
      }
    })

    // 1. Two-surface shell: rail/sidebar ground differs from the raised page surface, with radius, hairline & shadow.
    expect(metrics.shellBg).not.toBe(metrics.pageBg)
    expect(metrics.pageRadius).toBeGreaterThanOrEqual(10)
    expect(metrics.pageBorder).toBeGreaterThanOrEqual(0.5)
    expect(metrics.pageShadow).not.toBe("none")

    // Dark mode layers progress upward in luminance: shell < page < card, and active segment sits above its track.
    // Light mode lifts pure white page above soft shell ground, with subtly tinted cards.
    const shellLum = luminance(rgb(metrics.shellBg))
    const pageLum = luminance(rgb(metrics.pageBg))
    const panelLum = luminance(rgb(metrics.panelBg))
    const trackLum = luminance(rgb(metrics.perspTrackBg))
    const activeLum = luminance(rgb(metrics.perspActiveBg))
    if (scheme === "dark") {
      expect(shellLum).toBeLessThan(pageLum)
      expect(pageLum).toBeLessThan(panelLum)
      expect(activeLum).toBeGreaterThan(trackLum)
    } else {
      expect(shellLum).toBeLessThan(pageLum)
      expect(panelLum).toBeLessThan(pageLum)
      expect(activeLum).toBeGreaterThan(trackLum)
    }

    // 2. Panels and StatStrip sit on a subtly tinted card surface at 10-12px radius (at 90% root font-size).
    expect(metrics.panelBg).not.toBe(metrics.pageBg)
    expect(metrics.stripBg).not.toBe(metrics.pageBg)
    expect(metrics.panelRadius).toBeGreaterThanOrEqual(9)
    expect(metrics.panelRadius).toBeLessThanOrEqual(13)
    expect(metrics.stripRadius).toBeGreaterThanOrEqual(9)
    expect(metrics.stripRadius).toBeLessThanOrEqual(13)

    // 3. Primary action button uses knot violet (strongly chromatic blue/violet channel, never greyscale) and ~7-9px radius.
    const [pr, pg, pb] = rgb(metrics.primaryBg)
    expect(pb).toBeGreaterThan(pr + 40)
    expect(pb).toBeGreaterThan(pg + 60)
    expect(metrics.primaryRadius).toBeGreaterThanOrEqual(6)
    expect(metrics.primaryRadius).toBeLessThanOrEqual(9)

    // 4. Table header has a quiet tint distinct from pure page background, and KPI figures use tabular-nums.
    expect(metrics.thBg).not.toBe(metrics.pageBg)
    expect(metrics.kpiVariantNumeric).toContain("tabular-nums")
  })
}
