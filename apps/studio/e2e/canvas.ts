import { expect, type Page } from "@playwright/test"

/** Test helpers shared by the canvas surfaces. */
export const mod = process.platform === "darwin" ? "Meta" : "Control"

export const node = (page: Page, id: string) => page.locator(`.react-flow__node[data-id="${id}"]`)
export const edge = (page: Page, id: string) => page.locator(`.react-flow__edge[data-id="${id}"]`)

/** Against a local preview there is no Worker: the session read is answered as a signed-in viewer. */
export async function session(page: Page) {
  if (process.env.E2E_BASE) return
  await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u_e2e", name: "E2E", email: "viewer@example.com" }, organization: { id: "org_demo", name: "Demo", role: "viewer" }, environmentId: "analytics" } }))
}

export async function open(page: Page, tab?: string) {
  // Leaving with unsaved changes asks first; the tests always mean to leave.
  page.on("dialog", (dialog) => void dialog.accept())
  await session(page)
  await page.goto("/canvas")
  if (tab) await page.getByRole("button", { name: tab, exact: true }).click()
  await expect(page.getByText(/Checker: |Checker not loaded/).first()).toBeVisible({ timeout: 30_000 })
}

/** Click on an edge's wide interaction path, which React Flow draws for hit-testing. */
export async function clickEdge(page: Page, id: string) {
  await edge(page, id).locator(".react-flow__edge-interaction").click({ force: true })
}

/** Drag from one handle to another, as a user draws a connection. */
export async function drag(page: Page, from: ReturnType<Page["locator"]>, to: ReturnType<Page["locator"]>) {
  const a = await from.boundingBox()
  const b = await to.boundingBox()
  if (!a || !b) throw new Error("handle not visible")
  await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2)
  await page.mouse.down()
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2, { steps: 12 })
  await page.mouse.up()
}

export const handle = (page: Page, id: string, side: "left" | "right") => node(page, id).locator(`.react-flow__handle-${side}`).first()

/** A shortcut with the platform's command key: `press(page, "z")` is ⌘Z or Ctrl+Z. */
export const press = (page: Page, combo: string) => page.keyboard.press(`${mod}+${combo}`)
