import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * Every navigation entry goes somewhere: each destination on the rail, then, in each perspective,
 * every page its sidebar lists (items included). A route entry changes the URL and renders its page;
 * an anchor entry changes the hash and brings its section into view. No page throws.
 */
async function recorded(page: Page, errors: string[]) {
  page.on("pageerror", (error) => errors.push(error.message))
  // No session server answers the cell's reads: Studio opens the recorded world.
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
}

const settled = (page: Page) => expect(page.getByText("Connecting…")).toHaveCount(0)

test("nav: every rail destination and every sidebar entry, in both perspectives, navigates and renders", async ({ page }) => {
  test.setTimeout(300_000)
  const errors: string[] = []
  await recorded(page, errors)
  await page.goto("/sources")
  await settled(page)
  const rail = page.getByRole("toolbar", { name: "Studio" })
  const global = await rail.locator("a[href]:not([href='/'])").evaluateAll((links) => links.map((link) => link.getAttribute("href") ?? ""))
  expect(global).toContain("/decisions")
  for (const href of global) {
    await rail.locator(`a[href="${href}"]`).click()
    await expect(page).toHaveURL((url) => url.pathname === href)
    await expect(page.locator("main")).not.toContainText(/Something went wrong|Not Found/)
  }
  let entries = 0
  for (const perspective of ["Workspace", "Builder"] as const) {
    await page.getByRole("group", { name: "Perspective" }).getByRole("button", { name: perspective }).click()
    await expect(page.getByRole("group", { name: "Perspective" }).getByRole("button", { name: perspective })).toHaveAttribute("aria-pressed", "true")
    await settled(page)
    const nav = page.getByRole("navigation", { name: perspective })
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

test("header controls do not overlap Search at tablet width with the sidebar open", async ({ page }) => {
  await page.setViewportSize({ width: 768, height: 900 })
  await recorded(page, [])
  await page.goto("/")
  await settled(page)
  const trigger = await page.getByRole("button", { name: "Toggle Sidebar" }).boundingBox()
  const search = await page.getByRole("button", { name: "Search" }).boundingBox()
  const perspective = await page.getByRole("group", { name: "Perspective" }).boundingBox()
  const notifications = await page.getByRole("button", { name: "Notifications" }).boundingBox()
  expect(trigger && search && perspective && notifications).toBeTruthy()
  expect(trigger!.x + trigger!.width).toBeLessThanOrEqual(search!.x + 1)
  expect(search!.x + search!.width).toBeLessThanOrEqual(perspective!.x + 1)
  expect(perspective!.x + perspective!.width).toBeLessThanOrEqual(notifications!.x + 1)
})

test("collapsing with Ctrl+B and activating collapsed Records without entries both preserve keyboard focus", async ({ page }) => {
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
  const metricsItem = page.getByRole("navigation", { name: "Workspace" }).getByRole("treeitem", { name: "Metrics" })
  await metricsItem.focus()
  await expect(metricsItem).toBeFocused()
  // Collapsing the sidebar while focus is inside the tree moves focus onto the rail/trigger rather than dropping it to body.
  await page.keyboard.press("Control+b")
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeHidden()
  const afterCollapse = await page.evaluate(() => ({
    tag: document.activeElement?.tagName ?? "",
    inRailOrTrigger: Boolean(
      document.activeElement?.closest('[role="toolbar"][aria-label="Studio"]') ||
      document.activeElement?.getAttribute("data-slot") === "sidebar-trigger",
    ),
  }))
  expect(afterCollapse.tag).not.toBe("BODY")
  expect(afterCollapse.inRailOrTrigger).toBe(true)
  // Activating the collapsed Records fallback button (when no entries are loaded) expands the sidebar and moves focus into Records.
  const collapsedRecords = page.getByRole("toolbar", { name: "Studio" }).getByRole("button", { name: "Records" })
  await collapsedRecords.focus()
  await page.keyboard.press("Enter")
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible()
  const afterExpand = await page.evaluate(() => ({
    tag: document.activeElement?.tagName ?? "",
    sectionPath: document.activeElement?.getAttribute("data-section-path"),
    inSidebar: Boolean(document.activeElement?.closest('nav[aria-label="Workspace"]')),
  }))
  expect(afterExpand.tag).not.toBe("BODY")
  expect(afterExpand.inSidebar).toBe(true)
  expect(afterExpand.sectionPath).toBe("/objects")
})
