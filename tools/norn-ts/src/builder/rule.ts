/**
 * Closure-free RuleIR v2 builder (T7-TS-3, #2691): the lowering target of
 * `Norn.decision`. Mirrors `RulePayload` in `crates/norn-ir/src/rule.rs`: a
 * contract (`decide` / `sub_decision`), committed-event triggers and slice
 * declarations (T3-DEC-3, #2978), closed input/output schemas, and a JDM
 * decision graph whose cells are ZEN Expression text. The graph is carried as
 * JDM JSON exactly as `jdm-editor` reads and writes it, so an editor export
 * round-trips through TypeScript unchanged. The Rust checker owns every rule
 * (graph shape, ZEN typing against the slice, declared transitions).
 */
import type { Json } from "../canonical.ts"
import { type BuildDiagnostic, Diagnostics, type SourceLocation, located } from "./diagnostics.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"
import type { ObjectSchema } from "./schema.ts"

export type RuleContract = "decide" | "sub_decision"
export interface DecisionDependencyPin {
  readonly artifactId: string
  readonly version: string
}

/** A committed ledger event that triggers a `decide` rule. */
export type RuleTrigger =
  | { readonly on: "transitioned"; readonly objectType: string; readonly transition: string }
  | { readonly on: "created"; readonly objectType: string }
  | { readonly on: "deleted"; readonly objectType: string }
  | { readonly on: "observed"; readonly source: string }

/**
 * One slice entry: the trigger object, one object reached by a property of an
 * earlier entry, or (`link`) every target of a link of an earlier entry, as a
 * bounded list (#3079). `optional` omits a property-keyed entry whose key is
 * absent instead of refusing the turn.
 */
export interface SliceDecl {
  readonly name: string
  readonly objectType: string
  readonly key:
    | "triggerObject"
    | { readonly property: { readonly from: string; readonly property: string } }
    | { readonly link: { readonly from: string; readonly link: string } }
  readonly optional?: boolean
}

/** A JDM decision graph (`application/vnd.gorules.decision`), as `jdm-editor` exchanges it. */
export interface JdmGraph {
  readonly contentType?: string
  readonly nodes: ReadonlyArray<Json>
  readonly edges: ReadonlyArray<Json>
  readonly imports?: ReadonlyArray<string>
  readonly settings?: Json
}

export const JDM_CONTENT_TYPE = "application/vnd.gorules.decision"

export interface DecisionDef {
  readonly _tag: "Decision"
  /** `ruleId`. */
  readonly id: string
  /** The artifact id; defaults to `<id>.rule`. */
  readonly artifactId?: string
  readonly title: string
  readonly version: string
  readonly contract: RuleContract
  /** Required RuleIR v2 helper decisions, pinned by artifact id and version. */
  readonly dependencies: ReadonlyArray<DecisionDependencyPin>
  readonly triggers: ReadonlyArray<RuleTrigger>
  readonly slice: ReadonlyArray<SliceDecl>
  readonly input: ObjectSchema
  readonly output: ObjectSchema
  readonly scopeRequired: ReadonlyArray<string>
  readonly graph: JdmGraph
  readonly at?: SourceLocation
}

export function decision(
  meta: Omit<DecisionDef, "_tag" | "at" | "triggers" | "slice" | "dependencies"> & {
    readonly triggers?: ReadonlyArray<RuleTrigger>
    readonly slice?: ReadonlyArray<SliceDecl>
    readonly dependencies?: ReadonlyArray<DecisionDef | DecisionDependencyPin>
    readonly at?: SourceLocation | undefined
  },
): DecisionDef {
  const { at, triggers = [], slice = [], dependencies = [], ...rest } = meta
  const pins = dependencies.map((dependency): DecisionDependencyPin => (
    "_tag" in dependency
      ? { artifactId: dependency.artifactId ?? dependency.id + ".rule", version: dependency.version }
      : { artifactId: dependency.artifactId, version: dependency.version }
  ))
  return located({ _tag: "Decision" as const, ...rest, triggers: [...triggers], slice: [...slice], dependencies: pins }, at)
}

export type DecisionBuildResult =
  | { readonly ok: true; readonly artifact: Json }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<BuildDiagnostic> }

const RULE_ID = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/
const omitEmpty = <T>(list: ReadonlyArray<T>): ReadonlyArray<T> | undefined => (list.length === 0 ? undefined : list)

/** Lower one decision to its RuleIR v2 `rule` artifact. */
export function lowerDecision(def: DecisionDef, options: EnvelopeOptions): DecisionBuildResult {
  const diagnostics = new Diagnostics()
  if (!RULE_ID.test(def.id)) diagnostics.report("NORN-FLOW-LITERAL", `rule id ${JSON.stringify(def.id)} must be dotted lower-case segments (commerce.rule.credit_hold)`, def.at)
  if (!diagnostics.empty) return { ok: false, diagnostics: diagnostics.items }
  const payload = {
    ruleId: def.id,
    ruleVersion: "2.0",
    contract: def.contract,
    triggers: omitEmpty(def.triggers),
    // The canonical form omits a false `optional` (Rust `SliceDeclaration`).
    slice: omitEmpty(def.slice.map(({ optional, ...entry }) => (optional === true ? { ...entry, optional } : entry))),
    inputSchema: def.input,
    outputSchema: def.output,
    scopeRequired: def.scopeRequired,
    graph: def.graph,
  }
  const artifact = envelope(
    {
      kind: "rule",
      artifactId: def.artifactId ?? `${def.id}.rule`,
      namespace: def.id.slice(0, def.id.indexOf(".")),
      name: def.title,
      version: def.version,
      dependencies: def.dependencies.map((dependency): Json => ({
        artifactId: dependency.artifactId,
        kind: "rule",
        version: dependency.version,
        required: true,
      })),
    },
    payload,
    options,
  )
  return { ok: true, artifact }
}
