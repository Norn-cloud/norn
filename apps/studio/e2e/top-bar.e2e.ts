import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

// TOP BAR EXTRAS AND LIST PRIMITIVES (DESIGN.md 9.1, 9.2, 10, 12.4; restyle 2, #1705): the rules tick
// bar draws only what the cell serves (its laws, and what the inbox says each holds back), the
// language pill and the scheme toggle are real local state with an undo, everything works by
// keyboard, in Arabic, under reduced motion and at 390px, and list rows are 44px on a phone.

const definitions = JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as { artifactId: string; kind?: string }[]

const linesRule = { kind: "law", artifactId: "canon.ItemizedDocument.total_matches_lines.law", lawId: "canon.ItemizedDocument.total_matches_lines", version: "sha256:1" }
const heldInbox = {
  scanLimit: 500,
  sections: [
    { kind: "held_back", count: 3, shown: 1, approximate: false },
    { kind: "decision", count: 0, shown: 0, approximate: false },
    { kind: "source_error", count: 0, shown: 0, approximate: false },
  ],
  items: [{ kind: "held_back", ruleRef: linesRule, title: "Totals match lines", rationale: null, objectType: "canon.OrderLine", count: 3, approximate: false, target: "/objects/canon.OrderLine?population=held_back", records: [] }],
}
const cleanInbox = { scanLimit: 500, sections: [{ kind: "held_back", count: 0, shown: 0, approximate: false }], items: [] }

type Inbox = typeof heldInbox | typeof cleanInbox | "fail"

async function cell(page: Page, inbox: Inbox, served: readonly unknown[] = definitions) {
  await session(page)
  await page.route("**/api/host/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: served } })
    if (path.endsWith("/v1/inbox")) return inbox === "fail"
      ? route.fulfill({ status: 500, json: { code: "internal", message: "The inbox could not be read." } })
      : route.fulfill({ json: inbox })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
}

/** The tick bar's trigger, by its name; its counts sit on the tick strip inside it. */
const tickBar = (page: Page) => page.locator('[data-slot="topbar"]').getByRole("button", { name: /^Rules: / })
const ticks = (page: Page) => tickBar(page).locator('[data-slot="ticks"]')
/** The bar draws once the mode, the catalog and the inbox have answered; under a loaded suite that can take a few seconds. */
const drawn = (page: Page) => expect(tickBar(page)).toBeVisible({ timeout: 15_000 })

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: replays a stubbed cell`) })

test("top bar: the rules tick bar shows the served laws and fills only the share the inbox says is held back", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await cell(page, heldInbox)
  await page.goto("/map")
  await drawn(page)
  const bar = tickBar(page)
  const total = Number(await ticks(page).getAttribute("data-total"))
  expect(total).toBeGreaterThan(1)
  await expect(ticks(page)).toHaveAttribute("data-marked", "1")
  // The section counted 3 records and the inbox listed all 3, so the count is exact.
  await expect(bar).toHaveAccessibleName(`Rules: ${total}, 1 holding records back`)
  await expect(bar).toContainText(`1/${total}`)
  const filled = await bar.locator('[data-slot="ticks"] > [data-filled]').count()
  expect(filled).toBe(Math.min(9, Math.max(1, Math.round(10 / total))))
  await expect(bar.locator('[data-slot="ticks"] > span')).toHaveCount(10)
  // No tick reads as "satisfied": the cell sends no such verdict.
  await expect(bar.locator(".bg-dot-live")).toHaveCount(0)

  // Keyboard: focus, open, read, close; focus returns to the pill.
  await bar.focus()
  await expect(bar).toHaveCSS("outline-style", "solid")
  await expect(bar).toHaveCSS("outline-width", "2px")
  await page.keyboard.press("Enter")
  const popup = page.getByRole("dialog", { name: `${total} rules in this workspace` })
  await expect(popup).toBeVisible()
  const held = popup.locator('[data-item="canon.ItemizedDocument.total_matches_lines"]')
  await expect(held).toContainText("3 held back")
  await expect(held.locator('[data-slot="status-dot"][data-tone="held"]')).toBeVisible()
  // A rule the inbox does not list makes no claim: no dot, no word.
  await expect(popup.locator("li:not([data-item='canon.ItemizedDocument.total_matches_lines']) [data-slot='status-dot']")).toHaveCount(0)
  await expect(popup.getByRole("link", { name: "Open Rules" })).toHaveAttribute("href", "/rules")
  await page.keyboard.press("Escape")
  await expect(popup).toBeHidden()
  await expect(bar).toBeFocused()
})

test("top bar: an exact empty inbox reads 'nothing held back'; a failed inbox says so and claims nothing", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await cell(page, cleanInbox)
  await page.goto("/map")
  await drawn(page)
  const bar = tickBar(page)
  const total = Number(await ticks(page).getAttribute("data-total"))
  await expect(bar).toHaveAccessibleName(`Rules: ${total}, nothing held back`)
  await expect(bar.locator('[data-slot="ticks"] > [data-filled]')).toHaveCount(0)

  await page.unrouteAll({ behavior: "ignoreErrors" })
  await cell(page, "fail")
  await page.reload()
  await expect(tickBar(page)).toHaveAccessibleName(`Rules: ${total}, what they hold back could not be read`)
  await tickBar(page).click()
  await expect(page.getByRole("dialog")).toContainText("What the rules hold back could not be read.")
})

test("top bar: with no laws served there is no tick bar at all", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await cell(page, cleanInbox, definitions.filter(({ artifactId }) => !artifactId.endsWith(".law")))
  await page.goto("/map")
  await expect(page.locator('[data-slot="topbar"] [data-slot="language-pill"]')).toBeVisible()
  await expect(tickBar(page)).toHaveCount(0)
})

test("top bar: the language pill switches to Arabic and back, mirrors the bar, and works by keyboard", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await cell(page, heldInbox)
  await page.goto("/map")
  const pill = page.locator('[data-slot="topbar"] [data-slot="language-pill"]')
  await expect(pill).toHaveAccessibleName("Language: English. Switch to العربية")
  await expect(pill.locator('[lang="ar"]')).toHaveText("العربية")
  await pill.focus()
  await page.keyboard.press("Enter")
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl")
  await expect(page.locator("html")).toHaveAttribute("lang", "ar")
  await expect(pill.locator('[lang="en"]')).toHaveText("English")
  // In Arabic the extras sit at the bar's inline end, which is the left.
  const box = (await pill.boundingBox())!
  expect(box.x + box.width).toBeLessThan(720)
  const sidebar = (await page.locator('[data-slot="sidebar"]').first().boundingBox())!
  expect(sidebar.x).toBeGreaterThan(720)
  // Kept across a reload, and undone by the same pill.
  await page.reload()
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl")
  await pill.click()
  await expect(page.locator("html")).toHaveAttribute("dir", "ltr")
})

test("top bar: the scheme toggle pins light or dark, survives a reload, and Match system undoes it", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: "light" })
  await cell(page, heldInbox)
  await page.goto("/map")
  const toggle = page.locator('[data-slot="topbar"] [data-slot="scheme-toggle"]')
  await expect(toggle).toHaveAccessibleName("Switch to dark mode")
  await toggle.click()
  await expect(page.locator("html")).toHaveClass(/\bdark\b/)
  await expect(toggle).toHaveAccessibleName("Switch to light mode")
  await page.reload()
  await expect(page.locator("html")).toHaveClass(/\bdark\b/)
  // The OS changing does not override a pinned choice…
  await page.emulateMedia({ colorScheme: "light" })
  await expect(page.locator("html")).toHaveClass(/\bdark\b/)
  // …until the reader picks Match system again.
  await page.getByRole("button", { name: /Account$/ }).click()
  await page.getByRole("menuitemradio", { name: "Match system" }).click()
  await expect(page.locator("html")).not.toHaveClass(/\bdark\b/)
  await page.emulateMedia({ colorScheme: "dark" })
  await expect(page.locator("html")).toHaveClass(/\bdark\b/)
})

test("top bar: under reduced motion the extras switch instantly", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ reducedMotion: "reduce" })
  await cell(page, heldInbox)
  await page.goto("/map")
  await drawn(page)
  // `motion-reduce:transition-none` drops the transition itself (its property), so nothing animates.
  await expect(tickBar(page)).toHaveCSS("transition-property", "none")
  for (const slot of ["language-pill", "scheme-toggle"]) {
    await expect(page.locator(`[data-slot="topbar"] [data-slot="${slot}"]`)).toHaveCSS("transition-property", "none")
  }
})

test("top bar at 390px: the extras give way to the Language and view menu, which holds the language and the scheme", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: "light" })
  await cell(page, heldInbox)
  await page.goto("/map")
  await expect(tickBar(page)).toBeHidden()
  await expect(page.locator('[data-slot="topbar"] [data-slot="language-pill"]')).toBeHidden()
  await expect(page.locator('[data-slot="topbar"] [data-slot="scheme-toggle"]')).toBeHidden()
  const menu = page.getByRole("button", { name: "Language and view" })
  const box = (await menu.boundingBox())!
  expect(box.width).toBeGreaterThanOrEqual(44)
  expect(box.height).toBeGreaterThanOrEqual(44)
  await menu.click()
  await page.getByRole("menuitemradio", { name: "Dark" }).click()
  await expect(page.locator("html")).toHaveClass(/\bdark\b/)
  await menu.click()
  await page.getByRole("menuitemradio", { name: "العربية" }).click()
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl")
  // No horizontal page scroll at phone width.
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

test("dock at 390px: every destination shows its label under the icon and is at least 44px", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.addInitScript(() => { localStorage.setItem("norn.studio.shell", "dock") })
  await cell(page, heldInbox)
  await page.goto("/map")
  const dock = page.locator('[data-slot="floating-dock"]')
  await expect(dock).toBeVisible()
  const buttons = dock.locator('[role="toolbar"] > a, [role="toolbar"] > button, [role="toolbar"] [data-slot="chrome-dock-trigger"]')
  const count = await buttons.count()
  expect(count).toBeGreaterThan(2)
  for (let index = 0; index < count; index++) {
    const button = buttons.nth(index)
    const box = (await button.boundingBox())!
    expect(box.height, `dock button ${index} height`).toBeGreaterThanOrEqual(44)
    expect(box.width, `dock button ${index} width`).toBeGreaterThanOrEqual(44)
    const label = (await button.getAttribute("aria-label")) ?? ""
    const shown = (await button.innerText()).trim()
    expect(shown.length, `dock button ${index} shows a label`).toBeGreaterThan(0)
    expect(label.startsWith(shown) || label === "", `${shown} is part of ${label}`).toBe(true)
  }
  // At 640px and up the dock is icon-only again, 40px buttons.
  await page.setViewportSize({ width: 1024, height: 800 })
  const first = (await buttons.first().boundingBox())!
  expect(Math.round(first.width)).toBe(40)
  expect((await buttons.first().innerText()).trim()).toBe("")
})

test("list rows: Home's checklist rows are links at 44px on a phone, focus shows the 2px ring, and the arrow mirrors in Arabic", async ({ page }) => {
  // A workspace with only the trusted base: Home shows the Get started checklist.
  await cell(page, cleanInbox, definitions.filter(({ artifactId }) => artifactId.startsWith("core.") || artifactId.startsWith("std.")))
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  const rows = page.locator('[data-slot="row-list"] [data-slot="list-row"]')
  await expect(rows).toHaveCount(4)
  for (let index = 0; index < 4; index++) {
    const box = (await rows.nth(index).boundingBox())!
    expect(box.height).toBeGreaterThanOrEqual(44)
  }
  await expect(rows.first()).toHaveAttribute("href", "/sources")
  await expect(rows.first()).toHaveCSS("font-weight", "400")
  await rows.first().focus()
  await expect(rows.first()).toHaveCSS("outline-width", "2px")
  await expect(rows.first()).toHaveCSS("outline-style", "solid")
  await page.evaluate(() => { localStorage.setItem("norn.studio.locale", "ar") })
  await page.reload()
  const arrow = page.locator('[data-slot="list-row"] [data-slot="list-row-end"] svg').first()
  await expect(arrow).toHaveCSS("rotate", "180deg")
})
