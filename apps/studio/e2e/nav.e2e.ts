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
      for (const heading of await nav.locator('[role="treeitem"][aria-expanded="false"] > [data-slot="tree-label"]:not(a)').all()) await heading.click()
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
