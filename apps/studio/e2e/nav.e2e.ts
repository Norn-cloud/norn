import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * Every navigation entry goes somewhere: each shared destination (Home, Inbox), then, in each
 * perspective, every page the sidebar lists (items included). A route entry changes the URL and renders its page;
 * an anchor entry changes the hash and brings its section into view. No page throws.
 */
async function recorded(page: Page, errors: string[]) {
  page.on("pageerror", (error) => errors.push(error.message))
  // No session server answers the cell's reads: Studio opens the recorded world.
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
}

const settled = async (page: Page) => {
  await expect(page.locator("[data-route]")).toBeVisible()
  await expect(page.getByText("Connecting…")).toHaveCount(0)
}

/** Wait for a lazy route chunk to commit and render content before asserting on its text. */
async function routeCommitted(page: Page, path: string, label: string) {
  await expect(page.locator(`[data-route="${path}"]`), label).toBeVisible()
  await expect(page.getByRole("status", { name: "Loading page" }), label).toHaveCount(0)
  await expect(page.locator("main h1, main h2").first(), label).toBeVisible()
}

test("nav: every applet on the rail, and every row its menu lists, navigates and renders", async ({ page }) => {
  test.setTimeout(300_000)
  const errors: string[] = []
  await recorded(page, errors)
  await page.goto("/")
  await settled(page)
  // Two landmarks (DESIGN.md 9.1): the applets on the rail, and the open applet's menu.
  const rail = page.getByRole("navigation", { name: "Applets" })
  await expect(rail).toHaveCount(1)
  const applets = await rail.locator("a[data-applet]").evaluateAll((links) => links.map((link) => ({ id: link.getAttribute("data-applet") ?? "", name: link.getAttribute("aria-label") ?? "" })))
  expect(applets.length).toBeGreaterThan(4)
  let entries = 0
  for (const applet of applets) {
    await rail.locator(`a[data-applet="${applet.id}"]`).click()
    await settled(page)
    // Exactly one applet is current, and it is the one followed.
    await expect(rail.locator('[aria-current="page"]')).toHaveCount(1)
    await expect(rail.locator('[aria-current="page"]')).toHaveAttribute("data-applet", applet.id)
    const label = applet.name.replace(/, .*$/, "")
    const menu = page.getByRole("navigation", { name: `${label} menu` })
    await expect(menu, `${label} menu`).toBeVisible()
    // A list shows its first rows: open every "Show all" and every closed branch, then follow each link once.
    const visited = new Set<string>()
    for (let step = 0; step < 300; step += 1) {
      const more = menu.locator('[data-more] > [data-slot="tree-label"]', { hasText: "Show all" })
      for (let index = (await more.count()) - 1; index >= 0; index -= 1) await more.nth(index).click()
      const collapsed = menu.locator('[role="treeitem"][aria-expanded="false"] > [data-slot="tree-label"]:not(a)')
      for (let index = (await collapsed.count()) - 1; index >= 0; index -= 1) await collapsed.nth(index).click()
      const hrefs = await menu.locator("a[href]").evaluateAll((links) => links.map((link) => link.getAttribute("href") ?? ""))
      const href = hrefs.find((candidate) => !visited.has(candidate))
      if (href === undefined) break
      visited.add(href)
      entries += 1
      const [path = "", hash] = href.split("#")
      await menu.locator(`a[href="${href}"]`).click()
      await expect(page, `entry ${href} in ${label}`).toHaveURL((url) => url.pathname === path && (hash === undefined || url.hash === `#${hash}`))
      await routeCommitted(page, path, `entry ${href} in ${label}`)
      if (hash !== undefined) await expect(page.locator(`[id="${decodeURIComponent(hash)}"]`), `anchor ${hash}`).toBeInViewport()
      await expect(page.locator("main")).not.toContainText(/Something went wrong|Not Found/)
      // The page belongs to this applet, so its menu stays open and its rail button stays current.
      await expect(rail.locator('[aria-current="page"]'), `${href} keeps ${label} current`).toHaveAttribute("data-applet", applet.id)
    }
    expect(visited.size, `${label} lists pages`).toBeGreaterThan(0)
  }
  expect(entries).toBeGreaterThan(8)
  expect(errors).toEqual([])
})

test("header applies wide spacing on desktop, keeps breadcrumbs visible at 850px with Sidekick, and never overlaps at 768px", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await recorded(page, [])
  await page.route("**/sidekick/config.json", (route) => route.fulfill({ json: { sidekick: 1, socket: "/sidekick/ws" } }))
  // Open the Builder applet and a type detail page whose parent crumb is the Sources link.
  await page.goto("/sources")
  await settled(page)
  await page.getByRole("navigation", { name: "Builder menu" }).getByRole("link", { name: "Holding" }).click()
  await expect(page).toHaveURL(/\/sources\/canon\.Holding$/)
  // On wide desktop, the enclosing @container activates <header>'s own @2xl spacing. The row is a flex row,
  // not a centred three-column grid, so the tick bar, language pill and scheme toggle fit beside the crumbs (restyle 2).
  const headerStyle = () => page.locator('[data-slot="topbar"]').evaluate((el) => {
    const row = getComputedStyle(el.firstElementChild!)
    return { display: row.display, paddingLeft: Number.parseFloat(getComputedStyle(el).paddingLeft), columnGap: Number.parseFloat(row.columnGap) }
  })
  const desktopStyle = await headerStyle()
  expect(desktopStyle.display).toBe("flex")
  expect(desktopStyle.paddingLeft).toBeGreaterThanOrEqual(14)
  expect(desktopStyle.columnGap).toBeGreaterThanOrEqual(10)

  const checkHeader = async () => {
    const header = await page.locator('[data-slot="topbar"]').boundingBox()
    const trigger = await page.getByRole("button", { name: "Sidebar", exact: true }).boundingBox()
    const search = await page.getByRole("button", { name: "Search" }).boundingBox()
    const notifications = await page.getByRole("button", { name: "Notifications" }).boundingBox()
    expect(header && trigger && search && notifications).toBeTruthy()
    expect(trigger!.x + trigger!.width).toBeLessThanOrEqual(search!.x + 1)
    expect(search!.x + search!.width).toBeLessThanOrEqual(notifications!.x + 1)
    expect(notifications!.x + notifications!.width).toBeLessThanOrEqual(header!.x + header!.width + 1)
    return { search: search! }
  }

  // Open Sidekick from the rail, beside the open submenu island.
  await page.locator('[data-slot="sidebar-rail"]').getByRole("button", { name: "Sidekick" }).click()
  await expect(page.locator('[data-slot="assistant-panel"]')).toBeVisible()

  // At 850px with both panels docked, the Sources parent breadcrumb link remains visible and non-overlapping.
  // Below 1024px the submenu island collapses to nothing (a 200ms width change), so the bar is measured after it.
  await page.setViewportSize({ width: 850, height: 900 })
  await expect.poll(async () => (await page.locator('[data-slot="sidebar"]').boundingBox())?.width ?? 0).toBeLessThan(1)
  const { search: searchAt850 } = await checkHeader()
  const sourcesCrumb = page.getByRole("navigation", { name: "breadcrumb" }).getByRole("link", { name: "Sources" })
  await expect(sourcesCrumb).toBeVisible()
  const crumbBox = await sourcesCrumb.boundingBox()
  expect(crumbBox).toBeTruthy()
  expect(crumbBox!.width).toBeGreaterThanOrEqual(48)
  expect(crumbBox!.x + crumbBox!.width).toBeLessThanOrEqual(searchAt850.x + 1)

  // On a 3-crumb record detail page at 830px with both panels docked, only the immediate parent link is rendered
  // (no zero-width focusable ancestor links) and it fits cleanly without overlapping Search.
  await page.goto("/sources/canon.Holding/demo/holding/0001")
  await settled(page)
  await expect(page.locator('[data-slot="assistant-panel"]')).toBeVisible()
  await page.setViewportSize({ width: 830, height: 900 })
  const { search: searchAt830 } = await checkHeader()
  await expect(page.getByRole("navigation", { name: "breadcrumb" }).locator("a", { hasText: "Holding" })).toBeVisible()
  const renderedCrumbLinks = await page.getByRole("navigation", { name: "breadcrumb" }).locator("a").evaluateAll((links) =>
    links.filter((link) => getComputedStyle(link).display !== "none" && link.getClientRects().length > 0).map((link) => {
      const rect = link.getBoundingClientRect()
      return { text: link.textContent?.trim() ?? "", width: rect.width, right: rect.right }
    }),
  )
  expect(renderedCrumbLinks).toHaveLength(1)
  expect(renderedCrumbLinks[0]!.text).toBe("Holding")
  expect(renderedCrumbLinks[0]!.width).toBeGreaterThanOrEqual(48)
  expect(renderedCrumbLinks[0]!.right).toBeLessThanOrEqual(searchAt830.x + 1)

  // At 768px with both panels docked (the submenu island is collapsed from 640 to 1023px), <header> drops back to
  // compact flex spacing, and the crumbs collapse to what fits: no link is crushed, and controls never overlap.
  await page.setViewportSize({ width: 768, height: 900 })
  const compactStyle = await headerStyle()
  expect(compactStyle.display).toBe("flex")
  expect(desktopStyle.paddingLeft).toBeGreaterThan(compactStyle.paddingLeft)
  expect(desktopStyle.columnGap).toBeGreaterThan(compactStyle.columnGap)
  const visibleAt768 = await page.getByRole("navigation", { name: "breadcrumb" }).locator("a").evaluateAll((links) =>
    links.filter((link) => link.getClientRects().length > 0).map((link) => link.getBoundingClientRect().width),
  )
  expect(visibleAt768.length).toBeLessThanOrEqual(1)
  for (const width of visibleAt768) expect(width).toBeGreaterThanOrEqual(48)
  await checkHeader()
})
