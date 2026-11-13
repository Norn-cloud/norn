/**
 * ModelIR builder (#1171 M1): the lowering target of `Norn.model`. Mirrors
 * `ModelRoot` in `crates/norn-ir/src/model.rs`: a typed predictive model whose
 * internals stay opaque. The model declares its input (an object type), the
 * input properties it reads as features, an optional supervised target, the
 * typed prediction columns it writes, its trust and the closed contracts each
 * output must meet, and optionally the typed object type its predictions are
 * written to. The Rust checker is the authority on every rule; this
 * builder refuses only what it can see locally (malformed ids).
 */
import type { Json } from "../canonical.ts"
import { type BuildDiagnostic, Diagnostics, type SourceLocation, located } from "./diagnostics.ts"
import type { PropertyType } from "./entity.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"

/** VIEW-TRUST-1, strongest first. A model is `opaque` or `contract_checked`. */
export type TrustLevel = "native" | "proven" | "contract_checked" | "opaque"

/** A closed check on one output (`ContractRule`). */
export type ContractRule =
  | { readonly kind: "range"; readonly min?: string; readonly max?: string }
  | { readonly kind: "notNull" }
  | { readonly kind: "finite" }
  | { readonly kind: "oneOf"; readonly values: ReadonlyArray<string> }

export interface ModelContract {
  readonly id: string
  readonly output: string
  readonly rule: ContractRule
}

/** One prediction column: an EntityIR property type and its null policy. */
export interface ModelOutput {
  readonly id: string
  readonly type: PropertyType
  readonly nullable: boolean
}

export interface ModelDef {
  readonly _tag: "Model"
  /** `modelId`; the artifact is `<id>.model` in the id's first namespace segment. */
  readonly id: string
  readonly title: string
  readonly version: string
  /** The input object type's qualified name. */
  readonly input: string
  readonly features: ReadonlyArray<string>
  readonly target?: string
  readonly outputs: ReadonlyArray<ModelOutput>
  readonly trust: TrustLevel
  readonly contracts?: ReadonlyArray<ModelContract>
  /** The qualified name of the object type predictions are written to (M4). */
  readonly predictions?: string
  readonly method: { readonly label: string; readonly version?: string }
  readonly at?: SourceLocation
}

export function model(meta: Omit<ModelDef, "_tag" | "at"> & { readonly at?: SourceLocation | undefined }): ModelDef {
  const { at, ...rest } = meta
  return located({ _tag: "Model" as const, ...rest }, at)
}

export type ModelBuildResult =
  | { readonly ok: true; readonly artifact: Json }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<BuildDiagnostic> }

/** `MODEL_ID_PATTERN`: two or more dotted snake_case segments. */
const MODEL_ID = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/

/** Lower a model to its `model` artifact, `<id>.model`. */
export function lowerModel(def: ModelDef, options: EnvelopeOptions): ModelBuildResult {
  const diagnostics = new Diagnostics()
  if (!MODEL_ID.test(def.id)) diagnostics.report("NORN-FLOW-LITERAL", `model id ${JSON.stringify(def.id)} must be dotted snake_case segments (sales.churn_model)`, def.at)
  if (!diagnostics.empty) return { ok: false, diagnostics: diagnostics.items }
  const contracts = def.contracts ?? []
  const payload = {
    modelId: def.id,
    title: def.title,
    version: def.version,
    input: { kind: "objectType", objectType: def.input },
    features: [...def.features],
    target: def.target,
    outputs: def.outputs.map((o) => ({ id: o.id, type: o.type, nullable: o.nullable })),
    trust: def.trust,
    // The canonical form omits an empty contract list.
    contracts: contracts.length === 0 ? undefined : contracts.map((c) => ({ id: c.id, output: c.output, rule: c.rule })),
    predictions: def.predictions === undefined ? undefined : { objectType: def.predictions },
    method: { label: def.method.label, version: def.method.version },
  }
  const artifact = envelope(
    { kind: "model", artifactId: `${def.id}.model`, namespace: def.id.slice(0, def.id.indexOf(".")), name: def.title, version: def.version },
    payload,
    options,
  )
  return { ok: true, artifact }
}
