import { expect, test, type Locator, type Page } from "@playwright/test"
import { objectCell, population, session } from "./object-cell.ts"
import { recordCell } from "./record-cell.ts"
import { workspaceCell } from "./workspace-cell.ts"

/**
 * The three-island shell (apps/studio/DESIGN.md 9.1; owner decision, 2026-11-25): luma, an applet rail,
 * the open applet's submenu island and the main island, on the backdrop. One row per behaviour: the
 * frame and its rows, the rail's roving focus and `aria-current`, collapsing the submenu (pointer, ⌘B,
 * persistence, focus, width defaults), the menu's headings (pointer, Left/Right, Home/End, what holds
 * them open and for how long), Show all and the filter, the phone sheet, RTL, reduced motion,
 * the opaque fallbacks, and one Workspace unavailable. SHOTS=<dir> also saves the shell's surfaces in
 * light and dark at 1440 and 390.
 */

const SHOTS = process.env.SHOTS

async function recorded(page: Page) {
  // No session server answers the cell's reads: Studio opens the recorded world.
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
}

const rail = (page: Page) => page.getByRole("navigation", { name: "Applets" })
const menu = (page: Page, applet: string) => page.getByRole("navigation", { name: `${applet} menu` })
const toggle = (page: Page) => page.getByRole("button", { name: "Sidebar", exact: true })
const island = (page: Page) => page.locator('[data-slot="sidebar"]:not([data-mobile])')
const railIsland = (page: Page) => page.locator('[data-slot="sidebar-rail"]')
const main = (page: Page) => page.locator('[data-slot="sidebar-inset"]')
const current = (page: Page) => rail(page).locator('[aria-current="page"]')
const settled = (page: Page) => expect(page.getByText("Connecting…", { exact: true })).toHaveCount(0)
const width = async (locator: Locator) => Math.round((await locator.boundingBox())?.width ?? 0)

/** A CSS colour as painted over `under` (default the submenu island's own colour over the backdrop), read back as `rgb(r, g, b)`. */
async function painted(page: Page, color: string, under?: string): Promise<string> {
  return page.evaluate(([fill, ground]) => {
    const canvas = document.createElement("canvas")
    canvas.width = 1
    canvas.height = 1
    const ctx = canvas.getContext("2d", { willReadFrequently: true })!
    ctx.fillStyle = getComputedStyle(document.querySelector('[data-slot="sidebar-wrapper"]')!).backgroundColor
    ctx.fillRect(0, 0, 1, 1)
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

test("islands: luma by default, a 64px applet rail, a 256px submenu island and the main island floating 8px apart on the backdrop, two landmarks, one current applet and one current row on the neutral pill at 400", async ({ page }) => {
  await recorded(page)
  await page.goto("/sources")
  await settled(page)
  await expect.poll(() => page.evaluate(() => ({ skin: document.documentElement.dataset.skin, shell: document.documentElement.dataset.shell }))).toEqual({ skin: "luma", shell: "sidebar" })
  // Geometry: three islands, 8px apart and 8px from the window, each an opaque rounded panel (luma has
  // no glass since the monochrome rule, DESIGN.md 19.6; the glass skin keeps it).
  const [r, s, m] = await Promise.all([railIsland(page).boundingBox(), island(page).boundingBox(), main(page).boundingBox()])
  expect([Math.round(r!.x), Math.round(r!.y), Math.round(r!.width)]).toEqual([8, 8, 64])
  expect([Math.round(s!.x), Math.round(s!.width)]).toEqual([80, 256])
  expect(Math.round(m!.x)).toBe(344)
  expect(Math.round(m!.x + m!.width)).toBe(1440 - 8)
  for (const each of [railIsland(page), island(page), main(page)]) {
    await expect(each).toHaveCSS("border-top-left-radius", "16px")
    await expect(each).toHaveCSS("backdrop-filter", "none")
  }
  // Two landmarks: the applets, and the open applet's menu.
  await expect(rail(page)).toHaveCount(1)
  await expect(menu(page, "Builder")).toHaveCount(1)
  await expect(page.getByRole("navigation", { name: "Studio navigation" })).toHaveCount(0)
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "true")
  await expect(toggle(page)).toHaveAttribute("aria-controls", "studio-sidebar")
  // Exactly one applet is current, on the neutral pill with its own ink.
  await expect(current(page)).toHaveCount(1)
  await expect(current(page)).toHaveAttribute("data-applet", "builder")
  expect(await style(current(page), "background-color").then((color) => painted(page, color))).toBe(await painted(page, await token(page, "sidebar-accent")))
  // Exactly one menu row is current: the neutral pill with its own ink at weight 400.
  const row = menu(page, "Builder").locator('[aria-current="page"]')
  await expect(row).toHaveCount(1)
  await expect(row).toContainText("Sources")
  const pill = row.locator('> [data-slot="tree-label"]')
  expect(await style(pill, "background-color").then((color) => painted(page, color))).toBe(await painted(page, await token(page, "sidebar-accent")))
  expect(await style(pill, "font-weight")).toBe("400")
  expect(await style(pill, "box-shadow")).toBe("none")
  expect((await pill.boundingBox())?.height).toBe(32)
  // A resting row is muted; on hover it takes a 50% wash and keeps its text colour.
  const transforms = menu(page, "Builder").getByRole("link", { name: "Transforms" })
  const rest = await style(transforms, "color")
  expect(rest).toBe(await painted(page, await token(page, "muted-foreground")))
  await transforms.hover()
  const half = await page.evaluate(() => {
    const probe = document.createElement("span")
    probe.style.color = "color-mix(in oklab, var(--sidebar-accent) 50%, transparent)"
    document.body.append(probe)
    const color = getComputedStyle(probe).color
    probe.remove()
    return color
  })
  await expect.poll(() => style(transforms, "background-color").then((color) => painted(page, color))).toBe(await painted(page, half))
  expect(await style(transforms, "color")).toBe(rest)
  // A heading is a quiet step-1 label at 400 in sentence case with a 32px disclosure target.
  const header = menu(page, "Builder").locator('[role="treeitem"][aria-expanded]', { hasText: "Model" }).first().locator('> [data-slot="tree-label"]')
  expect(await style(header, "font-weight")).toBe("400")
  expect(await style(header, "text-transform")).toBe("none")
  expect(Number.parseFloat(await style(header, "font-size"))).toBeCloseTo(12.096, 1)
  expect((await header.boundingBox())?.height).toBe(32)
  // The workspace's state is visible text under the island's title, never only the rail disc's tooltip.
  await expect(island(page).locator("[data-workspace-state]")).toContainText(/\S · \S/)
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/islands-builder-light.png` })
})

test("rail: one tab stop with roving focus (arrows, Home, End), Enter opens the applet, aria-current follows, and each applet names itself and its count", async ({ page }) => {
  await recorded(page)
  await page.goto("/")
  await settled(page)
  await expect(current(page)).toHaveAttribute("data-applet", "/")
  const links = rail(page).locator("a[data-applet]")
  const names = await links.evaluateAll((all) => all.map((link) => link.getAttribute("aria-label")))
  expect(names.slice(0, 2)).toEqual(["Home", expect.stringMatching(/^Inbox(, .+ open)?$/)])
  expect(names).toContain("Builder")
  // One tab stop: Tab from the workspace switcher lands in the rail, and the next Tab leaves it.
  await railIsland(page).locator("[data-scope]").focus()
  await page.keyboard.press("Tab")
  await expect(page.locator(":focus")).toHaveAttribute("data-applet", /.+/)
  const first = await page.locator(":focus").getAttribute("data-applet")
  await page.keyboard.press("ArrowDown")
  await expect(page.locator(":focus")).not.toHaveAttribute("data-applet", first!)
  await page.keyboard.press("End")
  await expect(links.last()).toBeFocused()
  await page.keyboard.press("Home")
  await expect(links.first()).toBeFocused()
  // Arrow to Builder and follow it: the page, the current applet and the menu all move.
  for (let step = 0; step < names.length && (await page.locator(":focus").getAttribute("data-applet")) !== "builder"; step += 1) await page.keyboard.press("ArrowDown")
  await page.keyboard.press("Enter")
  await expect(page).toHaveURL(/\/sources$/)
  await expect(current(page)).toHaveCount(1)
  await expect(current(page)).toHaveAttribute("data-applet", "builder")
  await expect(menu(page, "Builder")).toBeVisible()
  // The tooltip names the applet; the focus ring is the 2px --ring outline.
  await expect(current(page)).toHaveCSS("outline-width", "2px")
  await current(page).hover()
  await expect(page.getByRole("tooltip")).toContainText("Builder")
})

test("rail: a page two applets list opens in the same applet on every fresh load, and a type's own page is the model's", async ({ page }) => {
  await recorded(page)
  // A type's record page is listed by the records (Data) and by the model (Builder); a fresh load settles on
  // one of them however the two menus' reads race, and stays there while they load.
  for (let attempt = 0; attempt < 3; attempt += 1) {
    await page.goto("/sources/canon.Holding/demo%2Fholding%2F0001")
    await settled(page)
    await expect(current(page)).toHaveAttribute("data-applet", "data")
  }
  // The type's own page is the model's item: opened from Builder's menu it is Builder's.
  await rail(page).getByRole("link", { name: "Builder" }).click()
  await menu(page, "Builder").getByRole("link", { name: "Holding" }).click()
  await expect(page).toHaveURL(/\/sources\/canon\.Holding$/)
  await expect(current(page)).toHaveAttribute("data-applet", "builder")
  // The rail opens the applet it names, even when its first page is one Builder lists too, and the
  // browser's Back returns to Builder.
  await rail(page).getByRole("link", { name: /^Data(,|$)/ }).click()
  await expect(page).toHaveURL(/\/objects\//)
  await expect(current(page)).toHaveAttribute("data-applet", "data")
  await expect(menu(page, "Data")).toBeVisible()
  await page.goBack()
  await expect(page).toHaveURL(/\/sources\/canon\.Holding$/)
  await expect(current(page)).toHaveAttribute("data-applet", "builder")
})

test("submenu island: the toggle, ⌘B and Ctrl+B collapse it to nothing (rail and main island stay), the choice survives a reload, focus is never stranded, and 640–1023px starts collapsed", async ({ page }) => {
  await recorded(page)
  await page.goto("/sources")
  await settled(page)
  // Collapse from the keyboard while focus is in the menu: focus moves to the current applet on the rail.
  await menu(page, "Builder").getByRole("treeitem", { name: /Sources/ }).first().focus()
  await page.keyboard.press("Control+b")
  await expect.poll(() => width(island(page))).toBe(0)
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "false")
  await expect(island(page)).toHaveAttribute("inert", "")
  await expect(current(page)).toBeFocused()
  await expect.poll(async () => Math.round((await main(page).boundingBox())!.x)).toBe(80)
  await expect(railIsland(page)).toBeVisible()
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/islands-collapsed-light.png` })
  // Kept per browser.
  await page.reload()
  await settled(page)
  await expect.poll(() => width(island(page))).toBe(0)
  // ⌘B (Meta) expands it again, and that choice is kept too.
  await page.keyboard.press("Meta+b")
  await expect.poll(() => width(island(page))).toBe(256)
  await page.reload()
  await settled(page)
  await expect.poll(() => width(island(page))).toBe(256)
  // The toggle collapses and expands too; focus outside the island stays where it is.
  await toggle(page).click()
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "false")
  await expect(toggle(page)).toBeFocused()
  await toggle(page).click()
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "true")
  // Width defaults: from 640 to 1023px it starts collapsed, from 1024px shown; a stored choice wins at both.
  await page.evaluate(() => localStorage.removeItem("norn.studio.sidebar"))
  await page.setViewportSize({ width: 900, height: 800 })
  await page.reload()
  await expect.poll(() => width(island(page))).toBe(0)
  await page.setViewportSize({ width: 1100, height: 800 })
  await expect.poll(() => width(island(page))).toBe(256)
  await toggle(page).click()
  await page.reload()
  await expect.poll(() => width(island(page))).toBe(0)
})

test("submenu island: a long list shows 8 rows and Show all; Show all and Show fewer toggle by pointer and keyboard; the filter narrows rows, says when nothing matches, and Escape clears it", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "uses the object stand-in cell")
  await session(page)
  await objectCell(page, population(12))
  await page.goto("/objects/canon.Holding")
  await expect(current(page)).toHaveAttribute("data-applet", "data")
  const data = menu(page, "Data")
  const records = data.locator('[data-section-path="/objects"] > [role="group"] > [role="treeitem"]:not([data-more])')
  const more = data.locator('[data-more] > [data-slot="tree-label"]')
  await expect(more).toHaveText(/^Show all \d+$/)
  const all = Number((await more.innerText()).replace(/\D/g, ""))
  expect(all).toBeGreaterThan(9)
  // The first eight, plus the current type wherever it falls.
  await expect.poll(() => records.count()).toBeLessThanOrEqual(9)
  await expect(data.locator('[aria-current="page"]')).toContainText("Holding")
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/islands-data-capped-light.png` })
  await more.click()
  await expect.poll(() => records.count()).toBe(all)
  await expect(more).toHaveText("Show fewer")
  // Show all persists per browser across reload (DESIGN.md 18).
  await page.reload()
  await expect(more).toHaveText("Show fewer")
  await expect.poll(() => records.count()).toBe(all)
  // By keyboard: the row is a tree item; Enter toggles it back, and that persists too.
  await data.locator("[data-more]").focus()
  await page.keyboard.press("Enter")
  await expect(more).toHaveText(`Show all ${all}`)
  await expect.poll(() => records.count()).toBeLessThanOrEqual(9)
  // The filter narrows the rows (case and accents fold) and lists every match, uncapped.
  const filter = page.getByRole("searchbox", { name: "Filter Data" })
  await filter.fill("HOLD")
  await expect(data.getByRole("link", { name: /Holding/ }).first()).toBeVisible()
  await expect(data.locator("[data-more]")).toHaveCount(0)
  await filter.fill("zzz-no-such-row")
  await expect(data.getByRole("status")).toHaveText("Nothing matches “zzz-no-such-row”.")
  // Escape empties it, and Down moves from the field into the list.
  await page.keyboard.press("Escape")
  await expect(filter).toHaveValue("")
  await expect(data.locator("[data-more]")).toHaveCount(1)
  await filter.focus()
  await page.keyboard.press("ArrowDown")
  await expect(page.locator(":focus")).toHaveAttribute("role", "treeitem")
})

test("submenu island: a heading collapses and reopens by pointer and by Left/Right with aria-expanded on the heading, Home and End reach the first and last row, and the reader's choice holds while moving inside the applet and across an applet switch, never hides the current row, and lasts while Studio is open", async ({ page }) => {
  await recorded(page)
  await page.goto("/sources")
  await settled(page)
  const builder = menu(page, "Builder")
  const rows = builder.getByRole("treeitem")
  const heading = (name: string) => builder.getByRole("treeitem", { name, exact: true })
  const model = heading("Model")
  const logic = heading("Logic")
  const logicLabel = logic.locator(':scope > [data-slot="tree-label"]')
  const canvas = builder.locator('[data-section-path="/canvas"]')
  const markings = builder.locator('[data-section-path="/markings"]')
  // Headings start open, and the state is on the heading itself, not only on the island toggle.
  await expect(model).toHaveAttribute("aria-expanded", "true")
  await expect(logic).toHaveAttribute("aria-expanded", "true")
  await expect(canvas).toBeVisible()
  // Pointer: a click on the heading closes it and hides its rows; a second click opens it again.
  await logicLabel.click()
  await expect(logic).toHaveAttribute("aria-expanded", "false")
  await expect(canvas).toBeHidden()
  await logicLabel.click()
  await expect(logic).toHaveAttribute("aria-expanded", "true")
  await expect(canvas).toBeVisible()
  // Keyboard: Left closes the focused heading, Right opens it.
  await logic.focus()
  await page.keyboard.press("ArrowLeft")
  await expect(logic).toHaveAttribute("aria-expanded", "false")
  await expect(canvas).toBeHidden()
  await expect(logic).toBeFocused()
  await page.keyboard.press("ArrowRight")
  await expect(logic).toHaveAttribute("aria-expanded", "true")
  await expect(canvas).toBeVisible()
  // Home and End reach the first and the last row: with Logic open the last is its last destination…
  await page.keyboard.press("End")
  await expect(markings).toBeFocused()
  await expect(rows.last()).toBeFocused()
  await page.keyboard.press("Home")
  await expect(model).toBeFocused()
  await expect(rows.first()).toBeFocused()
  // …and with Logic closed it is the heading itself, since its rows are gone.
  await logic.focus()
  await page.keyboard.press("ArrowLeft")
  await expect(logic).toHaveAttribute("aria-expanded", "false")
  await page.keyboard.press("Home")
  await expect(model).toBeFocused()
  await page.keyboard.press("End")
  await expect(logic).toBeFocused()
  await expect(rows.last()).toBeFocused()
  // The choice holds while moving inside the applet…
  await builder.getByRole("link", { name: "Semantic map" }).click()
  await expect(page).toHaveURL(/\/map$/)
  await expect(model).toHaveAttribute("aria-expanded", "true")
  await expect(logic).toHaveAttribute("aria-expanded", "false")
  await expect(canvas).toBeHidden()
  // …and across a switch to another applet and back, though Builder's menu leaves the page meanwhile.
  await rail(page).getByRole("link", { name: /^Data(,|$)/ }).click()
  await expect(current(page)).toHaveAttribute("data-applet", "data")
  await expect(builder).toHaveCount(0)
  await rail(page).getByRole("link", { name: "Builder" }).click()
  await expect(current(page)).toHaveAttribute("data-applet", "builder")
  await expect(logic).toHaveAttribute("aria-expanded", "false")
  await expect(canvas).toBeHidden()
  // The current row is never hidden: a page under the closed heading holds it open, even against a click…
  const filter = page.getByRole("searchbox", { name: "Filter Builder" })
  await filter.fill("Canvas")
  await builder.getByRole("link", { name: "Canvas" }).click()
  await expect(page).toHaveURL(/\/canvas$/)
  await filter.fill("")
  await expect(logic).toHaveAttribute("aria-expanded", "true")
  await expect(canvas).toHaveAttribute("aria-current", "page")
  await logicLabel.click()
  await expect(logic).toHaveAttribute("aria-expanded", "true")
  await expect(canvas).toBeVisible()
  // …and once the reader leaves it, their choice to keep Logic closed applies again.
  await builder.locator('[data-section-path="/map"] > [data-slot="tree-label"]').click()
  await expect(page).toHaveURL(/\/map$/)
  await expect(logic).toHaveAttribute("aria-expanded", "false")
  // Persisted per browser across reload (DESIGN.md 18).
  await page.reload()
  await settled(page)
  await expect(logic).toHaveAttribute("aria-expanded", "false")
  await expect(canvas).toBeHidden()
  // Clearing stored menu choices restores the open defaults.
  await page.evaluate(() => localStorage.removeItem("norn.studio.menu"))
  await page.reload()
  await settled(page)
  await expect(logic).toHaveAttribute("aria-expanded", "true")
  await expect(canvas).toBeVisible()
})

test("submenu island: a destination with no page whose items were read and are none says so", async ({ page }) => {
  await session(page)
  // A live cell with no definitions: the records list is read, and empty.
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: [] } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
  await page.goto("/views")
  await settled(page)
  await expect(menu(page, "Data").locator('[data-section-path="/objects"] [data-empty]')).toHaveText("None yet")
})

test("below 640px: no rail; the toggle opens one modal 'Navigation' sheet with the applets beside the open applet's menu, 44px targets; choosing an applet shows its menu; Escape, the close button and a followed row close it and focus returns to the toggle", async ({ page }) => {
  await recorded(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await settled(page)
  // The rail and the submenu island leave the layout; the main island fills the window.
  await expect(railIsland(page)).toHaveCount(0)
  await expect(page.locator("#studio-sidebar")).toHaveCount(0)
  await expect.poll(async () => { const b = await main(page).boundingBox(); return [Math.round(b!.x), Math.round(b!.width)] }).toEqual([0, 390])
  await expect(toggle(page)).toHaveAttribute("aria-haspopup", "dialog")
  await expect(toggle(page)).toHaveAttribute("aria-expanded", "false")
  await expect(toggle(page)).not.toHaveAttribute("aria-controls", /.+/)
  const trigger = await toggle(page).boundingBox()
  expect([trigger!.width >= 44, trigger!.height >= 44]).toEqual([true, true])

  await toggle(page).click()
  const sheet = page.getByRole("dialog", { name: "Navigation" })
  await expect(sheet).toBeVisible()
  await expect(page.locator('button[aria-label="Sidebar"]')).toHaveAttribute("aria-controls", "studio-sidebar-sheet")
  await expect.poll(async () => { const b = await sheet.boundingBox(); return [Math.round(b!.x), Math.round(b!.y), Math.round(b!.width)] }).toEqual([8, 8, 334])
  // Both landmarks live in the sheet; the open applet's menu shows first.
  const applets = sheet.getByRole("navigation", { name: "Applets" })
  await expect(applets.locator('[aria-current="page"]')).toHaveAttribute("data-applet", "/")
  await expect(applets.locator('[aria-pressed="true"]')).toHaveAttribute("data-applet", "/")
  await expect(sheet.getByRole("navigation", { name: "Home menu" })).toBeVisible()
  // Every target is at least 44 × 44 CSS px.
  for (const target of [...await applets.locator("button").all(), ...await sheet.locator('[data-slot="tree-label"]').all()]) {
    if (!(await target.isVisible())) continue
    const box = (await target.boundingBox())!
    expect(box.height, await target.getAttribute("aria-label") ?? await target.innerText()).toBeGreaterThanOrEqual(44)
  }
  // Choosing an applet shows its menu beside the applets, without leaving the page.
  await applets.locator('[data-applet="builder"]').click()
  await expect(applets.locator('[aria-pressed="true"]')).toHaveAttribute("data-applet", "builder")
  await expect(applets.locator('[aria-current="page"]')).toHaveAttribute("data-applet", "/")
  await expect(sheet.getByRole("navigation", { name: "Builder menu" })).toBeVisible()
  await expect(page).toHaveURL(/\/$/)
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/islands-phone-sheet-light.png` })
  // Focus is trapped in the sheet.
  for (let step = 0; step < 20; step += 1) {
    await page.keyboard.press("Tab")
    await expect.poll(() => page.evaluate(() => document.activeElement?.closest('[role="dialog"]')?.id ?? "")).toBe("studio-sidebar-sheet")
  }
  await page.keyboard.press("Escape")
  await expect(sheet).toHaveCount(0)
  await expect(toggle(page)).toBeFocused()

  await toggle(page).click()
  await sheet.getByRole("button", { name: "Close navigation" }).click()
  await expect(sheet).toHaveCount(0)
  await expect(toggle(page)).toBeFocused()

  // A followed row navigates and closes the sheet.
  await toggle(page).click()
  await applets.locator('[data-applet="builder"]').click()
  await sheet.getByRole("navigation", { name: "Builder menu" }).getByRole("link", { name: "Transforms" }).click()
  await expect(page).toHaveURL(/\/transforms$/)
  await expect(sheet).toHaveCount(0)
})

// Below 640px the workspace switcher and the account menu (the one phone path to Sign out) live only inside the
// modal sheet: their menus open over it, take focus, close before the sheet on Escape, and Sign out reaches the server.
test("below 640px: the workspace switcher and the account menu open from inside the Navigation sheet, take focus, Escape closes the menu before the sheet, and Sign out reaches the server", async ({ page }) => {
  await recorded(page)
  const signOuts: string[] = []
  await page.route("**/api/auth/sign-out", (route) => { signOuts.push(route.request().method()); return route.fulfill({ status: 200, json: {} }) })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await settled(page)
  await toggle(page).click()
  const sheet = page.getByRole("dialog", { name: "Navigation" })
  await expect(sheet).toBeVisible()
  const focusInMenu = () => page.evaluate(() => document.activeElement?.closest('[role="menu"]') !== null)
  const stillOpen = page.locator("#studio-sidebar-sheet[data-open]:not([data-ending-style])")

  const scope = sheet.locator("[data-scope]")
  expect((await scope.boundingBox())!.height).toBeGreaterThanOrEqual(44)
  await scope.click()
  const menuPopup = page.getByRole("menu")
  await expect(menuPopup).toBeVisible()
  await expect.poll(focusInMenu).toBe(true)
  await page.keyboard.press("Escape")
  await expect(menuPopup).toHaveCount(0)
  await expect(stillOpen).toHaveCount(1)
  await expect(scope).toBeFocused()

  const account = sheet.getByRole("button", { name: "Account" })
  expect((await account.boundingBox())!.height).toBeGreaterThanOrEqual(44)
  await account.focus()
  await page.keyboard.press("Enter")
  await expect(menuPopup.getByRole("menuitem", { name: "Sign out" })).toBeVisible()
  await expect.poll(focusInMenu).toBe(true)
  await page.keyboard.press("Escape")
  await expect(menuPopup).toHaveCount(0)
  await expect(stillOpen).toHaveCount(1)
  await expect(account).toBeFocused()
  // A second Escape closes the sheet itself and returns focus to the toggle.
  await page.keyboard.press("Escape")
  await expect(sheet).toHaveCount(0)
  await expect(toggle(page)).toBeFocused()

  await toggle(page).click()
  await sheet.getByRole("button", { name: "Account" }).click()
  await menuPopup.getByRole("menuitem", { name: "Sign out" }).click()
  await expect.poll(() => signOuts).toEqual(["POST"])
})

test("islands: in the dock shell ⌘B stores no choice and opens nothing", async ({ page }) => {
  await recorded(page)
  await page.addInitScript(() => {
    try { localStorage.setItem("norn.studio.shell", "dock") } catch { /* storage blocked */ }
    ;(window as unknown as { seenB: number }).seenB = 0
    window.addEventListener("keydown", (event: KeyboardEvent) => { if (event.key === "b" && (event.ctrlKey || event.metaKey)) (window as unknown as { seenB: number }).seenB += 1 })
  })
  await page.goto("/")
  await expect(page.locator('[data-slot="floating-dock"]')).toBeVisible()
  await expect(railIsland(page)).toHaveCount(0)
  await page.keyboard.press("Control+b")
  await page.keyboard.press("Meta+b")
  await expect.poll(() => page.evaluate(() => (window as unknown as { seenB: number }).seenB)).toBe(2)
  expect(await page.evaluate(() => localStorage.getItem("norn.studio.sidebar"))).toBeNull()
  await expect(page.getByRole("dialog")).toHaveCount(0)
})

test("islands in Arabic: the rail sits at the right edge with the submenu island and the main island to its left, the toggle mirrors, and the phone sheet opens from the right", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("norn.studio.locale", "ar"))
  await recorded(page)
  await page.goto("/sources")
  await settled(page)
  await expect.poll(() => page.evaluate(() => document.documentElement.dir)).toBe("rtl")
  const [r, s, m] = await Promise.all([railIsland(page).boundingBox(), island(page).boundingBox(), main(page).boundingBox()])
  expect(Math.round(r!.x + r!.width)).toBe(1440 - 8)
  expect(s!.x + s!.width).toBeLessThanOrEqual(r!.x)
  expect(m!.x + m!.width).toBeLessThanOrEqual(s!.x)
  expect(await style(toggle(page).locator("svg"), "scale")).toBe("-1 1")
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/islands-rtl-light.png` })
  await page.setViewportSize({ width: 390, height: 844 })
  await toggle(page).click()
  const sheet = page.getByRole("dialog", { name: "Navigation" })
  await expect.poll(async () => { const b = await sheet.boundingBox(); return Math.round(b!.x + b!.width) }).toBe(390 - 8)
  // The applet column is on the sheet's start side, which is the right.
  const column = await sheet.getByRole("navigation", { name: "Applets" }).boundingBox()
  const pane = await sheet.getByRole("navigation", { name: /menu$/ }).boundingBox()
  expect(column!.x).toBeGreaterThan(pane!.x)
})

test("islands under reduced motion: collapsing, rows and the phone sheet switch instantly", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" })
  await recorded(page)
  await page.goto("/sources")
  await settled(page)
  expect(await style(island(page), "transition-property")).toBe("none")
  expect(await style(menu(page, "Builder").getByRole("link", { name: "Transforms" }), "transition-property")).toBe("none")
  expect(await style(rail(page).locator("a[data-applet]").first(), "transition-property")).toBe("none")
  await page.setViewportSize({ width: 390, height: 844 })
  await toggle(page).click()
  const sheet = page.getByRole("dialog", { name: "Navigation" })
  await expect(sheet).toBeVisible()
  expect(await style(sheet, "transition-property")).toBe("none")
})

test("islands under reduced transparency and forced colours: the glass skin falls back to opaque surfaces with no blur", async ({ page }) => {
  await recorded(page)
  // Luma's islands are opaque already (DESIGN.md 19.6); the glass skin is where the fallback matters.
  await page.addInitScript(() => localStorage.setItem("norn.studio.skin", "glass"))
  const cdp = await page.context().newCDPSession(page)
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-transparency", value: "reduce" }] })
  await page.goto("/sources")
  await settled(page)
  expect(await page.evaluate(() => matchMedia("(prefers-reduced-transparency: reduce)").matches)).toBe(true)
  const opaque = async (locator: Locator, tokenName: string) => {
    await expect(locator).toHaveCSS("backdrop-filter", "none")
    expect(await style(locator, "background-color")).toBe(await painted(page, await token(page, tokenName), "rgb(0, 0, 0)"))
  }
  await opaque(main(page), "canvas")
  await opaque(island(page), "shell")
  await opaque(railIsland(page), "shell")
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/islands-reduced-transparency-light.png` })
  await cdp.send("Emulation.setEmulatedMedia", { features: [] })
  await expect(main(page)).toHaveCSS("backdrop-filter", /blur/)
  // Forced colours: no blur and no shadow; the edge takes the system's text colour.
  await page.emulateMedia({ forcedColors: "active" })
  await expect(main(page)).toHaveCSS("backdrop-filter", "none")
  await expect(main(page)).toHaveCSS("box-shadow", "none")
})

test("Workspace unavailable shows once: the page's full state, or a one-line status; one Retry and one incident id, and below 640px the server's words and the incident sit behind a 44px Details beside a 44px Retry", async ({ page }) => {
  await page.addInitScript(() => {
    const state = window as unknown as { __sawUnavailableBanner?: boolean }
    state.__sawUnavailableBanner = false
    const check = () => {
      if (document.querySelector('[data-connection="unavailable"]') !== null) {
        state.__sawUnavailableBanner = true
      }
    }
    const observer = new MutationObserver(() => {
      check()
      requestAnimationFrame(check)
    })
    observer.observe(document, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["data-connection"],
    })
    check()
    const tick = () => {
      check()
      requestAnimationFrame(tick)
    }
    requestAnimationFrame(tick)
  })
  await session(page)
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) {
      return route.fulfill({
        status: 503,
        json: { format: "norn/refusal@2", code: "cell.unopenable", category: "internal", retryable: false, reason: null, incidentId: "inc-shell-404", message: "The workspace cell could not be opened.", explanation: "Storage migration is paused for this environment." },
      })
    }
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
  const banner = page.locator('[data-connection="unavailable"]')
  const state = page.locator("[data-workspace-unavailable]")
  const details = page.getByRole("button", { name: "Details", exact: true })
  let headline = ""
  for (const [width, height] of [[1440, 900], [390, 844]] as const) {
    await page.setViewportSize({ width, height })
    // Home shows the page's own full state; Connections has none, so the one-line status stands in.
    for (const [path, full] of [["/", true], ["/connections", false]] as const) {
      const at = `${path} at ${width}`
      await page.goto(path)
      await expect(full ? state : banner, at).toBeVisible()
      await expect(full ? banner : state, at).toHaveCount(0)
      if (full) {
        // Synchronous layout-effect suppression means the shell banner never flashes before a full page state mounts.
        const flashed = await page.evaluate(() => (window as unknown as { __sawUnavailableBanner?: boolean }).__sawUnavailableBanner)
        expect(flashed, `${at}: shell banner must not flash when page renders its own unavailable state`).toBe(false)
      }
      const retries = page.getByRole("button", { name: "Retry", exact: true }).filter({ visible: true })
      await expect(retries, at).toHaveCount(1)
      if (width === 390) expect((await retries.boundingBox())!.height, `${at}: Retry`).toBeGreaterThanOrEqual(44)
      const incidents = page.getByText("inc-shell-404", { exact: true }).filter({ visible: true })
      if (full || width >= 640) {
        // The page's state, and the status line from 640px up, show the server's words and the incident once.
        await expect(incidents, at).toHaveCount(1)
        await expect(details, at).toBeHidden()
        if (!full) headline = (await banner.locator("span[title]").first().getAttribute("title")) ?? ""
      } else {
        // The phone line keeps to one row; Details opens the server's words and the incident to quote.
        expect((await banner.boundingBox())!.height, `${at}: status`).toBeLessThanOrEqual(60)
        await expect(incidents, at).toHaveCount(0)
        expect((await details.boundingBox())!.height, `${at}: Details`).toBeGreaterThanOrEqual(44)
        await details.click()
        const opened = page.locator("[data-connection-details]")
        expect(headline, "the wide line's headline").not.toBe("")
        await expect(opened).toContainText(headline)
        await expect(opened).toContainText("Storage migration is paused for this environment.")
        await expect(incidents, at).toHaveCount(1)
        await expect(opened.getByRole("button", { name: "Copy inc-shell-404" })).toBeVisible()
        // Settled and on top of the page, not under the main island.
        await expect(opened).toHaveCSS("opacity", "1")
        const box = (await opened.boundingBox())!
        expect(await page.evaluate(([x, y]) => document.elementFromPoint(x!, y!)?.closest("[data-connection-details]") !== null, [box.x + box.width / 2, box.y + box.height / 2])).toBe(true)
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/islands-unavailable-details-390-light.png` })
        await page.keyboard.press("Escape")
        await expect(opened).toBeHidden()
        await expect(details).toBeFocused()
      }
      if (SHOTS && path === "/") await page.screenshot({ path: `${SHOTS}/islands-unavailable-${width === 390 ? "390" : "desktop"}-light.png` })
    }
  }
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

test("Home and Inbox show a local persisted Recent list for opened records, runs and views with Clear (#1774, DESIGN.md 18)", async ({ page }) => {
  test.skip(Boolean(process.env.E2E_BASE), "uses stand-in routes")
  await recorded(page)
  const recordedVisit = (id: string) =>
    expect.poll(() =>
      page.evaluate((needle) =>
        Object.entries(localStorage).some(([key, value]) => key.startsWith("norn.studio.recent:") && value.includes(needle)),
      id),
    ).toBe(true)
  await page.goto("/runs/run_demo_99")
  await expect(rail(page)).toBeVisible()
  await settled(page)
  await recordedVisit("run_demo_99")
  await page.goto("/views/view_demo_1")
  await expect(rail(page)).toBeVisible()
  await settled(page)
  await recordedVisit("view_demo_1")
  await page.goto("/")
  await expect(rail(page)).toBeVisible()
  await settled(page)
  const homeMenu = menu(page, "Home")
  const recent = homeMenu.locator('[data-slot="recent-section"]')
  await expect(recent).toBeVisible()
  await expect(recent).toContainText("view_demo_1")
  await expect(recent).toContainText("run_demo_99")
  // Persists across reload and also appears on Inbox.
  await page.reload()
  await expect(rail(page)).toBeVisible()
  await settled(page)
  await expect(recent).toBeVisible()
  await rail(page).getByRole("link", { name: /^Inbox/ }).click()
  await expect(menu(page, "Inbox").locator('[data-slot="recent-section"]')).toContainText("view_demo_1")
  // Clear removes the list immediately and across reload.
  await menu(page, "Inbox").getByRole("button", { name: "Clear", exact: true }).click()
  await expect(menu(page, "Inbox").locator('[data-slot="recent-section"]')).toHaveCount(0)
  await page.reload()
  await expect(rail(page)).toBeVisible()
  await settled(page)
  await expect(menu(page, "Inbox").locator('[data-slot="recent-section"]')).toHaveCount(0)
})

test("navigation nits (#1774): clicking the open row does not push a duplicate history entry, command palette preserves applet state on current URL, and dock links carry applet state", async ({ page }) => {
  await recorded(page)
  await page.goto("/")
  await settled(page)
  await rail(page).getByRole("link", { name: "Builder" }).click()
  await expect(page).toHaveURL(/\/sources$/)
  const lengthBefore = await page.evaluate(() => window.history.length)
  // Clicking the already-open Sources row must not push a duplicate history entry.
  await menu(page, "Builder").getByRole("link", { name: "Sources", exact: true }).click()
  await expect(page).toHaveURL(/\/sources$/)
  expect(await page.evaluate(() => window.history.length)).toBe(lengthBefore)
  // Command palette navigating to current URL keeps applet state intact.
  await page.keyboard.press("Control+k")
  const input = page.getByRole("combobox", { name: /Search pages/ })
  await input.fill("Sources")
  await page.keyboard.press("Enter")
  await expect(page).toHaveURL(/\/sources$/)
  await expect(current(page)).toHaveAttribute("data-applet", "builder")
  expect(await page.evaluate(() => window.history.length)).toBe(lengthBefore)
  // Dock links carry applet state so switching back to three-island or navigating preserves applet.
  await page.evaluate(() => localStorage.setItem("norn.studio.shell", "dock"))
  await page.reload()
  await settled(page)
  const dock = page.locator('[data-slot="floating-dock"]')
  await expect(dock).toBeVisible()
  await dock.getByRole("link", { name: "Home", exact: true }).click()
  await expect(page).toHaveURL(/\/$/)
  expect(await page.evaluate(() => (window.history.state as { applet?: string } | null)?.applet)).toBe("/")
})

/** The shell's surfaces in light and dark, at 1440 and 390 (only with SHOTS): home, a record type with the Data menu, a record, views, the canvas, the phone nav. */
test.describe("island screenshots", () => {
  test.skip(!SHOTS, "SHOTS=<dir> saves the shell's surfaces")
  test.skip(Boolean(process.env.E2E_BASE), "uses the object, record and workspace stand-ins")

  async function both(page: Page, name: string, ready: () => Promise<void>, phone?: () => Promise<void>) {
    for (const [width, height, size] of [[1440, 900, "desktop"], [390, 844, "390"]] as const) {
      await page.setViewportSize({ width, height })
      for (const scheme of ["light", "dark"] as const) {
        await page.emulateMedia({ colorScheme: scheme })
        await ready()
        if (size === "390" && phone) await phone()
        await page.waitForTimeout(400)
        await page.screenshot({ path: `${SHOTS}/${name}-${size}-${scheme}.png` })
        if (size === "390" && phone) await page.keyboard.press("Escape")
      }
    }
    await page.emulateMedia({ colorScheme: "light" })
  }

  test("home", async ({ page }) => {
    await recorded(page)
    await page.goto("/")
    await settled(page)
    await both(page, "home", () => expect(page.getByRole("heading", { name: "Home", level: 1 })).toBeVisible())
  })

  test("a record type with the Data menu", async ({ page }) => {
    await session(page)
    await objectCell(page, population(40))
    await page.goto("/objects/canon.Holding")
    await expect(page.getByText("40 records", { exact: true })).toBeVisible()
    await both(page, "records-data-menu", () => expect(page.getByRole("grid", { name: "Holding records" })).toBeVisible())
  })

  test("a record page", async ({ page }) => {
    await recordCell(page, "admin")
    await page.goto("/sources/canon.Party/demo/party/0001")
    await both(page, "record-page", () => expect(page.locator("[data-fact]").first()).toBeVisible())
  })

  test("views", async ({ page }) => {
    await workspaceCell(page)
    await page.goto("/views")
    await both(page, "views", () => expect(page.getByRole("heading", { name: "Views", level: 1 })).toBeVisible())
  })

  test("the builder canvas", async ({ page }) => {
    await recorded(page)
    await page.goto("/canvas")
    await settled(page)
    await both(page, "builder-canvas", () => expect(page.getByRole("heading", { level: 1 })).toBeVisible())
  })

  test("the phone nav", async ({ page }) => {
    await recorded(page)
    await page.goto("/sources")
    await settled(page)
    await page.setViewportSize({ width: 390, height: 844 })
    for (const scheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme: scheme })
      await toggle(page).click()
      await expect(page.getByRole("dialog", { name: "Navigation" })).toBeVisible()
      await page.waitForTimeout(400)
      await page.screenshot({ path: `${SHOTS}/phone-nav-390-${scheme}.png` })
      await page.keyboard.press("Escape")
    }
  })
})
