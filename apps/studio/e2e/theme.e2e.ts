import { expect, test, type Locator, type Page } from "@playwright/test"
import { actionCell } from "./action-cell.ts"
import { objectCell, population, session } from "./object-cell.ts"
import { recordCell } from "./record-cell.ts"

const SHOTS = process.env.SHOTS

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

const contrastRatio = (a: [number, number, number], b: [number, number, number]) => {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (hi! + 0.05) / (lo! + 0.05)
}

/** Wait until all running CSS transitions/animations on `locator` and its ancestors have finished and ancestors are opaque (past dialog `@starting-style` fade-in). */
async function settledAnimations(locator: Locator) {
  await expect.poll(() =>
    locator.evaluate((el) => {
      let running = 0
      let minAncestorOpacity = 1
      for (let node: Element | null = el; node !== null; node = node.parentElement) {
        running += node.getAnimations().filter((a) => a.playState === "running").length
        if (node !== el) minAncestorOpacity = Math.min(minAncestorOpacity, Number.parseFloat(getComputedStyle(node).opacity || "1"))
      }
      return running === 0 && minAncestorOpacity >= 0.99
    }),
  ).toBe(true)
}

/** True when a computed `box-shadow` contains at least one visible (non-zero-alpha) shadow layer. */
function hasVisibleShadow(boxShadow: string): boolean {
  if (!boxShadow || boxShadow === "none") return false
  return boxShadow
    .split(/,(?![^(]*\))/)
    .some((layer) => !/rgba\(\s*0,\s*0,\s*0,\s*0\s*\)\s+0px\s+0px\s+0px\s+0px/.test(layer.trim()))
}

/**
 * Measure the real composited text vs background contrast of `locator` by walking the DOM ancestor
 * chain from `<html>` down to `locator` and compositing every background and effective `opacity`
 * through a 1x1 canvas, matching what the browser actually paints.
 */
async function measureRenderedContrast(locator: Locator) {
  await settledAnimations(locator)
  const sample = await locator.evaluate((el) => {
    const chain: Element[] = []
    for (let node: Element | null = el; node !== null; node = node.parentElement) chain.unshift(node)
    const canvas = document.createElement("canvas")
    canvas.width = 1
    canvas.height = 1
    const ctx = canvas.getContext("2d", { willReadFrequently: true })!
    const read = (): [number, number, number] => {
      const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data
      return [r!, g!, b!]
    }
    const base = document.documentElement.classList.contains("dark") ? "#111115" : "#ffffff"
    ctx.fillStyle = base
    ctx.fillRect(0, 0, 1, 1)

    let effectiveOpacity = 1
    let backingBg: [number, number, number] = read()
    for (const node of chain) {
      const style = getComputedStyle(node)
      const op = Number.parseFloat(style.opacity || "1")
      if (node === el) backingBg = read()
      if (style.backgroundColor && style.backgroundColor !== "rgba(0, 0, 0, 0)" && style.backgroundColor !== "transparent") {
        ctx.save()
        ctx.globalAlpha = effectiveOpacity * op
        ctx.fillStyle = style.backgroundColor
        ctx.fillRect(0, 0, 1, 1)
        ctx.restore()
      }
      effectiveOpacity *= op
    }
    const bg = read()

    const own = getComputedStyle(el)
    ctx.save()
    ctx.fillStyle = `rgb(${bg[0]}, ${bg[1]}, ${bg[2]})`
    ctx.fillRect(0, 0, 1, 1)
    ctx.globalAlpha = effectiveOpacity
    ctx.fillStyle = own.color
    ctx.fillRect(0, 0, 1, 1)
    ctx.restore()
    const fg = read()

    return { fg, bg, backingBg, opacity: effectiveOpacity, ownOpacity: Number.parseFloat(own.opacity || "1") }
  })
  return { ...sample, ratio: contrastRatio(sample.fg, sample.bg) }
}

/**
 * Press a key (`Tab`, `ArrowRight`, `ArrowDown`, etc.) until `target` becomes `document.activeElement`,
 * failing clearly if it is not reached within `maxSteps`.
 */
async function moveFocusWithKey(page: Page, target: Locator, key: string, maxSteps = 25) {
  for (let step = 0; step < maxSteps; step += 1) {
    if (await target.evaluate((el) => el === document.activeElement)) return
    await page.keyboard.press(key)
  }
  const active = await page.evaluate(() => {
    const el = document.activeElement
    return el ? `${el.tagName} aria-label="${el.getAttribute("aria-label") ?? ""}" text="${el.textContent?.trim().slice(0, 40) ?? ""}"` : "null"
  })
  throw new Error(`did not reach target with ${key} within ${maxSteps} presses (stuck on ${active})`)
}

/**
 * Assert that keyboard focus produced a real visible `:focus-visible` outline indicator on `indicator`
 * (which defaults to `focused`, or a child label when focus sits on a treeitem wrapper).
 */
async function expectVisibleFocusOutline(focused: Locator, indicator: Locator = focused, expectedToken: "ring" | "destructive" = "ring") {
  await expect(focused).toBeFocused()
  await settledAnimations(indicator)
  const state = await focused.evaluate((el, tokenName) => {
    const target = el.querySelector<HTMLElement>(":scope > [data-slot='tree-label']") ?? (el as HTMLElement)
    const own = getComputedStyle(target)
    const canvas = document.createElement("canvas")
    canvas.width = 1
    canvas.height = 1
    const ctx = canvas.getContext("2d", { willReadFrequently: true })!
    const toRgb = (cssColor: string, fallback: string): [number, number, number] => {
      ctx.fillStyle = fallback
      ctx.fillRect(0, 0, 1, 1)
      ctx.fillStyle = cssColor
      ctx.fillRect(0, 0, 1, 1)
      const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data
      return [r!, g!, b!]
    }
    const base = document.documentElement.classList.contains("dark") ? "#111115" : "#ffffff"
    const expectedVar = getComputedStyle(document.documentElement).getPropertyValue(`--${tokenName}`).trim()
    return {
      focusVisible: el.matches(":focus-visible"),
      outlineStyle: own.outlineStyle,
      outlineWidth: Number.parseFloat(own.outlineWidth),
      outlineOffset: Number.parseFloat(own.outlineOffset),
      outlineColor: toRgb(own.outlineColor, base),
      expectedColor: toRgb(expectedVar, base),
      boxShadow: own.boxShadow,
    }
  }, expectedToken)
  // If an explicit indicator differs from focused and isn't a tree-label, read it directly.
  if (indicator !== focused) {
    const indStyle = await indicator.evaluate((el) => {
      const own = getComputedStyle(el)
      return { outlineStyle: own.outlineStyle, outlineWidth: Number.parseFloat(own.outlineWidth), outlineOffset: Number.parseFloat(own.outlineOffset) }
    })
    expect(indStyle.outlineStyle).toBe("solid")
    expect(indStyle.outlineWidth).toBeGreaterThanOrEqual(1.8)
    expect(Math.abs(indStyle.outlineOffset)).toBeGreaterThanOrEqual(0.8)
  }
  expect(state.focusVisible, "keyboard navigation must activate :focus-visible").toBe(true)
  expect(state.outlineStyle, "focus outline must be solid").toBe("solid")
  expect(state.outlineWidth, "focus outline width >= 2px").toBeGreaterThanOrEqual(1.8)
  expect(Math.abs(state.outlineOffset), "focus outline must have non-zero offset").toBeGreaterThanOrEqual(0.8)
  expect(state.outlineColor, `focus outline color must match --${expectedToken}`).toEqual(state.expectedColor)
  return state
}

async function shot(page: Page, name: string, locator?: Locator) {
  if (!SHOTS) return
  await page.waitForTimeout(150)
  if (locator) await locator.screenshot({ path: `${SHOTS}/${name}.png` })
  else await page.screenshot({ path: `${SHOTS}/${name}.png` })
}

for (const scheme of ["light", "dark"] as const) {
  test(`theme v4 (${scheme}): Luma glass islands by default, Polar monochrome + Open v6 double-shell geometry, primary button states (rest/hover/focus), and layered surfaces`, async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.emulateMedia({ colorScheme: scheme })
    await recorded(page)
    // The default is luma (owner decision, 2026-11-25): the main island is a floating glass panel with
    // its specular edge, radius and depth shadow, blurred over the backdrop.
    await page.goto("/sources")
    await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.dataset.skin)).toBe("luma")
    const island = page.locator('[data-slot="sidebar-inset"]')
    await expect(island).toHaveCSS("border-top-left-radius", "18px")
    await expect(island).toHaveCSS("border-top-width", "1px")
    await expect(island).toHaveCSS("backdrop-filter", /blur\(24px\)/)
    expect(await island.evaluate((element) => getComputedStyle(element).boxShadow)).not.toBe("none")
    // Polar is the monochrome alternative, one choice away.
    await page.evaluate(() => localStorage.setItem("norn.studio.skin", "polar"))
    await page.reload()
    await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
    await expect(page.locator('[data-slot="stat-strip"]')).toBeVisible()
    await expect(page.locator('[data-slot="panel"]').first()).toBeVisible()
    await expect(page.locator('[data-slot="table-header"]').first()).toBeVisible()

    // HTML scheme class matches system color scheme, and the chosen skin is Polar monochrome.
    await expect.poll(() => page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(scheme === "dark")
    await expect.poll(() => page.evaluate(() => document.documentElement.dataset.skin)).toBe("polar")

    const measureLayout = () => page.evaluate(() => {
      const shell = document.querySelector<HTMLElement>('[data-slot="sidebar"]')!
      const inset = document.querySelector<HTMLElement>('[data-slot="sidebar-inset"]')!
      const panel = document.querySelector<HTMLElement>('[data-slot="panel"]')!
      const kpi = document.querySelector<HTMLElement>('[data-slot="kpi"]')!
      const kpiWell = document.querySelector<HTMLElement>('[data-slot="kpi-well"]')!
      const primary = document.querySelector<HTMLElement>('[data-slot="button"][data-variant="default"]')!
      const thRow = document.querySelector<HTMLElement>('[data-slot="table-header"] tr')!
      const kpiValue = document.querySelector<HTMLElement>('[data-slot="kpi-well"] > div')!

      const canvas = document.createElement("canvas")
      canvas.width = 1
      canvas.height = 1
      const ctx = canvas.getContext("2d", { willReadFrequently: true })!
      const toRgb = (cssColor: string, fallbackBg: string) => {
        ctx.clearRect(0, 0, 1, 1)
        ctx.fillStyle = fallbackBg
        ctx.fillRect(0, 0, 1, 1)
        ctx.fillStyle = cssColor
        ctx.fillRect(0, 0, 1, 1)
        const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data
        return `rgb(${r}, ${g}, ${b})`
      }

      const cs = (el: HTMLElement) => getComputedStyle(el)
      const pageBg = toRgb(cs(inset).backgroundColor, "#000000")
      return {
        shellBg: toRgb(cs(shell).backgroundColor, "#000000"),
        pageBg,
        pageRadius: Number.parseFloat(cs(inset).borderTopLeftRadius),
        pageBorder: Number.parseFloat(cs(inset).borderTopWidth),
        pageShadow: cs(inset).boxShadow,
        panelBg: toRgb(cs(panel).backgroundColor, pageBg),
        panelRadius: Number.parseFloat(cs(panel).borderTopLeftRadius),
        kpiRadius: Number.parseFloat(cs(kpi).borderTopLeftRadius),
        kpiWellBg: toRgb(cs(kpiWell).backgroundColor, pageBg),
        kpiWellRadius: Number.parseFloat(cs(kpiWell).borderTopLeftRadius),
        primaryBg: toRgb(cs(primary).backgroundColor, pageBg),
        primaryRadius: Number.parseFloat(cs(primary).borderTopLeftRadius),
        thBg: toRgb(cs(thRow).backgroundColor, pageBg),
        kpiVariantNumeric: cs(kpiValue).fontVariantNumeric,
      }
    })

    const polar = await measureLayout()

    // 1. The main island is its own floating panel in every skin (DESIGN.md 9.1); polar's islands are
    // opaque, so the submenu island and the main island are the same monochrome ground.
    expect(polar.pageRadius).toBe(18)
    expect(polar.pageBorder).toBe(1)
    expect(polar.shellBg).toBe(polar.pageBg)

    // Polar monochrome layers: cards lift off the island.
    const polarPageLum = luminance(rgb(polar.pageBg))
    const polarPanelLum = luminance(rgb(polar.panelBg))
    expect(polar.panelBg).not.toBe(polar.pageBg)
    expect(polarPageLum).not.toBe(polarPanelLum)

    // 2. Polar keeps the concentric double shell (DESIGN.md 5): 16px rim, 4px inset, 11px well, in px.
    expect(polar.panelRadius).toBe(16)
    expect(polar.kpiRadius).toBe(16)
    expect(polar.kpiWellRadius).toBe(11)

    // 3. Polar primary action button: strictly monochrome (no blue/purple tint), a pill, >= 4.5:1 rest & hover, visible focus.
    const primary = page.locator('[data-slot="button"][data-variant="default"]').first()
    const [ppr, ppg, ppb] = rgb(polar.primaryBg)
    expect(Math.max(ppr, ppg, ppb) - Math.min(ppr, ppg, ppb)).toBeLessThanOrEqual(8)
    expect(polar.primaryRadius).toBeGreaterThanOrEqual(16)

    const primaryRest = await measureRenderedContrast(primary)
    expect(primaryRest.ratio, `${scheme} polar primary rest contrast`).toBeGreaterThanOrEqual(4.5)
    await primary.hover()
    const primaryHover = await measureRenderedContrast(primary)
    expect(primaryHover.ratio, `${scheme} polar primary hover contrast`).toBeGreaterThanOrEqual(4.5)
    // Keyboard focus via Tab onto primary button from the previous header control.
    await page.getByRole("button", { name: "Notifications" }).focus()
    await moveFocusWithKey(page, primary, "Tab", 25)
    await expectVisibleFocusOutline(primary)

    // 4. Table header has a quiet tint distinct from pure page background, and KPI figures use tabular-nums.
    expect(polar.thBg).not.toBe(polar.pageBg)
    expect(polar.kpiVariantNumeric).toContain("tabular-nums")

    // 5. Switch to Open v6 skin and verify signature double-shell geometry + cobalt primary CTA remain intact.
    await page.evaluate(() => {
      localStorage.setItem("norn.studio.skin", "open")
    })
    await page.reload()
    await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
    await expect(page.locator('[data-slot="stat-strip"]')).toBeVisible()
    await expect(page.locator('[data-slot="panel"]').first()).toBeVisible()
    await expect(page.locator('[data-slot="table-header"]').first()).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.dataset.skin)).toBe("open")

    const open = await measureLayout()
    const openShellLum = luminance(rgb(open.shellBg))
    const openPageLum = luminance(rgb(open.pageBg))
    const openPanelLum = luminance(rgb(open.panelBg))
    const openWellLum = luminance(rgb(open.kpiWellBg))
    if (scheme === "dark") {
      expect(openShellLum).toBeLessThan(openPageLum)
      expect(openPageLum).toBeLessThan(openPanelLum)
      expect(openPanelLum).toBeLessThan(openWellLum)
    } else {
      expect(openShellLum).toBeLessThan(openPageLum)
      expect(openWellLum).toBeLessThan(openPageLum)
    }
    expect(open.kpiWellBg).not.toBe(open.pageBg)
    expect(open.pageRadius).toBe(18)
    expect(open.panelRadius).toBe(20)
    expect(open.kpiRadius).toBe(20)
    expect(open.kpiWellRadius).toBe(15)
    const [opr, opg, opb] = rgb(open.primaryBg)
    expect(opb).toBeGreaterThan(opr + 40)
    expect(opb).toBeGreaterThan(opg + 60)
  })

  test(`theme v4 (${scheme}): keyboard focus stays visible on selected line tabs, the current applet on the rail and the current menu row`, async ({ page }) => {
    test.skip(Boolean(process.env.E2E_BASE), "uses deterministic record-cell stand-in")
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.emulateMedia({ colorScheme: scheme })
    await recordCell(page, "admin")
    await page.goto("/sources/canon.Party/demo/party/0001")

    // 1. Selected record-page line tab: Tab onto Overview, verify focus outline, then ArrowRight to Lineage and back.
    const tablist = page.locator('[data-slot="tabs-list"][data-variant="line"]')
    const overview = tablist.getByRole("tab", { name: "Overview" })
    const lineage = tablist.getByRole("tab", { name: "Lineage" })
    await expect(overview).toHaveAttribute("aria-selected", "true")

    await page.getByRole("button", { name: "Read again" }).focus()
    await moveFocusWithKey(page, overview, "Tab", 25)
    await expectVisibleFocusOutline(overview)
    await expect(overview).toHaveCSS("background-color", "rgba(0, 0, 0, 0)")
    const selectedTab = await measureRenderedContrast(overview)
    expect(selectedTab.ratio, `${scheme} selected line tab`).toBeGreaterThanOrEqual(4.5)
    await shot(page, `line-tab-selected-${scheme}`, tablist)

    await page.keyboard.press("ArrowRight")
    await expect(lineage).toHaveAttribute("aria-selected", "true")
    await expectVisibleFocusOutline(lineage)

    await page.keyboard.press("ArrowLeft")
    await expect(overview).toHaveAttribute("aria-selected", "true")
    await expectVisibleFocusOutline(overview)

    // 2. Current applet: on Inbox (/decisions), Tab from the workspace switcher into the applet rail and arrow
    // onto the current applet; the 2px ring coexists with the neutral selection pill (no ring or bar of its own).
    await page.unrouteAll({ behavior: "ignoreErrors" })
    await recorded(page)
    await page.goto("/decisions")
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible()
    const rail = page.getByRole("navigation", { name: "Applets" }).getByRole("toolbar")
    const activeRail = rail.locator('a[href="/decisions"][aria-current="page"]')
    await expect(activeRail).toBeVisible()
    await page.locator("[data-scope]").focus()
    await page.keyboard.press("Tab")
    try {
      await moveFocusWithKey(page, activeRail, "ArrowDown", 6)
    } catch {
      await moveFocusWithKey(page, activeRail, "ArrowUp", 6)
    }
    await expectVisibleFocusOutline(activeRail)
    await expect(activeRail).not.toHaveCSS("background-color", "rgba(0, 0, 0, 0)")
    await shot(page, `focus-rail-active-${scheme}`, rail)

    // 3. Current menu row: on /sources, Tab from the Builder menu's filter into its tree and arrow onto aria-current="page".
    await page.goto("/sources")
    await expect(page.getByRole("heading", { name: "Sources", level: 1 })).toBeVisible()
    const currentTreeItem = page.locator('nav[aria-label="Builder menu"] [role="tree"] [role="treeitem"][aria-current="page"]')
    await expect(currentTreeItem).toBeVisible()
    await page.getByRole("searchbox", { name: "Filter Builder" }).focus()
    await page.keyboard.press("Tab")
    await moveFocusWithKey(page, currentTreeItem, "ArrowDown", 15)
    await expectVisibleFocusOutline(currentTreeItem, currentTreeItem.locator('> [data-slot="tree-label"]'))
  })

  test(`theme v4 (${scheme}): Records filter count, Discard dialog destructive hover/focus, and disabled primary button pass WCAG AA`, async ({ page }) => {
    test.skip(Boolean(process.env.E2E_BASE), "uses deterministic object/action stand-ins")
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.emulateMedia({ colorScheme: scheme })
    await session(page)
    const objects = await objectCell(page, population(40))
    await actionCell(page, objects)
    await page.goto("/objects/canon.Holding")
    await expect(page.getByText("40 records", { exact: true })).toBeVisible()

    // 1. Records filter count badge: apply a filter and check contrast both at rest and while hovering the Filter button.
    const filterButton = page.getByRole("button", { name: /^Filter/ })
    await filterButton.click()
    await page.getByLabel("Condition 1 property").selectOption("valuation_method")
    await page.getByLabel("Condition 1 value").selectOption("fifo")
    await page.getByRole("button", { name: "Apply" }).click()
    const countBadge = filterButton.locator("span", { hasText: "1" })
    await expect(countBadge).toBeVisible()

    const countRest = await measureRenderedContrast(countBadge)
    expect(countRest.ratio, `${scheme} filter count rest`).toBeGreaterThanOrEqual(4.5)
    await filterButton.hover()
    const countHover = await measureRenderedContrast(countBadge)
    expect(countHover.ratio, `${scheme} filter count on Filter hover`).toBeGreaterThanOrEqual(4.5)
    await shot(page, `fixed-filter-count-${scheme}`, page.locator('[role="toolbar"][aria-label="Records"]'))

    // 2. Destructive "Discard" button in the dark/light dialog (bg-popover): rest, hover >= 4.5:1 and visible focus outline.
    await page.getByRole("button", { name: "New holding" }).click()
    await page.getByRole("menuitem", { name: "Open a holding" }).click()
    await page.getByRole("dialog").getByLabel("Holding id").fill("demo/holding/9999")
    await page.keyboard.press("Escape")
    const discardDialog = page.getByRole("dialog", { name: "Discard this draft?" })
    await expect(discardDialog).toBeVisible()
    const discardBtn = discardDialog.getByRole("button", { name: "Discard" })

    const discardRest = await measureRenderedContrast(discardBtn)
    expect(discardRest.ratio, `${scheme} Discard button rest`).toBeGreaterThanOrEqual(4.5)
    await expect(discardBtn).toHaveCSS("transition-property", "border-color, box-shadow, transform")
    await discardBtn.hover()
    const discardHover = await measureRenderedContrast(discardBtn)
    expect(discardHover.ratio, `${scheme} Discard button hover`).toBeGreaterThanOrEqual(4.5)
    await shot(page, `discard-hover-${scheme}`, discardDialog)

    await discardDialog.getByRole("button", { name: "Keep draft" }).focus()
    await moveFocusWithKey(page, discardBtn, "Tab", 5)
    await expectVisibleFocusOutline(discardBtn, discardBtn, "destructive")
  })

  test(`theme v4 (${scheme}): ChoiceColumn unselected Recommended mark, DecisionTree numbers & options, and Overwrite destructive hover on card pass WCAG AA`, async ({ page }) => {
    test.skip(Boolean(process.env.E2E_BASE), "uses deterministic stand-ins")
    page.on("dialog", (dialog) => void dialog.accept())
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.emulateMedia({ colorScheme: scheme })

    const catalog = {
      objectSet: "demo.Conversion",
      properties: [{ name: "occurred_at", kind: "timestamp", unit: null }],
      metrics: [{ name: "demo.yield", label: "Yield", type: "decimal" }],
      grains: ["week"],
      operators: ["eq"],
      example: { irVersion: "0.1", calendar: "iso_utc", freshness: { kind: "live", maxAgeMs: 5000 } },
      declarations: { metrics: [], objectType: null },
      maxGroupBy: 1,
    }
    const windowSpec = { start: "2026-07-06T00:00:00Z", end: "2026-10-26T00:00:00Z" }
    const tree = {
      basis: "From the catalogue",
      measures: [{ id: "demo.yield", label: "Yield", detail: "yield", recommended: true, value: {} }],
      groupBy: [{ id: "occurred_at", label: "occurred at", detail: "time", recommended: true, value: {} }],
      grains: { occurred_at: [{ id: "week", label: "Week", detail: "weekly", recommended: true, value: {} }] },
      conditions: [
        { id: "every", label: "Every record", detail: "all", recommended: true, value: { filter: [] } },
        { id: "gold", label: "metal is gold", detail: "only gold", recommended: false, value: { filter: [{ path: { property: "metal" }, operator: "eq", value: "gold" }] } },
      ],
      filterable: [],
      windows: [{ id: "cell", label: "This window", detail: "dates", recommended: true, value: { window: windowSpec } }],
    }
    await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u_e2e", name: "Analyst", email: "analyst@example.com" }, organization: { id: "org_demo", name: "Demo", role: "member" }, environmentId: "analytics" } }))
    await page.route("**/api/questions/catalog", (route) => route.fulfill({ json: catalog }))
    await page.route("**/api/questions/recommendations", (route) => route.fulfill({ json: tree }))
    await page.route("**/api/questions/check", (route) => route.fulfill({ json: { kind: "question", admitted: true, findings: [], plan: { grain: "week", question: {}, sums: [], metrics: [] }, restatement: { text: "The checked question.", sha256: "a".repeat(64) } } }))
    await page.route("**/api/host/v1/definitions**", (route) => {
      const url = new URL(route.request().url())
      if (url.pathname.endsWith(":list")) return route.fulfill({ json: { items: [], cursor: null } })
      if (url.pathname.endsWith(":admit")) return route.fulfill({ json: { admitted: true, diagnostics: [], restatement: null, restatementSha256: null } })
      if (url.pathname.endsWith(":install")) return route.fulfill({ status: 409, json: { code: "definition.version_conflict" } })
      return route.fulfill({ json: { id: "studio.dashboard.conflict", kind: "dashboard", version: 1, payload: { title: "Weekly ops", tiles: [] } } })
    })

    // 1. ChoiceColumn (/metrics/build): enabled unselected option with Recommended stays opaque (>= 4.5:1), selected keeps focus outline, disabled Save button stays flat.
    await page.goto("/metrics/build")
    const questionTree = page.getByTestId("question-tree")
    const firstMeasure = questionTree.getByRole("list", { name: "Measure", exact: true }).getByRole("button").first()
    await expect(firstMeasure).toBeVisible()
    await page.getByRole("button", { name: "Search" }).focus()
    await moveFocusWithKey(page, firstMeasure, "Tab")
    const focus = await expectVisibleFocusOutline(firstMeasure)
    const outlineClearance = await firstMeasure.evaluate((button, extent) => {
      const scrollport = button.closest('[data-testid="question-tree"]')
      if (!scrollport) throw new Error("missing question tree scrollport")
      const choice = button.getBoundingClientRect()
      const port = scrollport.getBoundingClientRect()
      const left = port.left + scrollport.clientLeft
      const top = port.top + scrollport.clientTop
      return Math.min(
        choice.left - left - extent,
        left + scrollport.clientWidth - choice.right - extent,
        choice.top - top - extent,
        top + scrollport.clientHeight - choice.bottom - extent,
      )
    }, focus.outlineWidth + focus.outlineOffset)
    expect(outlineClearance, "first Measure choice outline fits inside the scrollport").toBeGreaterThanOrEqual(0)
    await shot(page, `measure-focus-${scheme}`, questionTree)

    const everyBtn = page.getByRole("button", { name: /Every record/ })
    const goldBtn = page.getByRole("button", { name: /metal is gold/ })
    await expect(everyBtn).toBeVisible()
    await goldBtn.click()
    await expect(goldBtn).toHaveAttribute("aria-pressed", "true")
    await expect(everyBtn).toHaveAttribute("aria-pressed", "false")

    const unselectedLabel = everyBtn.locator("span", { hasText: /^Every record$/ })
    const unselectedRecommended = everyBtn.locator("span", { hasText: /^Recommended$/ })
    const labelRest = await measureRenderedContrast(unselectedLabel)
    const recRest = await measureRenderedContrast(unselectedRecommended)
    expect(labelRest.opacity, `${scheme} unselected ChoiceColumn must stay opaque`).toBe(1)
    expect(recRest.opacity, `${scheme} unselected Recommended mark must stay opaque`).toBe(1)
    expect(labelRest.ratio, `${scheme} unselected ChoiceColumn label rest`).toBeGreaterThanOrEqual(4.5)
    expect(recRest.ratio, `${scheme} unselected ChoiceColumn Recommended rest`).toBeGreaterThanOrEqual(4.5)
    await everyBtn.hover()
    const recHover = await measureRenderedContrast(unselectedRecommended)
    expect(recHover.ratio, `${scheme} unselected ChoiceColumn Recommended hover`).toBeGreaterThanOrEqual(4.5)
    // Selected choice keeps visible keyboard focus outline when Tabbed onto.
    await everyBtn.focus()
    await moveFocusWithKey(page, goldBtn, "Tab", 4)
    await expectVisibleFocusOutline(goldBtn)
    await shot(page, `fixed-choice-columns-${scheme}`, page.locator('[data-field="filters"]'))

    // Disabled primary Save button is visibly dimmed and flat (no colored action shadow).
    const saveBtn = page.getByRole("button", { name: "Save question" })
    await expect(saveBtn).toBeDisabled()
    const disabledStyle = await saveBtn.evaluate((el) => ({
      opacity: Number.parseFloat(getComputedStyle(el).opacity),
      boxShadow: getComputedStyle(el).boxShadow,
    }))
    expect(disabledStyle.opacity).toBeLessThan(0.6)
    expect(hasVisibleShadow(disabledStyle.boxShadow), `disabled primary button shadow: ${disabledStyle.boxShadow}`).toBe(false)

    // 2. DecisionTree (/sheet.html?state=decision%2Fpath): selected option numbers, unselected options, Agent's pick (selected & unselected), and selected option focus.
    await page.goto("/sheet.html?state=decision%2Fpath")
    const decisionTree = page.getByTestId("decision-tree")
    await expect(decisionTree).toBeVisible()

    const selectedOption = decisionTree.locator('button[aria-pressed="true"]').first()
    const selectedNumber = selectedOption.locator("span > span").first()
    const numContrast = await measureRenderedContrast(selectedNumber)
    expect(numContrast.ratio, `${scheme} DecisionTree selected number`).toBeGreaterThanOrEqual(4.5)

    const unselectedOption = decisionTree.locator('button[aria-pressed="false"]').first()
    const unselectedOptContrast = await measureRenderedContrast(unselectedOption.locator("span[data-option-label]").first())
    expect(unselectedOptContrast.opacity, `${scheme} DecisionTree unselected option stays opaque`).toBe(1)
    expect(unselectedOptContrast.ratio, `${scheme} DecisionTree unselected option label`).toBeGreaterThanOrEqual(4.5)

    const agentPick = decisionTree.locator("span", { hasText: /^Agent's pick$/ }).first()
    const pickContrast = await measureRenderedContrast(agentPick)
    expect(pickContrast.opacity, `${scheme} DecisionTree Agent's pick stays opaque`).toBe(1)
    expect(pickContrast.ratio, `${scheme} DecisionTree Agent's pick`).toBeGreaterThanOrEqual(4.5)

    // Keyboard focus on selected DecisionTree option card stays visible over .canvas-node-selected.
    await page.keyboard.press("Tab")
    await moveFocusWithKey(page, selectedOption, "Tab", 10)
    await expectVisibleFocusOutline(selectedOption)
    await shot(page, `fixed-decision-tree-${scheme}`, decisionTree)

    // Selecting the sibling option leaves "Agent's pick" on an enabled unselected card — verify it stays opaque and >= 4.5:1.
    const leaveIt = decisionTree.getByRole("button", { name: /Leave it/ })
    await leaveIt.click()
    await expect(leaveIt).toHaveAttribute("aria-pressed", "true")
    const unselectedAgentPick = decisionTree.getByRole("button", { name: /Send a reminder/ }).locator("span", { hasText: /^Agent's pick$/ })
    const unselectedPickContrast = await measureRenderedContrast(unselectedAgentPick)
    expect(unselectedPickContrast.opacity, `${scheme} DecisionTree unselected Agent's pick stays opaque`).toBe(1)
    expect(unselectedPickContrast.ratio, `${scheme} DecisionTree unselected Agent's pick`).toBeGreaterThanOrEqual(4.5)

    // 3. "Overwrite with mine" destructive button on conflict alert card (bg-card): rest and hover >= 4.5:1.
    await page.goto("/dashboards/studio.dashboard.conflict/edit")
    await expect(page.getByLabel("Title")).toHaveValue("Weekly ops")
    await page.getByLabel("Title").fill("Weekly ops updated")
    await page.getByRole("button", { name: "Save", exact: true }).click()
    const conflictCard = page.getByRole("alert").filter({ hasText: "Someone saved a newer version" })
    await expect(conflictCard).toBeVisible()
    const overwriteBtn = conflictCard.getByRole("button", { name: "Overwrite with mine" })

    const overwriteRest = await measureRenderedContrast(overwriteBtn)
    expect(overwriteRest.ratio, `${scheme} Overwrite with mine rest`).toBeGreaterThanOrEqual(4.5)
    await overwriteBtn.hover()
    const overwriteHover = await measureRenderedContrast(overwriteBtn)
    expect(overwriteHover.ratio, `${scheme} Overwrite with mine hover`).toBeGreaterThanOrEqual(4.5)
    await shot(page, `fixed-overwrite-hover-${scheme}`, conflictCard)
  })
}
