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

/**
 * The opening viewport: the whole graph centred when it fits at READABLE or larger (never above 1:1); otherwise
 * READABLE, framed on `anchor` (the start or initial state) at the left, so the user pans to the rest.
 */
export function openingViewport(nodes: readonly Box[], container: { readonly width: number; readonly height: number }, anchor?: Box): Viewport {
  if (nodes.length === 0 || container.width <= 0 || container.height <= 0) return { x: 0, y: 0, zoom: 1 }
  const left = Math.min(...nodes.map(({ x }) => x)), right = Math.max(...nodes.map(({ x, width }) => x + width))
  const top = Math.min(...nodes.map(({ y }) => y)), bottom = Math.max(...nodes.map(({ y, height }) => y + height))
  const width = right - left, height = bottom - top
  const free = { width: container.width - 2 * INSET.side, height: container.height - INSET.top - INSET.bottom }
  const fit = Math.min(1, free.width / width, free.height / height)
  const zoom = fit >= READABLE ? fit : READABLE
  const y = height * zoom <= free.height
    ? INSET.top + (free.height - height * zoom) / 2 - top * zoom
    : container.height / 2 - ((anchor ?? nodes[0]!).y + (anchor ?? nodes[0]!).height / 2) * zoom
  if (fit >= READABLE) return { x: INSET.side + (free.width - width * zoom) / 2 - left * zoom, y, zoom }
  const at = anchor ?? nodes.reduce((a, b) => (b.x < a.x || (b.x === a.x && b.y < a.y) ? b : a))
  return { x: INSET.side - at.x * zoom, y, zoom }
}
