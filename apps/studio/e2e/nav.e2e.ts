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

const settled = (page: Page) => expect(page.getByText("Connecting…")).toHaveCount(0)

test("nav: every shared destination and every sidebar entry, in both perspectives, navigates and renders", async ({ page }) => {
  test.setTimeout(300_000)
  const errors: string[] = []
  await recorded(page, errors)
  await page.goto("/sources")
  await settled(page)
  // One landmark holds every destination (DESIGN.md 9.1); the shared ones lead it.
  const sidebar = page.getByRole("navigation", { name: "Studio navigation" })
  await expect(sidebar).toHaveCount(1)
  const global = await sidebar.locator('[data-section-path="/decisions"] > a[href]').evaluateAll((links) => links.map((link) => link.getAttribute("href") ?? ""))
  expect(global).toEqual(["/decisions"])
  for (const href of global) {
    await sidebar.locator(`a[href="${href}"]`).click()
    await expect(page).toHaveURL((url) => url.pathname === href)
    await expect(page.locator("main")).not.toContainText(/Something went wrong|Not Found/)
  }
  let entries = 0
  for (const perspective of ["Workspace", "Builder"] as const) {
    await page.getByRole("group", { name: "Perspective" }).getByRole("button", { name: perspective }).click()
    await expect(page.getByRole("group", { name: "Perspective" }).getByRole("button", { name: perspective })).toHaveAttribute("aria-pressed", "true")
    await settled(page)
    const nav = page.getByRole("navigation", { name: "Studio navigation" })
    await expect(nav.getByRole("tree", { name: `${perspective} pages` })).toBeVisible()
    // A destination lists its items once it is open, so walk until every link the sidebar shows has been followed.
    const visited = new Set<string>()
    for (let step = 0; step < 300; step += 1) {
      const collapsed = nav.locator('[role="treeitem"][aria-expanded="false"] > [data-slot="tree-label"]:not(a)')
      for (let index = (await collapsed.count()) - 1; index >= 0; index -= 1) await collapsed.nth(index).click()
      const hrefs = await nav.locator("a[href]").evaluateAll((links) => links.map((link) => link.getAttribute("href") ?? ""))
      const href = hrefs.find((candidate) => !visited.has(candidate))
      if (href === undefined) break
      visited.add(href)
      entries += 1
      const [path = "", hash] = href.split("#")
      await nav.locator(`a[href="${href}"]`).click()
      await expect(page, `entry ${href} in ${perspective}`).toHaveURL((url) => url.pathname === path && (hash === undefined || url.hash === `#${hash}`))
      if (hash !== undefined) await expect(page.locator(`[id="${decodeURIComponent(hash)}"]`), `anchor ${hash}`).toBeInViewport()
      await expect(page.locator("main")).not.toContainText(/Something went wrong|Not Found/)
      // The page belongs to this perspective, so its sidebar stays.
      await expect(page.getByRole("group", { name: "Perspective" }).getByRole("button", { name: perspective })).toHaveAttribute("aria-pressed", "true")
    }
    expect(visited.size, `${perspective} lists pages`).toBeGreaterThan(2)
  }
  expect(entries).toBeGreaterThan(8)
  expect(errors).toEqual([])
})

test("header applies wide spacing on desktop, keeps breadcrumbs visible at 850px with Sidekick, and never overlaps at 768px", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await recorded(page, [])
  await page.route("**/sidekick/config.json", (route) => route.fulfill({ json: { sidekick: 1, socket: "/sidekick/ws" } }))
  // Switch to Builder perspective and open a type detail page whose parent crumb is the Sources link.
  await page.goto("/sources")
  await settled(page)
  await page.getByRole("group", { name: "Perspective" }).getByRole("button", { name: "Builder" }).click()
  await page.getByRole("navigation", { name: "Studio navigation" }).getByRole("link", { name: "Holding" }).click()
  await expect(page).toHaveURL(/\/sources\/canon\.Holding$/)
  // On wide desktop, the enclosing @container activates <header>'s own @2xl/@4xl spacing and centered grid.
  const headerStyle = () => page.locator('[data-slot="topbar"]').evaluate((el) => {
    const row = getComputedStyle(el.firstElementChild!)
    return { display: row.display, paddingLeft: Number.parseFloat(getComputedStyle(el).paddingLeft), columnGap: Number.parseFloat(row.columnGap) }
  })
  const desktopStyle = await headerStyle()
  expect(desktopStyle.display).toBe("grid")
  expect(desktopStyle.paddingLeft).toBeGreaterThanOrEqual(14)
  expect(desktopStyle.columnGap).toBeGreaterThanOrEqual(10)

  const checkHeader = async () => {
    const header = await page.locator('[data-slot="topbar"]').boundingBox()
    const trigger = await page.getByRole("button", { name: "Sidebar", exact: true }).boundingBox()
    const search = await page.getByRole("button", { name: "Search" }).boundingBox()
    const perspective = await page.getByRole("group", { name: "Perspective" }).boundingBox()
    const notifications = await page.getByRole("button", { name: "Notifications" }).boundingBox()
    expect(header && trigger && search && perspective && notifications).toBeTruthy()
    expect(trigger!.x + trigger!.width).toBeLessThanOrEqual(search!.x + 1)
    expect(search!.x + search!.width).toBeLessThanOrEqual(perspective!.x + 1)
    expect(perspective!.x + perspective!.width).toBeLessThanOrEqual(notifications!.x + 1)
    expect(notifications!.x + notifications!.width).toBeLessThanOrEqual(header!.x + header!.width + 1)
    return { search: search! }
  }

  // Open Sidekick alongside the open sidebar.
  await page.locator('[data-slot="sidebar"]').getByRole("button", { name: "Sidekick" }).click()
  await expect(page.locator('[data-slot="assistant-panel"]')).toBeVisible()

  // At 850px with both panels docked, the Sources parent breadcrumb link remains visible and non-overlapping.
  await page.setViewportSize({ width: 850, height: 900 })
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

  // At 768px with both panels docked (the sidebar is the 48px rail from 640 to 1023px), <header> drops back to
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

test("collapsing with Ctrl+B and activating collapsed Records (even with Data collapsed) preserve keyboard focus", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await session(page)
  // Live cell with empty definitions so Records has no menu entries yet.
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: [] } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
  await page.goto("/")
  await settled(page)
  const workspaceNav = page.getByRole("navigation", { name: "Studio navigation" })
  const tree = workspaceNav.getByRole("tree", { name: "Workspace pages" })
  // Collapse the Data space first so activating Records from the collapsed rail must reopen it before focusing.
  const dataSpace = workspaceNav.locator('[role="treeitem"]', { hasText: "Data" }).first()
  await dataSpace.locator('> [data-slot="tree-label"]').click()
  await expect(dataSpace).toHaveAttribute("aria-expanded", "false")
  const metricsItem = workspaceNav.getByRole("treeitem", { name: "Metrics" })
  await metricsItem.focus()
  await expect(metricsItem).toBeFocused()
  // Collapsing the sidebar while focus is inside the tree moves focus onto the rail/trigger rather than dropping it to body.
  await page.keyboard.press("Control+b")
  await expect(tree).toBeHidden()
  const afterCollapse = await page.evaluate(() => ({
    tag: document.activeElement?.tagName ?? "",
    inRailOrTrigger: Boolean(
      document.activeElement?.closest('nav[aria-label="Studio navigation"] [role="toolbar"]') ||
      document.activeElement?.getAttribute("data-slot") === "sidebar-trigger",
    ),
  }))
  expect(afterCollapse.tag).not.toBe("BODY")
  expect(afterCollapse.inRailOrTrigger).toBe(true)
  // Activating the collapsed Records fallback button reopens the Data space and moves focus onto Records (never the workspace switcher).
  const collapsedRecords = workspaceNav.getByRole("toolbar", { name: "Workspace pages" }).getByRole("button", { name: "Records" })
  await collapsedRecords.focus()
  await page.keyboard.press("Enter")
  await expect(tree).toBeVisible()
  await expect(dataSpace).toHaveAttribute("aria-expanded", "true")
  const afterExpand = await page.evaluate(() => ({
    tag: document.activeElement?.tagName ?? "",
    sectionPath: document.activeElement?.getAttribute("data-section-path"),
    inSidebar: Boolean(document.activeElement?.closest('nav[aria-label="Studio navigation"] [role="tree"]')),
  }))
  expect(afterExpand.tag).not.toBe("BODY")
  expect(afterExpand.inSidebar).toBe(true)
  expect(afterExpand.sectionPath).toBe("/objects")
})
