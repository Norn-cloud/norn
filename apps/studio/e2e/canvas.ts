import { expect, type Locator, type Page } from "@playwright/test"

/** Test helpers shared by the canvas surfaces. */
export const mod = process.platform === "darwin" ? "Meta" : "Control"

export const node = (page: Page, id: string) => page.locator(`.react-flow__node[data-id="${id}"]`)
export const edge = (page: Page, id: string) => page.locator(`.react-flow__edge[data-id="${id}"]`)

export async function open(page: Page, tab?: string) {
  // Leaving with unsaved changes asks first; the tests always mean to leave.
  page.on("dialog", (dialog) => void dialog.accept())
  await page.goto("/canvas")
  if (tab) await page.getByRole("button", { name: tab, exact: true }).click()
  await expect(page.getByText(/Checker: |Checker not loaded/).first()).toBeVisible({ timeout: 30_000 })
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
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const surface = await page.locator("[data-canvas-surface]").boundingBox()
    const boxes = await Promise.all(targets.map((target) => target.boundingBox()))
    if (!surface || boxes.some((box) => !box)) return
    const left = Math.min(...boxes.map((box) => box!.x)), right = Math.max(...boxes.map((box) => box!.x + box!.width))
    const top = Math.min(...boxes.map((box) => box!.y)), bottom = Math.max(...boxes.map((box) => box!.y + box!.height))
    // The toolbar floats over the top of the canvas and the minimap over its bottom right.
    if (left >= surface.x + 24 && right <= surface.x + surface.width - 200 && top >= surface.y + 80 && bottom <= surface.y + surface.height - 24) return
    const dx = surface.x + (surface.width - 180) / 2 - (left + right) / 2
    const dy = surface.y + 80 + (surface.height - 104) / 2 - (top + bottom) / 2
    const from = await blank(page)
    await page.mouse.move(from.x, from.y)
    await page.mouse.down()
    await page.mouse.move(from.x + dx, from.y + dy, { steps: 8 })
    await page.mouse.up()
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

/** Click on an edge's wide interaction path, which React Flow draws for hit-testing. */
export async function clickEdge(page: Page, id: string) {
  const path = edge(page, id).locator(".react-flow__edge-interaction")
  await reveal(page, path)
  await path.click({ force: true })
}

/** Drag from one handle to another, as a user draws a connection. */
export async function drag(page: Page, from: Locator, to: Locator) {
  await reveal(page, from, to)
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
