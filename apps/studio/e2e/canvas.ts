import { expect, type Locator, type Page } from "@playwright/test"

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
  // The canvas fades in once it is framed; Playwright counts a transparent element as visible, so check it is shown.
  await expect(page.locator("[data-canvas-surface]")).toHaveCSS("opacity", "1")
}

/** A point on the empty canvas (the pane itself, not a node, edge, toolbar or minimap), in page coordinates. */
async function blank(page: Page): Promise<{ x: number; y: number }> {
  const point = await page.evaluate(() => {
    const surface = document.querySelector("[data-canvas-surface]")?.getBoundingClientRect()
    if (!surface) return null
    for (let y = surface.bottom - 40; y > surface.top + 90; y -= 23) for (let x = surface.left + 90; x < surface.right - 40; x += 29) {
      if (document.elementFromPoint(x, y)?.classList.contains("react-flow__pane")) return { x, y }
    }
    return null
  })
  if (!point) throw new Error("no empty canvas to grab")
  return point
}

/**
 * Pan the canvas until the targets sit inside it, the way a person drags the empty canvas. The canvas opens at a
 * readable zoom and does not shrink a large graph to fit, so a far node can start out of view.
 */
export async function reveal(page: Page, ...targets: Locator[]) {
  for (let attempt = 0; attempt < 8; attempt += 1) {
    const surface = await page.locator("[data-canvas-surface]").boundingBox()
    const boxes = await Promise.all(targets.map((target) => target.boundingBox()))
    if (!surface || boxes.some((box) => !box)) return
    const left = Math.min(...boxes.map((box) => box!.x)), right = Math.max(...boxes.map((box) => box!.x + box!.width))
    const top = Math.min(...boxes.map((box) => box!.y)), bottom = Math.max(...boxes.map((box) => box!.y + box!.height))
    // The toolbar floats over the top of the canvas and the minimap over its bottom right.
    if (left >= surface.x + 24 && right <= surface.x + surface.width - 200 && top >= surface.y + 80 && bottom <= surface.y + surface.height - 24) return
    // Ends farther apart than the canvas shows (a long drag): zoom out first, as a person would, then centre them.
    if (right - left > surface.width - 224 || bottom - top > surface.height - 104) {
      await page.locator(".react-flow__controls-zoomout").click()
      await page.waitForTimeout(250)
      continue
    }
    const dx = surface.x + (surface.width - 180) / 2 - (left + right) / 2
    const dy = surface.y + 80 + (surface.height - 104) / 2 - (top + bottom) / 2
    const from = await blank(page)
    await page.mouse.move(from.x, from.y)
    await page.mouse.down()
    await page.mouse.move(from.x + dx, from.y + dy, { steps: 8 })
    await page.mouse.up()
    // A pan swallows the click that lands right after it (d3's drag suppression); a person pauses, so do the same.
    await page.waitForTimeout(150)
  }
}

/** A node, panned into view first. */
export async function shown(page: Page, id: string) {
  const found = node(page, id)
  await expect(found).toHaveCount(1)
  await reveal(page, found)
  return found
}

/** Clear the selection by clicking the empty canvas. */
export async function clear(page: Page) {
  const at = await blank(page)
  await page.mouse.click(at.x, at.y)
}

/**
 * Click an edge where a person would: the middle of its wide interaction path, which React Flow draws for hit-testing.
 * A straight level edge has a zero-height box, so the point comes from the path itself, not its bounding box.
 */
export async function clickEdge(page: Page, id: string) {
  const path = edge(page, id).locator(".react-flow__edge-interaction")
  await reveal(page, path)
  // The point nearest the middle where this edge, not one crossing or sharing its handle, is on top.
  const at = await path.evaluate((element, id) => {
    const shape = element as SVGGeometryElement
    const length = shape.getTotalLength()
    for (const t of [0.5, 0.6, 0.4, 0.7, 0.3, 0.8, 0.2, 0.9, 0.1]) {
      const point = shape.getPointAtLength(length * t)
      const screen = new DOMPoint(point.x, point.y).matrixTransform(shape.getScreenCTM() ?? undefined)
      // The line, or the edge's own label pill (clicking a label selects its edge).
      const hit = document.elementFromPoint(screen.x, screen.y)
      if (hit?.closest(".react-flow__edge")?.getAttribute("data-id") === id || hit?.closest(`[data-edge-label="${id}"],[data-exit-label="${id}"]`)) return { x: screen.x, y: screen.y }
    }
    return null
  }, id)
  if (!at) throw new Error(`edge ${id} is covered along its whole length`)
  await page.mouse.click(at.x, at.y)
  await expect(edge(page, id)).toHaveClass(/selected/)
}

/** Drag from one handle to another, as a user draws a connection. */
export async function drag(page: Page, from: Locator, to: Locator) {
  await reveal(page, from, to)
  const a = await from.boundingBox()
  const b = await to.boundingBox()
  if (!a || !b) {
    // Say which end is missing and what its edge or node holds, so a failure names its cause.
    const holder = async (end: Locator) => end.evaluate((element) => (element.closest(".react-flow__edge, .react-flow__node") ?? element).outerHTML.slice(0, 600)).catch(() => "not in the page")
    throw new Error(`drag end not visible: ${!a ? `from (${await holder(from)})` : ""} ${!b ? `to (${await holder(to)})` : ""}`)
  }
  await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2)
  await page.mouse.down()
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2, { steps: 12 })
  await page.mouse.up()
}

export const handle = (page: Page, id: string, side: "left" | "right") => node(page, id).locator(`.react-flow__handle-${side}`).first()

/** A shortcut with the platform's command key: `press(page, "z")` is ⌘Z or Ctrl+Z. */
export const press = (page: Page, combo: string) => page.keyboard.press(`${mod}+${combo}`)

/** Each node's position as drawn (its React Flow transform), by id: equal maps mean an identical layout. */
export const placed = (page: Page) => page.locator(".react-flow__node").evaluateAll((all) =>
  Object.fromEntries(all.map((one) => [one.getAttribute("data-id") ?? "", (one as HTMLElement).style.transform])))

/** Each connection's ends, from React Flow's own "Edge from <source> to <target>" label. */
export const edgeEnds = (page: Page) => page.locator(".react-flow__edge").evaluateAll((all) =>
  all.map((one) => /^Edge from (\S+) to (\S+)$/.exec(one.getAttribute("aria-label") ?? "")?.slice(1) ?? []).filter((ends) => ends.length === 2) as [string, string][])

/** Each node's left edge on screen, by id. */
export const lefts = async (page: Page) => Object.fromEntries(await page.locator(".react-flow__node").evaluateAll((all) =>
  all.map((one) => [one.getAttribute("data-id") ?? "", one.getBoundingClientRect().x] as const)))
