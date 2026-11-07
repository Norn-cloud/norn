/**
 * How a lifecycle step is drawn between two states: a step along the main row goes straight across; any other
 * (down to a side or terminal state, back up, or backwards along the row) leaves to the right, drops into the lane
 * between rows (or above the row for a backward step), runs along it and enters its target from the left, with
 * rounded corners. Each label sits where only its own edge runs: mid-gap on a straight step, and on a lane step at
 * the start of its run, under its own source, so labels sharing a lane are staggered by source and never on a port.
 */
export interface Point { readonly x: number; readonly y: number }
export interface Routed {
  readonly path: string
  /** Where the label goes and which side of it touches that point. */
  readonly label: { readonly x: number; readonly y: number; readonly anchor: "middle" | "start" | "end" }
  readonly straight: boolean
}

/** Row pitch of the lifecycle layout, the horizontal stub before a turn, and the corner radius. */
export const LANE = { pitch: 110, stub: 18, radius: 10 } as const

/** A polyline through the points with each corner rounded (radius clamped to half of its shorter leg). */
export function rounded(points: readonly Point[], radius: number = LANE.radius): string {
  const [first, ...rest] = points
  if (!first) return ""
  let d = `M ${first.x} ${first.y}`
  for (let i = 0; i < rest.length; i += 1) {
    const at = rest[i]!, before = points[i]!, after = rest[i + 1]
    if (!after) { d += ` L ${at.x} ${at.y}`; break }
    const inLen = Math.hypot(at.x - before.x, at.y - before.y), outLen = Math.hypot(after.x - at.x, after.y - at.y)
    const r = Math.min(radius, inLen / 2, outLen / 2)
    if (r <= 0) { d += ` L ${at.x} ${at.y}`; continue }
    const from = { x: at.x - ((at.x - before.x) / inLen) * r, y: at.y - ((at.y - before.y) / inLen) * r }
    const to = { x: at.x + ((after.x - at.x) / outLen) * r, y: at.y + ((after.y - at.y) / outLen) * r }
    d += ` L ${from.x} ${from.y} Q ${at.x} ${at.y} ${to.x} ${to.y}`
  }
  return d
}

export function laneRoute(source: Point, target: Point, lane: { readonly pitch: number; readonly stub: number; readonly radius: number } = LANE): Routed {
  const sameRow = Math.abs(source.y - target.y) < 2
  if (sameRow && target.x > source.x) {
    return { path: `M ${source.x} ${source.y} L ${target.x} ${target.y}`, label: { x: (source.x + target.x) / 2, y: source.y, anchor: "middle" }, straight: true }
  }
  // Between the two rows it joins, or above the row for a step back along it.
  const y = sameRow ? source.y - lane.pitch / 2 : Math.max(source.y, target.y) - lane.pitch / 2
  const out = source.x + lane.stub, into = target.x - lane.stub
  const points = [source, { x: out, y: source.y }, { x: out, y }, { x: into, y }, { x: into, y: target.y }, target]
  const forward = into >= out
  return { path: rounded(points, lane.radius), label: { x: out + (forward ? 10 : -10), y, anchor: forward ? "start" : "end" }, straight: false }
}
