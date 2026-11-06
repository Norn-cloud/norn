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

/**
 * One lane step's own way through: its rail (a vertical offset from the lane's middle), how far right of its source
 * it turns down (`out`), how far left of its target it turns in (`into`), and where on the target's left side it
 * lands (`port`, a vertical offset from the handle). Steps sharing a lane, a source or a target each get their own,
 * RAIL_GAP apart, so every step can be traced from its source to its target.
 */
export interface Rail {
  readonly offset: number; readonly out: number; readonly into: number; readonly port: number
  /** Where its label's centre sits along its run, from the turn at `out` toward the target; placed so no two labels overlap. */
  readonly along?: number
}
export const RAIL_GAP = 8
const NO_RAIL: Rail = { offset: 0, out: 0, into: 0, port: 0 }

/** Offsets around the middle, nearest first: 0, -g, g, -2g, 2g… (or ±g, ±2g… when the middle is taken). */
const around = (count: number, gap: number, skipMiddle: boolean) => {
  const values: number[] = []
  for (let k = skipMiddle ? 1 : 0; values.length < count; k += 1) for (const v of k === 0 ? [0] : [-k * gap, k * gap]) if (values.length < count) values.push(v)
  return values.sort((a, b) => a - b)
}

/** Each lane step's rail, from where its states sit (their positions; a row is a shared y). Straight steps need none. */
export function railsOf(steps: readonly { readonly id: string; readonly source: Point; readonly target: Point }[], gap: number = RAIL_GAP): ReadonlyMap<string, Rail> {
  const straight = (step: { source: Point; target: Point }) => Math.abs(step.source.y - step.target.y) < 2 && step.target.x > step.source.x
  const lane = steps.filter((step) => !straight(step))
  const key = (p: Point) => `${p.x},${p.y}`
  const group = <T,>(items: readonly T[], by: (item: T) => string) => items.reduce((all, item) => all.set(by(item), [...all.get(by(item)) ?? [], item]), new Map<string, T[]>())
  const offset = new Map<string, number>(), out = new Map<string, number>(), into = new Map<string, number>(), port = new Map<string, number>()
  // A rail per step in each lane, ordered by where they leave, then where they go.
  for (const shared of group(lane, ({ source, target }) => Math.abs(source.y - target.y) < 2 ? `above:${source.y}` : `between:${Math.max(source.y, target.y)}`).values()) {
    const ordered = [...shared].sort((a, b) => a.source.x - b.source.x || a.target.x - b.target.x)
    around(ordered.length, gap, false).forEach((v, i) => offset.set(ordered[i]!.id, v))
  }
  for (const leaving of group(lane, ({ source }) => key(source)).values()) [...leaving].sort((a, b) => a.target.x - b.target.x).forEach((step, i) => out.set(step.id, i * gap))
  for (const [at, arriving] of group(lane, ({ target }) => key(target))) {
    const ordered = [...arriving].sort((a, b) => a.source.y - b.source.y || b.source.x - a.source.x)
    ordered.forEach((step, i) => into.set(step.id, i * gap))
    // A straight step lands on the handle itself, so lane steps into the same target take the places beside it.
    const middleTaken = steps.some((step) => straight(step) && key(step.target) === at)
    around(ordered.length, gap, middleTaken).forEach((v, i) => port.set(ordered[i]!.id, v))
  }
  return new Map(lane.map(({ id }) => [id, { offset: offset.get(id) ?? 0, out: out.get(id) ?? 0, into: into.get(id) ?? 0, port: port.get(id) ?? 0 }]))
}

export function laneRoute(source: Point, target: Point, lane: { readonly pitch: number; readonly stub: number; readonly radius: number } = LANE, rail: Rail = NO_RAIL): Routed {
  const sameRow = Math.abs(source.y - target.y) < 2
  if (sameRow && target.x > source.x) {
    return { path: `M ${source.x} ${source.y} L ${target.x} ${target.y}`, label: { x: (source.x + target.x) / 2, y: source.y, anchor: "middle" }, straight: true }
  }
  // Between the two rows it joins, or above the row for a step back along it; on this step's own rail.
  const y = (sameRow ? source.y - lane.pitch / 2 : Math.max(source.y, target.y) - lane.pitch / 2) + rail.offset
  const out = source.x + lane.stub + rail.out, into = target.x - lane.stub - rail.into
  const landing = { x: target.x, y: target.y + rail.port }
  const points = [source, { x: out, y: source.y }, { x: out, y }, { x: into, y }, { x: into, y: landing.y }, landing]
  const forward = into >= out
  if (rail.along !== undefined) return { path: rounded(points, lane.radius), label: { x: out + (forward ? rail.along : -rail.along), y, anchor: "middle" }, straight: false }
  return { path: rounded(points, lane.radius), label: { x: out + (forward ? 10 : -10), y, anchor: forward ? "start" : "end" }, straight: false }
}

/** A label's height on the canvas (the edge pill), and the room kept between two labels. */
const LABEL_HEIGHT = 18, LABEL_CLEAR = 6

/**
 * Where each lane step's label sits along its run so that no two labels overlap: in order along each lane, each
 * label takes the first place on its own run (from its turn toward its target) clear of every label placed before
 * it (past the run's end only when the run is full). `width` is each label's drawn width.
 */
export function placeLabels(steps: readonly { readonly id: string; readonly source: Point; readonly target: Point; readonly width: number }[], rails: ReadonlyMap<string, Rail>, lane = LANE): ReadonlyMap<string, Rail> {
  const placed: { x1: number; x2: number; y: number }[] = []
  const clear = (x1: number, x2: number, y: number) => placed.every((box) => x2 + LABEL_CLEAR <= box.x1 || x1 >= box.x2 + LABEL_CLEAR || Math.abs(y - box.y) >= LABEL_HEIGHT)
  const runs = steps.flatMap((step) => {
    const rail = rails.get(step.id)
    if (!rail) return []
    const sameRow = Math.abs(step.source.y - step.target.y) < 2
    const y = (sameRow ? step.source.y - lane.pitch / 2 : Math.max(step.source.y, step.target.y) - lane.pitch / 2) + rail.offset
    const out = step.source.x + lane.stub + rail.out, into = step.target.x - lane.stub - rail.into
    return [{ step, rail, y, out, into, forward: into >= out }]
  }).sort((a, b) => a.y - b.y || Math.min(a.out, a.into) - Math.min(b.out, b.into))
  const result = new Map(rails)
  for (const { step, rail, y, out, into, forward } of runs) {
    const half = step.width / 2
    let along = half + 8
    for (let tries = 0; tries < 200 && !clear(...(forward ? [out + along - half, out + along + half] : [out - along - half, out - along + half]) as [number, number], y); tries += 1) along += 4
    const center = forward ? out + along : out - along
    placed.push({ x1: center - half, x2: center + half, y })
    result.set(step.id, { ...rail, along })
  }
  return result
}
