/** Candidate dashboards lower to the cell's `dashboard` definition; the cell's checker admits each tile. */
import { toJson, type Json } from "../canonical.ts"

export type DashboardView = "number" | "line" | "area" | "bar" | "pie" | "table"
export interface DashboardTileSpec {
  readonly title: string
  /** The tile's question, already lowered to QuestionIR. */
  readonly question: Json
  readonly view: DashboardView
  readonly width?: "half" | "full"
}
export interface DashboardSpec {
  readonly title: string
  readonly tiles: ReadonlyArray<DashboardTileSpec>
}

/**
 * DASHBOARD-LOWER-1: the same payload Studio saves. Tiles get stable ids in
 * order and a two-column reading-order layout (numbers and pies half width
 * unless the author says otherwise); the origin says the version was
 * written as code. Whether each view fits its question is the cell's call.
 */
export function lowerDashboard(dashboard: DashboardSpec): Json {
  let x = 0
  let y = 0
  const tiles = dashboard.tiles.map((tile, index) => {
    const w = (tile.width ?? (tile.view === "number" || tile.view === "pie" ? "half" : "full")) === "full" ? 2 : 1
    if (x + w > 2) { x = 0; y += 1 }
    const layout = { x, y, w, h: 1 }
    x += w
    if (x >= 2) { x = 0; y += 1 }
    const view = tile.view === "number" || tile.view === "table" ? { kind: tile.view } : { kind: "chart", chart: tile.view }
    return { id: `t${index + 1}`, title: tile.title, question: tile.question, view, layout }
  })
  return toJson({ title: dashboard.title, tiles, origin: { by: "code" } })
}
