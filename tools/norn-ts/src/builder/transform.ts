/**
 * Transform authoring (ViewIR v1, #341): typed relational plans as data over
 * ZEN, lowered to a `derived` artifact whose `calc` is `relational`. The Rust
 * checker types every node against the installed definitions; this builder
 * only resolves the inputs to their pinned entity artifacts.
 */
import { type Json, toJson } from "../canonical.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"

/** One declared input: an alias the plan's `scan` nodes read. */
export interface TransformInput {
  readonly alias: string
  readonly artifactId: string
  readonly version: string
}

export interface TransformDef {
  readonly _tag: "Transform"
  readonly id: string
  readonly title: string
  readonly version: string
  readonly inputs: ReadonlyArray<TransformInput>
  /** Operators by node id: `scan`, `filter`, `project`, `join`, `aggregate`, `window`, `union`, `dedupe`. */
  readonly nodes: Json
  /** The node whose rows are the transform's rows. */
  readonly output: string
}

/** A dotted lower-case id (`shop.revenue_by_region`). */
export const TRANSFORM_ID = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/

export function transform(meta: Omit<TransformDef, "_tag">): TransformDef {
  // Refuse function-valued/non-JSON data before any artifact is emitted.
  toJson(meta)
  return { _tag: "Transform", ...meta }
}

export function lowerTransform(def: TransformDef, options: EnvelopeOptions): { readonly ok: true; readonly artifact: Json } {
  const pins = new Map<string, Json>()
  for (const input of def.inputs) {
    pins.set(`${input.artifactId}@${input.version}`, { artifactId: input.artifactId, kind: "entity", version: input.version, required: true })
  }
  const payload = {
    viewVersion: "0.1",
    viewId: def.id,
    inputs: def.inputs.map((input) => ({ artifactId: input.artifactId, kind: "entity", version: input.version, alias: input.alias })),
    calc: { kind: "relational", nodes: def.nodes, output: def.output },
    outputSchema: {},
    maintenance: "recompute_on_read",
  }
  return {
    ok: true,
    artifact: envelope(
      { kind: "derived", artifactId: def.id, namespace: def.id.split(".")[0]!, name: def.title, version: def.version, dependencies: [...pins.values()] },
      payload,
      options,
    ),
  }
}
