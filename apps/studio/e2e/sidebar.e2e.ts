import { expect, test, type Locator, type Page } from "@playwright/test"
import { objectCell, population, session } from "./object-cell.ts"
import { workspaceCell } from "./workspace-cell.ts"

/**
 * The default shell (apps/studio/DESIGN.md 9.1): Polar and a thin source-list sidebar. One row per rule:
 * the frame and its rows, collapse to the 48px rail (pointer, ⌘B, persistence, focus), width defaults,
 * the keyboard tree, the phone sheet below 640px, RTL and reduced motion. SHOTS=<dir> also saves the
 * restyled surfaces in light and dark at desktop and phone widths.
 */

const SHOTS = process.env.SHOTS

async function recorded(page: Page) {
  // No session server answers the cell's reads: Studio opens the recorded world.
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
}

const nav = (page: Page) => page.getByRole("navigation", { name: "Studio navigation" })
const toggle = (page: Page) => page.getByRole("button", { name: "Sidebar", exact: true })
const column = (page: Page) => page.locator('[data-slot="sidebar"]:not([data-mobile])')
const settled = (page: Page) => expect(page.getByText("Connecting…")).toHaveCount(0)

/** A CSS colour as painted over `under` (default the sidebar ground), read back as `rgb(r, g, b)`. */
async function painted(page: Page, color: string, under?: string): Promise<string> {
  return page.evaluate(([fill, ground]) => {
    const canvas = document.createElement("canvas")
    canvas.width = 1
    canvas.height = 1
    const ctx = canvas.getContext("2d", { willReadFrequently: true })!
    ctx.fillStyle = ground ?? getComputedStyle(document.querySelector('[data-slot="sidebar"]')!).backgroundColor
    ctx.fillRect(0, 0, 1, 1)
    ctx.fillStyle = fill!
    ctx.fillRect(0, 0, 1, 1)
    const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data
    return `rgb(${r}, ${g}, ${b})`
  }, [color, under] as const)
}

const style = (locator: Locator, property: string) => locator.evaluate((element, name) => getComputedStyle(element).getPropertyValue(name), property)
const token = (page: Page, name: string) => page.evaluate((property) => getComputedStyle(document.documentElement).getPropertyValue(property).trim(), `--${name}`)

test.beforeEach(({ page }) => page.setViewportSize({ width: 1440, height: 900 }))

test("sidebar: the default is polar + sidebar, 232px with one landmark, one current row on the neutral pill at 400, quiet group headers, and a 50% hover wash that keeps the text colour", async ({ page }) => {
  await recorded(page)
  await page.goto("/decisions")
  await settled(page)
  await expect.poll(() => page.evaluate(() => ({ skin: document.documentElement.dataset.skin, shell: document.documentElement.dataset.shell }))).toEqual({ skin: "polar", shell: "sidebar" })
  await expect(nav(page)).toHaveCount(1)
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(232)
  // The toggle names itself and the column it controls.
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "true")
  await expect(toggle(page)).toHaveAttribute("aria-controls", "studio-sidebar")
  await expect(page.locator("#studio-sidebar")).toHaveCount(1)
  await expect(toggle(page)).not.toHaveAttribute("aria-haspopup", /.+/)

  // Exactly one row is current, and it is the neutral pill with its own ink at weight 400.
  const current = nav(page).locator('[aria-current="page"]')
  await expect(current).toHaveCount(1)
  await expect(current).toContainText("Inbox")
  const pill = current.locator('> [data-slot="tree-label"]')
  expect(await style(pill, "background-color").then((color) => painted(page, color))).toBe(await painted(page, await token(page, "sidebar-accent")))
  expect(await style(pill, "color")).toBe("rgb(24, 24, 27)")
  expect(await style(pill, "font-weight")).toBe("400")
  expect(await style(pill, "box-shadow")).toBe("none")
  expect((await pill.boundingBox())?.height).toBe(32)

  // A resting row is muted; on hover it takes a 50% wash and keeps its text colour (hover never looks selected).
  const home = nav(page).getByRole("link", { name: "Home" })
  const rest = await style(home, "color")
  expect(rest).toBe(await painted(page, await token(page, "muted-foreground")))
  await home.hover()
  // The wash fades in over 150ms, so it is read once it has painted: half the pill over the ground.
  const ground = await painted(page, await style(column(page), "background-color"))
  const pillColor = await painted(page, await token(page, "sidebar-accent"))
  const half = await page.evaluate(() => {
    const probe = document.createElement("span")
    probe.style.color = "color-mix(in oklab, var(--sidebar-accent) 50%, transparent)"
    document.body.append(probe)
    const color = getComputedStyle(probe).color
    probe.remove()
    return color
  })
  const wash = await painted(page, half)
  await expect.poll(() => style(home, "background-color").then((color) => painted(page, color))).toBe(wash)
  expect(wash).not.toBe(ground)
  expect(wash).not.toBe(pillColor)
  expect(await style(home, "color")).toBe(rest)

  // A group header is a quiet step-1 label at 400 in sentence case with a 32px disclosure target.
  const space = nav(page).locator('[role="treeitem"][aria-expanded]', { hasText: "Data" }).first()
  const header = space.locator('> [data-slot="tree-label"]')
  expect(await style(header, "font-weight")).toBe("400")
  expect(await style(header, "text-transform")).toBe("none")
  expect(Number.parseFloat(await style(header, "font-size"))).toBeCloseTo(12.096, 1)
  expect((await header.boundingBox())?.height).toBe(32)
  // The content column is the unframed canvas, beside a hairline on the sidebar's inline-end edge.
  expect(await style(column(page), "border-right-width")).toBe("1px")
  expect(await style(page.locator('[data-slot="sidebar-inset"]'), "border-top-width")).toBe("0px")
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/sidebar-expanded-light.png` })
})

test("sidebar: the toggle and ⌘B collapse it to a 48px rail of 32px icons and back, the choice survives a reload, and focus is never stranded", async ({ page }) => {
  await recorded(page)
  await page.goto("/decisions")
  await settled(page)
  // Collapse from the keyboard while focus is in the list: focus moves to the current rail icon.
  await nav(page).getByRole("treeitem", { name: /Inbox/ }).focus()
  await page.keyboard.press("Control+b")
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(48)
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "false")
  await expect(nav(page).getByRole("tree")).toHaveCount(0)
  const rail = nav(page).getByRole("toolbar")
  const current = rail.locator('[aria-current="page"]')
  await expect(current).toHaveCount(1)
  await expect(current).toHaveAttribute("aria-label", "Inbox")
  await expect(current).toBeFocused()
  const box = await current.boundingBox()
  expect([box?.width, box?.height]).toEqual([32, 32])
  expect(await style(current, "background-color").then((color) => painted(page, color))).toBe(await painted(page, await token(page, "sidebar-accent")))
  // The rail keeps one landmark and every destination; spaces become hairline-led groups.
  await expect(rail.getByRole("group").first()).toHaveAttribute("aria-label", /.+/)
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/sidebar-rail-light.png` })

  // A collapse from the footer hands focus to the same control on the rail.
  await toggle(page).click()
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(232)
  await page.locator('[data-slot="sidebar"]').getByRole("button", { name: "Account" }).focus()
  await page.keyboard.press("Control+b")
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(48)
  await expect(page.locator('[data-slot="sidebar"]').getByRole("button", { name: "Account" })).toBeFocused()

  // The reader's choice is kept per browser.
  await page.reload()
  await settled(page)
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(48)

  // A destination with no page of its own expands the sidebar and lands on its row.
  await page.goto("/")
  await settled(page)
  await nav(page).getByRole("toolbar").getByRole("button", { name: "Records" }).click()
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(232)
  await expect(page.locator(':focus')).toHaveAttribute("data-section-path", "/objects")

  // The toggle collapses and expands too, and the expanded choice also survives a reload.
  await toggle(page).click()
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "false")
  await toggle(page).click()
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "true")
  await page.reload()
  await settled(page)
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(232)
})

test("sidebar: it starts as the rail from 640 to 1023px and expanded from 1024px, and a stored choice wins at both", async ({ page }) => {
  await recorded(page)
  await page.setViewportSize({ width: 900, height: 800 })
  await page.goto("/")
  await settled(page)
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(48)
  await page.setViewportSize({ width: 1100, height: 800 })
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(232)
  // Collapse at 1100, then the stored choice holds at 1100 and expanding at 900 holds at 900.
  await toggle(page).click()
  await page.reload()
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(48)
  await page.setViewportSize({ width: 900, height: 800 })
  await toggle(page).click()
  await page.reload()
  await expect.poll(async () => (await column(page).boundingBox())?.width).toBe(232)
})

test("sidebar: the source list is a keyboard tree (arrows, Right/Left, Home/End) with a 2px ring drawn inside the 8px gutter", async ({ page }) => {
  await recorded(page)
  await page.goto("/")
  await settled(page)
  const tree = nav(page).getByRole("tree")
  const items = tree.getByRole("treeitem")
  await items.first().focus()
  await expect(items.first()).toContainText("Home")
  await page.keyboard.press("ArrowDown")
  await expect(page.locator(":focus")).toContainText("Inbox")
  const label = page.locator(':focus > [data-slot="tree-label"]')
  expect(await style(label, "outline-style")).toBe("solid")
  expect(await style(label, "outline-width")).toBe("2px")
  // The ring fades in with the row's colour transition, so it is read once settled.
  const ring = await painted(page, await token(page, "ring"))
  await expect.poll(() => style(label, "outline-color").then((color) => painted(page, color))).toBe(ring)
  // The ring (2px at a 1px offset) stays inside the list's 8px gutter, so the scroller never clips it.
  const [row, list] = await Promise.all([label.boundingBox(), nav(page).boundingBox()])
  expect(row!.x - 3).toBeGreaterThanOrEqual(list!.x)
  expect(row!.x + row!.width + 3).toBeLessThanOrEqual(list!.x + list!.width)
  // Down onto the first space, then Left closes it and Right opens it again.
  const space = tree.locator('[role="treeitem"][aria-expanded]').first()
  for (let step = 0; step < 4 && !(await space.evaluate((element) => element === document.activeElement)); step += 1) await page.keyboard.press("ArrowDown")
  await expect(space).toBeFocused()
  await expect(space).toHaveAttribute("aria-expanded", "true")
  await page.keyboard.press("ArrowLeft")
  await expect(space).toHaveAttribute("aria-expanded", "false")
  await page.keyboard.press("ArrowRight")
  await expect(space).toHaveAttribute("aria-expanded", "true")
  await page.keyboard.press("End")
  await expect(items.last()).toBeFocused()
  await page.keyboard.press("Home")
  await expect(items.first()).toBeFocused()
  // Enter follows the link.
  await page.keyboard.press("ArrowDown")
  await page.keyboard.press("Enter")
  await expect(page).toHaveURL(/\/decisions$/)
})

test("sidebar below 640px: the toggle opens a modal 'Navigation' sheet from the start edge with 44px targets; Escape, the close button and a followed link close it and focus returns to the toggle", async ({ page }) => {
  await recorded(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await settled(page)
  // The column leaves the layout; the toggle opens a dialog and names it only while it is open.
  await expect(page.locator("#studio-sidebar")).toHaveCount(0)
  await expect(toggle(page)).toHaveAttribute("aria-haspopup", "dialog")
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "false")
  await expect(toggle(page)).not.toHaveAttribute("aria-controls", /.+/)
  const trigger = await toggle(page).boundingBox()
  expect(trigger!.width).toBeGreaterThanOrEqual(44)
  expect(trigger!.height).toBeGreaterThanOrEqual(44)

  await toggle(page).click()
  const sheet = page.getByRole("dialog", { name: "Navigation" })
  await expect(sheet).toBeVisible()
  // The page behind the modal sheet is inert, so the toggle is read by its slot, not its role.
  const inert = page.locator('button[aria-label="Sidebar"]')
  await expect(inert).toHaveAttribute("aria-expanded", "true")
  await expect(inert).toHaveAttribute("aria-controls", "studio-sidebar-sheet")
  await expect(sheet).toHaveAttribute("id", "studio-sidebar-sheet")
  // Inset 8px from the start edge, a strip of the page left visible.
  await expect.poll(async () => { const b = await sheet.boundingBox(); return [Math.round(b!.x), Math.round(b!.y), Math.round(b!.width)] }).toEqual([8, 8, 320])
  // Every target is at least 44 × 44 CSS px; the disclosure chevrons always show.
  for (const target of await sheet.locator('[data-slot="tree-label"]').all()) {
    if (!(await target.isVisible())) continue
    expect((await target.boundingBox())!.height, await target.innerText()).toBeGreaterThanOrEqual(44)
  }
  const close = sheet.getByRole("button", { name: "Close navigation" })
  const closeBox = await close.boundingBox()
  expect([closeBox!.width, closeBox!.height]).toEqual([44, 44])
  for (const indicator of await sheet.locator('[role="treeitem"][aria-expanded] > [data-slot="tree-label"] > :last-child').all()) {
    expect(await style(indicator, "opacity")).toBe("1")
  }
  // Focus is trapped in the sheet: past its last control Base UI's focus guard hands it back on the next frame.
  for (let step = 0; step < 25; step += 1) {
    await page.keyboard.press("Tab")
    await expect.poll(() => page.evaluate(() => document.activeElement?.closest('[role="dialog"]')?.id ?? document.activeElement?.outerHTML.slice(0, 120))).toBe("studio-sidebar-sheet")
  }
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/sidebar-sheet-390-light.png` })
  await page.keyboard.press("Escape")
  await expect(sheet).toHaveCount(0)
  await expect(toggle(page)).toBeFocused()
  await expect(toggle(page)).not.toHaveAttribute("aria-controls", /.+/)

  await toggle(page).click()
  await sheet.getByRole("button", { name: "Close navigation" }).click()
  await expect(sheet).toHaveCount(0)
  await expect(toggle(page)).toBeFocused()

  await toggle(page).click()
  await sheet.getByRole("link", { name: /Inbox/ }).click()
  await expect(page).toHaveURL(/\/decisions$/)
  await expect(sheet).toHaveCount(0)
})

test("sidebar in Arabic: it sits on the right with its hairline on the inline-end edge, the toggle mirrors, and the phone sheet opens from the right", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("norn.studio.locale", "ar"))
  await recorded(page)
  await page.goto("/")
  await settled(page)
  await expect.poll(() => page.evaluate(() => document.documentElement.dir)).toBe("rtl")
  const [side, main] = await Promise.all([column(page).boundingBox(), page.locator('[data-slot="sidebar-inset"]').boundingBox()])
  expect(side!.x).toBeGreaterThan(main!.x)
  expect(Math.round(side!.x + side!.width)).toBe(1440)
  expect(await style(column(page), "border-left-width")).toBe("1px")
  expect(await style(column(page), "border-right-width")).toBe("0px")
  expect(await style(toggle(page).locator("svg"), "scale")).toBe("-1 1")
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/sidebar-rtl-light.png` })
  await page.setViewportSize({ width: 390, height: 844 })
  await toggle(page).click()
  const sheet = page.getByRole("dialog", { name: "Navigation" })
  await expect.poll(async () => { const b = await sheet.boundingBox(); return Math.round(b!.x + b!.width) }).toBe(390 - 8)
})

test("sidebar under reduced motion: collapsing, rows and the phone sheet switch instantly", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" })
  await recorded(page)
  await page.goto("/")
  await settled(page)
  expect(await style(column(page), "transition-property")).toBe("none")
  expect(await style(nav(page).locator('[data-slot="tree-label"]').first(), "transition-property")).toBe("none")
  await page.setViewportSize({ width: 390, height: 844 })
  await toggle(page).click()
  const sheet = page.getByRole("dialog", { name: "Navigation" })
  await expect(sheet).toBeVisible()
  expect(await style(sheet, "transition-property")).toBe("none")
})

test("notifications say honestly that the cell sends none yet, and point to the Inbox", async ({ page }) => {
  await recorded(page)
  await page.goto("/")
  await settled(page)
  await page.getByRole("button", { name: "Notifications" }).click()
  const note = page.locator('[data-notifications="unavailable"]')
  await expect(note).toContainText("doesn't receive notifications")
  await expect(note).not.toContainText("Nothing yet")
  await note.getByRole("link", { name: "Inbox" }).click()
  await expect(page).toHaveURL(/\/decisions$/)
})

/** The restyled surfaces in light and dark, at desktop and phone widths, for the PR (only with SHOTS). */
test.describe("restyle screenshots", () => {
  test.skip(!SHOTS, "SHOTS=<dir> saves the restyled surfaces")
  test.skip(Boolean(process.env.E2E_BASE), "uses the object and workspace stand-ins")

  async function both(page: Page, name: string, ready: () => Promise<void>) {
    for (const [width, height, size] of [[1440, 900, "desktop"], [390, 844, "390"]] as const) {
      await page.setViewportSize({ width, height })
      for (const scheme of ["light", "dark"] as const) {
        await page.emulateMedia({ colorScheme: scheme })
        await ready()
        await page.waitForTimeout(400)
        await page.screenshot({ path: `${SHOTS}/${name}-${size}-${scheme}.png` })
      }
    }
    await page.emulateMedia({ colorScheme: "light" })
  }

  test("shell and home", async ({ page }) => {
    await recorded(page)
    await page.goto("/")
    await settled(page)
    await both(page, "home", () => expect(page.getByRole("heading", { name: "Home", level: 1 })).toBeVisible())
  })

  test("object list, and a record with its detail sheet open", async ({ page }) => {
    await session(page)
    await objectCell(page, population(40))
    await page.goto("/objects/canon.Holding")
    await expect(page.getByText("40 records", { exact: true })).toBeVisible()
    await both(page, "object-list", () => expect(page.getByRole("grid", { name: "Holding records" })).toBeVisible())
    await page.setViewportSize({ width: 1440, height: 900 })
    const grid = page.getByRole("grid", { name: "Holding records" })
    await grid.focus()
    await page.keyboard.press("Enter")
    const drawer = page.getByRole("dialog")
    await expect(drawer.getByRole("heading", { name: "demo/holding/0001" })).toBeVisible()
    await both(page, "record-sheet", () => expect(drawer).toBeVisible())
  })

  test("views page", async ({ page }) => {
    await workspaceCell(page)
    await page.goto("/views")
    await expect(page.getByRole("heading", { name: "Views", level: 1 })).toBeVisible()
    await both(page, "views", () => expect(page.getByRole("heading", { name: "Views", level: 1 })).toBeVisible())
  })
})
