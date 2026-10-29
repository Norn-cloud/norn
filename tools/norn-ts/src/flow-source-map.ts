/** Read-only projection of authoring flow syntax onto admitted artifacts. */
import type { Step } from "./builder/flow.ts"

export const FLOW_SOURCE_MAP_FORMAT = "norn-flow-source-map/1" as const
export const FLOW_SOURCE_MAP_DRAFT_FORMAT = "norn-flow-source-map-draft/1" as const

export type FlowSourceMapStepKind =
  | "perform"
  | "catchTag"
  | "if"
  | "switch"
  | "retry"
  | "sleep"
  | "all"
  | "succeed"
  | "fail"
  | "compose"

export interface FlowSourceMapLocation {
  readonly file: string
  readonly line: number
  /** Rust Option fields accept either omission or JSON null on the wire. */
  readonly column?: number | null
}

/** Compiler-only identity and edge labels carried on the closure-free builder AST. */
export interface FlowSourceMapAnnotation {
  readonly id: string
  readonly kind: FlowSourceMapStepKind
  /** Repo/provenance-root-relative, one-based source location. */
  readonly at?: FlowSourceMapLocation
  /** Labels for Route branches, aligned with `branches`. */
  readonly branchLabels?: ReadonlyArray<string>
  /** Label for a Route's otherwise branch. */
  readonly otherwiseLabel?: string
}

export interface FlowSourceMapArtifactRef {
  readonly artifactId: string
  readonly version: string
  readonly sha256: string
}

export interface FlowSourceMapStep {
  readonly id: string
  readonly kind: FlowSourceMapStepKind
  readonly at?: FlowSourceMapLocation | null
  readonly artifactIds: ReadonlyArray<string>
  readonly stateIds: ReadonlyArray<string>
  readonly decisionRowIds: ReadonlyArray<string>
}

export interface FlowSourceMapEdge {
  readonly from: string
  readonly to: string
  readonly kind: "next" | "branch" | "catch" | "retry" | "contains"
  readonly label?: string | null
}

export interface FlowSourceMapFlow {
  readonly flowId: string
  readonly flowVersion: string
  readonly sourcePath: string
  readonly artifactRefs: ReadonlyArray<FlowSourceMapArtifactRef>
  readonly steps: ReadonlyArray<FlowSourceMapStep>
  readonly edges: ReadonlyArray<FlowSourceMapEdge>
}

export interface FlowSourceMap {
  readonly format: typeof FLOW_SOURCE_MAP_FORMAT
  readonly flows: ReadonlyArray<FlowSourceMapFlow>
}

/** Compiler/pack graph before Rust supplies authoritative Norn1 artifact hashes. */
export interface FlowSourceMapDraft {
  readonly format: typeof FLOW_SOURCE_MAP_DRAFT_FORMAT
  readonly flows: ReadonlyArray<FlowSourceMapFlow>
}

export interface FlowSourceMapGraph {
  readonly steps: ReadonlyArray<FlowSourceMapStep>
  readonly edges: ReadonlyArray<FlowSourceMapEdge>
}

/** Build graph shape from compiler-annotated, closure-free steps. */
export function flowSourceMapGraph(steps: ReadonlyArray<Step>): FlowSourceMapGraph {
  const nodes = new Map<string, FlowSourceMapStep>()
  const edges: FlowSourceMapEdge[] = []
  const edgeKeys = new Set<string>()
  const tails = (shape: Shape): ReadonlyArray<string> => shape.tails

  interface Shape {
    readonly first: string | undefined
    /** Nodes that can fall through to the next step. Terminal branches have no tail. */
    readonly tails: ReadonlyArray<string>
  }

  const edge = (value: FlowSourceMapEdge): void => {
    const key = `${value.from}\u0000${value.to}\u0000${value.kind}\u0000${value.label ?? ""}`
    if (edgeKeys.has(key)) return
    edgeKeys.add(key)
    edges.push(value)
  }

  const visitSequence = (sequence: ReadonlyArray<Step>): Shape => {
    let first: string | undefined
    let previousTails: ReadonlyArray<string> = []
    for (const step of sequence) {
      const id = step.sourceMap?.id
      if (id) {
        const prior = nodes.get(id)
        // Invalid duplicate authoring ids are diagnosed by the normal flow
        // lowerer; do not turn them into an uncaught projection error here.
        if (!prior) nodes.set(id, {
          id,
          kind: step.sourceMap!.kind,
          ...(step.sourceMap?.at ? { at: step.sourceMap.at } : {}),
          artifactIds: [],
          stateIds: [],
          decisionRowIds: [],
        })
        first ??= id
        for (const from of previousTails) edge({ from, to: id, kind: "next" })
      }

      const contain = (values: ReadonlyArray<Step>, label: string): void => {
        if (!id) return
        for (const value of values) {
          const childId = value.sourceMap?.id
          if (childId) edge({ from: id, to: childId, kind: "contains", label })
        }
      }

      const routeBranch = (values: ReadonlyArray<Step>, label: string): Shape => {
        const shape = visitSequence(values)
        if (id && shape.first) edge({ from: id, to: shape.first, kind: "branch", label })
        contain(values, label)
        return shape
      }

      let stepTails: ReadonlyArray<string> = id ? [id] : previousTails
      switch (step._tag) {
        case "All":
        case "Compose": {
          const values = step._tag === "All" ? step.steps : step.body
          const shape = visitSequence(values)
          contain(values, step._tag === "All" ? "steps" : "body")
          stepTails = shape.first ? tails(shape) : id ? [id] : []
          break
        }
        case "Retry": {
          const performId = step.perform.sourceMap?.id
          if (performId) {
            const shape = visitSequence([step.perform])
            if (id) {
              edge({ from: id, to: performId, kind: "contains", label: "perform" })
              edge({ from: id, to: performId, kind: "retry" })
            }
            stepTails = shape.tails
          }
          break
        }
        case "Route": {
          const branches = step.branches.map((route, index) => routeBranch(
            route.then,
            step.sourceMap?.branchLabels?.[index] ?? `branch-${index + 1}`,
          ))
          branches.push(routeBranch(step.otherwise, step.sourceMap?.otherwiseLabel ?? "otherwise"))
          stepTails = branches.flatMap((shape) => shape.first ? shape.tails : id ? [id] : [])
          break
        }
        case "CatchTag":
        {
          const body = visitSequence(step.body)
          const handler = visitSequence(step.handler)
          contain(step.body, "body")
          contain(step.handler, "handler")
          if (id && handler.first) edge({ from: id, to: handler.first, kind: "catch", label: step.tag })
          stepTails = [
            ...(body.first ? body.tails : id ? [id] : []),
            ...(handler.first ? handler.tails : []),
          ]
          break
        }
        case "Succeed":
        case "Fail":
          stepTails = []
          break
        default:
          break
      }
      previousTails = stepTails
    }
    return { first, tails: previousTails }
  }

  visitSequence(steps)
  return { steps: [...nodes.values()], edges }
}
