/**
 * Closure-free metric board builder: the lowering target of `Norn.metric`
 * and `Norn.board`.
 *
 * A board document (`norn.board/1`) carries, per panel, the typed metric
 * authoring input (`MetricDefinition`, `crates/norn-ir/src/metric.rs`) and
 * its question (`QuestionIR`). The Rust host generates MetricIR from the
 * admitted ontology, checks the question against the native metric profile
 * at registration and refuses to start on any finding; nothing here is a
 * second metric authority. Boards are host registrations, not cell
 * definition artifacts, so they are written beside the definitions.
 */
import type { Json } from "../canonical.ts"
import type { SourceLocation } from "./diagnostics.ts"

export interface MetricFilterDecl {
  readonly property: string
  readonly op: "eq" | "ne"
  readonly value: string
}

/** An additive measure over one object type: an exact-money SUM or an object COUNT. */
export interface MetricSpec {
  readonly id: string
  readonly title: string
  readonly over: string
  readonly measure: { readonly sum: string } | { readonly count: "objects" }
  /** Date or UTC timestamp property the question window applies to. */
  readonly time: string
  readonly where?: ReadonlyArray<MetricFilterDecl>
  readonly owner: string
}

export interface MetricDef extends MetricSpec {
  readonly _tag: "Metric"
  readonly at?: SourceLocation
}

export interface PanelDecl {
  readonly id: string
  readonly title: string
  readonly metric: MetricDef
  readonly groupBy?: string
  /** Half-open UTC instants. */
  readonly window: { readonly start: string; readonly end: string }
  readonly maxAgeMs: number
}

export interface BoardSpec {
  readonly id: string
  readonly title: string
  readonly version: string
  /** Object type of the per-panel live rows the host maintains. */
  readonly rows: string
  /** Object type of each accepted immutable result. */
  readonly results: string
  readonly panels: ReadonlyArray<PanelDecl>
}

export interface BoardDef extends BoardSpec {
  readonly _tag: "Board"
  readonly at?: SourceLocation
}

export const metric = (meta: MetricSpec & { readonly at?: SourceLocation }): MetricDef => ({ _tag: "Metric", ...meta })
export const board = (meta: BoardSpec & { readonly at?: SourceLocation }): BoardDef => ({ _tag: "Board", ...meta })

function definition(m: MetricDef): Json {
  const sum = "sum" in m.measure ? m.measure.sum : undefined
  return {
    name: m.id,
    label: m.title,
    type: "simple",
    type_params: {
      expr: sum ?? "*",
      metric_aggregation_params: { semantic_model: m.over, agg: sum === undefined ? "count" : "sum", agg_time_dimension: m.time },
    },
    "x-norn": {
      irVersion: "0.1",
      additivity: { kind: "additive" },
      timeDimension: { property: m.time },
      defaultFilters: (m.where ?? []).map((f) => ({ path: { property: f.property }, operator: f.op, value: f.value })),
      owner: m.owner,
      certified: false,
      authz: { action: "norn.read", minGroupSize: 1 },
    },
  }
}

/** The board document for one cell scope. */
export function lowerBoard(def: BoardDef, scope: { readonly orgId: string; readonly environmentId: string }): Json {
  return {
    format: "norn.board/1",
    id: def.id,
    title: def.title,
    version: def.version,
    scope: { orgId: scope.orgId, environmentId: scope.environmentId },
    rows: def.rows,
    results: def.results,
    panels: def.panels.map((p) => ({
      id: p.id,
      title: p.title,
      definition: definition(p.metric),
      question: {
        irVersion: "0.1",
        name: `${def.id}.${p.id}`,
        objectSet: p.metric.over,
        aggregate: [{ metric: p.metric.id }],
        ...(p.groupBy === undefined ? {} : { groupBy: [{ path: { property: p.groupBy } }] }),
        window: { start: p.window.start, end: p.window.end },
        freshness: { kind: "live", maxAgeMs: p.maxAgeMs },
      },
    })),
  }
}
