import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * Every navigation entry goes somewhere: each applet on the rail, then every page its sidebar
 * lists (sub-entries included). A route entry changes the URL and renders its page; an anchor
 * entry changes the hash and brings its section into view. No page throws.
 */
async function recorded(page: Page, errors: string[]) {
  page.on("pageerror", (error) => errors.push(error.message))
  // No session server answers the cell's reads: Studio opens the recorded world.
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
}

const settled = (page: Page) => expect(page.getByText("Connecting…")).toHaveCount(0)

test("nav: every rail applet and every sidebar entry navigates and renders", async ({ page }) => {
  test.setTimeout(240_000)
  const errors: string[] = []
  await recorded(page, errors)
  await page.goto("/sources")
  await settled(page)
  const applets = await page.getByRole("toolbar", { name: "Applets" }).locator("a[href]:not([href='/'])").evaluateAll((links) => links.map((link) => link.getAttribute("href") ?? ""))
  expect(applets.length).toBeGreaterThan(5)
  let entries = 0
  for (const applet of applets) {
    await page.getByRole("toolbar", { name: "Applets" }).locator(`a[href="${applet}"]`).click()
    await expect(page).toHaveURL((url) => url.pathname === applet)
    await settled(page)
    const tree = page.getByRole("tree")
    // Open every group, so its entries are on the page.
    for (const heading of await tree.locator('[role="treeitem"][aria-expanded="false"] > [data-slot="tree-label"]').all()) await heading.click()
    const hrefs = await tree.locator("a[href]").evaluateAll((links) => links.map((link) => link.getAttribute("href") ?? ""))
    for (const href of hrefs) {
      entries += 1
      const [path = "", hash] = href.split("#")
      await tree.locator(`a[href="${href}"]`).click()
      await expect(page, `entry ${href} under ${applet}`).toHaveURL((url) => url.pathname === path && (hash === undefined || url.hash === `#${hash}`))
      if (hash !== undefined) await expect(page.locator(`[id="${decodeURIComponent(hash)}"]`), `anchor ${hash}`).toBeInViewport()
      await expect(page.locator("main")).not.toContainText(/Something went wrong|Not Found/)
    }
  }
  expect(entries).toBeGreaterThan(applets.length)
  expect(errors).toEqual([])
})
