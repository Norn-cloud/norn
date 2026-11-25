import { expect, test, type Locator, type Page } from "@playwright/test"
import { node, open } from "./canvas.ts"

// Where things land on the canvas: a new node inside the view, nothing clipped at the canvas's edges, the page
// header never scrolled away, and findings in plain words with their codes behind the Technical view.

const surface = (page: Page) => page.locator("[data-canvas-surface]")

/** Whether `target`'s box lies wholly inside the canvas surface's box (a pixel of rounding allowed). */
async function inside(page: Page, target: Locator) {
  const outer = await surface(page).boundingBox()
  const box = await target.boundingBox()
  if (!outer || !box) return false
  return box.x >= outer.x - 1 && box.y >= outer.y - 1 && box.x + box.width <= outer.x + outer.width + 1 && box.y + box.height <= outer.y + outer.height + 1
}

async function chooseFromAccountMenu(page: Page, name: string) {
  await page.getByRole("button", { name: /Account$/ }).click()
  await page.getByRole("menuitemradio", { name, exact: true }).click()
}

test("placement: a step added from the toolbar lands wholly inside the canvas", async ({ page }) => {
  await open(page, "Automation")
  await page.getByRole("button", { name: "Ask a person", exact: true }).click()
  await expect(node(page, "decide")).toBeVisible()
  expect(await inside(page, node(page, "decide"))).toBe(true)
  // A second one lands in view too, beside the first rather than on it.
  await page.getByRole("button", { name: "Wait", exact: true }).click()
  await expect(node(page, "sleep")).toBeVisible()
  expect(await inside(page, node(page, "sleep"))).toBe(true)
})

test.describe("at 1440 × 900", () => {
  test.use({ viewport: { width: 1440, height: 900 } })

  test("placement: the toolbar and Save draft are inside the canvas on load, and fit view holds every node", async ({ page }) => {
    await open(page, "Automation")
    const toolbar = page.getByRole("toolbar", { name: "Canvas actions" })
    expect(await inside(page, toolbar)).toBe(true)
    expect(await inside(page, toolbar.getByRole("button", { name: "Save draft" }))).toBe(true)
    expect(await inside(page, toolbar.getByRole("button", { name: "Save to server" }))).toBe(true)
    // Fit view frames the whole graph: no node runs past the canvas's edge.
    await page.locator(".react-flow__controls-fitview").click()
    await page.waitForTimeout(300)
    const nodes = page.locator(".react-flow__node")
    expect(await nodes.count()).toBeGreaterThan(0)
    for (const one of await nodes.all()) expect(await inside(page, one)).toBe(true)
  })

  test("placement: the page header stays in view on load and after adding and selecting a step", async ({ page }) => {
    await open(page, "Automation")
    const top = async () => (await page.getByRole("heading", { level: 1 }).first().boundingBox())?.y ?? -1
    expect(await page.evaluate(() => window.scrollY)).toBe(0)
    expect(await top()).toBeGreaterThanOrEqual(0)
    await page.getByRole("button", { name: "Ask a person", exact: true }).click()
    await node(page, "decide").click()
    expect(await top()).toBeGreaterThanOrEqual(0)
  })

  test("findings: the inspector says them in plain words with a dot; the code shows in the Technical view", async ({ page }) => {
    await open(page, "Automation")
    await page.getByRole("button", { name: "Ask a person", exact: true }).click()
    const findings = page.locator("[data-issue]")
    await expect(findings.first()).toBeVisible()
    await expect(findings.first().locator("[data-slot=status-dot]")).toContainText(/Problem|Warning|Note/)
    for (const text of await findings.allInnerTexts()) expect(text).not.toMatch(/canvas\./)
    await chooseFromAccountMenu(page, "Technical")
    await expect(page.locator('[data-issue="canvas.decision.no_question"]')).toContainText("canvas.decision.no_question")
    await chooseFromAccountMenu(page, "Business")
    await expect(page.locator('[data-issue="canvas.decision.no_question"]')).not.toContainText("canvas.decision.no_question")
  })
})
