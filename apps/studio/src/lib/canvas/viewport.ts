/**
 * Where a canvas opens, computed before render from the layout (positions are known, card sizes are fixed), so the
 * canvas never waits on measurement and never shows a frame at the wrong zoom.
 */
export interface Box { readonly x: number; readonly y: number; readonly width: number; readonly height: number }
export interface Viewport { readonly x: number; readonly y: number; readonly zoom: number }

/** Nodes never open smaller than this: titles stay at 12px or more on screen. */
export const READABLE = 0.85
/** Room the floating toolbar takes at the top, and the margins on the other sides, in screen pixels. */
export const INSET = { top: 88, side: 48, bottom: 24 } as const
export type Insets = { readonly top: number; readonly side: number; readonly bottom: number }
/** The minimap's size; when it is shown the opening view keeps the band above it clear. */
export const MINIMAP = { width: 160, height: 100, margin: 15 } as const
export const WITH_MINIMAP: Insets = { ...INSET, bottom: MINIMAP.height + 2 * MINIMAP.margin }

const bounds = (nodes: readonly Box[]) => {
  const left = Math.min(...nodes.map(({ x }) => x)), right = Math.max(...nodes.map(({ x, width }) => x + width))
  const top = Math.min(...nodes.map(({ y }) => y)), bottom = Math.max(...nodes.map(({ y, height }) => y + height))
  return { left, top, width: right - left, height: bottom - top }
}

/** Whether the whole graph opens at READABLE or larger; when it does, there is nothing off-view for a minimap to show. */
export function fitsReadable(nodes: readonly Box[], container: { readonly width: number; readonly height: number }, insets: Insets = INSET): boolean {
  if (nodes.length === 0 || container.width <= 0 || container.height <= 0) return true
  const { width, height } = bounds(nodes)
  return Math.min(1, (container.width - 2 * insets.side) / width, (container.height - insets.top - insets.bottom) / height) >= READABLE
}

/**
 * The opening viewport: the whole graph centred when it fits at READABLE or larger (never above 1:1); otherwise
 * READABLE, framed on `anchor` (the start or initial state) at the left, so the user pans to the rest.
 */
export function openingViewport(nodes: readonly Box[], container: { readonly width: number; readonly height: number }, anchor?: Box, insets: Insets = INSET): Viewport {
  if (nodes.length === 0 || container.width <= 0 || container.height <= 0) return { x: 0, y: 0, zoom: 1 }
  const { left, top, width, height } = bounds(nodes)
  const free = { width: container.width - 2 * insets.side, height: container.height - insets.top - insets.bottom }
  const fit = Math.min(1, free.width / width, free.height / height)
  const zoom = fit >= READABLE ? fit : READABLE
  const y = height * zoom <= free.height
    ? insets.top + (free.height - height * zoom) / 2 - top * zoom
    : insets.top + free.height / 2 - ((anchor ?? nodes[0]!).y + (anchor ?? nodes[0]!).height / 2) * zoom
  if (fit >= READABLE) return { x: insets.side + (free.width - width * zoom) / 2 - left * zoom, y, zoom }
  const at = anchor ?? nodes.reduce((a, b) => (b.x < a.x || (b.x === a.x && b.y < a.y) ? b : a))
  return { x: insets.side - at.x * zoom, y, zoom }
}

/** Whether no node, at this viewport, lies under the minimap in the container's bottom-right corner. */
export function clearOfMinimap(nodes: readonly Box[], view: Viewport, container: { readonly width: number; readonly height: number }): boolean {
  const map = { left: container.width - MINIMAP.margin - MINIMAP.width, top: container.height - MINIMAP.margin - MINIMAP.height, right: container.width - MINIMAP.margin, bottom: container.height - MINIMAP.margin }
  return nodes.every(({ x, y, width, height }) => {
    const left = x * view.zoom + view.x, top = y * view.zoom + view.y
    return left + width * view.zoom <= map.left || left >= map.right || top + height * view.zoom <= map.top || top >= map.bottom
  })
}
