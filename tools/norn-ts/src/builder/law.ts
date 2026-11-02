/** Law authoring is data over ZEN, lowered to the Rust-owned LawIR contract. */
import { Schema } from "effect"
import { type Json, toJson } from "../canonical.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"
import * as X from "./expr.ts"
import type { ValueSchema } from "./schema.ts"

const TextSlot = Schema.Struct({ en: Schema.String, ar: Schema.String })
const Wordings = Schema.Record(Schema.String, Schema.Struct({
  lawDigest: Schema.String,
  slots: Schema.Struct({ scope: TextSlot, activation: TextSlot, component: TextSlot, response: TextSlot }),
}))
export const decodeLawWordings = (value: unknown) => Schema.decodeUnknownSync(Wordings)(value, { onExcessProperty: "error" })

export type LawModality = { readonly kind: "always" | "never" | "eventually" } | { readonly kind: "until"; readonly predicate: string }
export type LawTiming = { readonly kind: "within"; readonly millis: string } | { readonly kind: "before_deadline"; readonly property: string }
export type LawResponse =
  | { readonly kind: "predicate"; readonly expression: string }
  | { readonly kind: "invariant"; readonly objectType: string; readonly invariantId: string }
  | { readonly kind: "action"; readonly artifactId: string }
  | { readonly kind: "transition"; readonly objectType: string; readonly property: string; readonly transitionId: string }
export interface LawScope {
  readonly root: { readonly kind: "company" | "app" | "cell" | "entity"; readonly id: string }
  readonly objectTypes: ReadonlyArray<string>
  readonly lifecycleStates?: ReadonlyArray<{ readonly objectType: string; readonly property: string; readonly states: ReadonlyArray<string> }>
  readonly commitments?: ReadonlyArray<string>
}
export interface LawIR {
  readonly lawVersion: "0.1"
  readonly id: string
  readonly owner: string
  readonly severity: "error" | "warning"
  readonly rationale: string
  readonly provenance: { readonly source: "authoring_sdk" | "agent" | "migration" | "import" | "system"; readonly sourcePath?: string; readonly actorId?: string; readonly createdAt?: string }
  readonly scope: LawScope
  readonly subject: string
  readonly condition?: string
  readonly modality: LawModality
  readonly timing?: LawTiming
  readonly response: LawResponse
}
export interface LawDef {
  readonly _tag: "Law"
  readonly id: string
  readonly title: string
  readonly version: string
  readonly law: LawIR
  readonly wordings?: ReturnType<typeof decodeLawWordings>
  readonly dependencies: ReadonlyArray<{ readonly artifactId: string; readonly kind: "entity" | "action" | "rule" | "policy" | "law" | "connector" | "mapping" | "relation" | "derived" | "setting"; readonly version: string; readonly required: boolean }>
}

/** Reuse the existing typed expression builder; Rust remains the admission authority. */
export function zenPredicate(expression: X.Expr, subject: ValueSchema): string {
  const scope: X.Scope = new Map([["self", { zen: "self", schema: subject }]])
  if (X.typeOf(expression, scope).type !== "boolean") throw new X.ExprError("NORN-FLOW-EXPRESSION", "a law predicate must be Boolean")
  return X.render(expression, scope)
}

export function law(meta: Omit<LawDef, "_tag">): LawDef {
  // Refuse function-valued/non-JSON data before any artifact is emitted.
  toJson(meta)
  if (meta.wordings) {
    decodeLawWordings(meta.wordings)
    if (Object.keys(meta.wordings).some(id => id !== meta.law.id)) throw new Error("wording key must equal its law ID")
  }
  return { _tag: "Law", ...meta }
}

export function lowerLaw(def: LawDef, options: EnvelopeOptions): { readonly ok: true; readonly artifact: Json } {
  return { ok: true, artifact: envelope({ kind: "law", artifactId: def.id, namespace: def.id.split(".")[0]!, name: def.title, version: def.version, dependencies: toJson(def.dependencies) as Json[] }, def.law, options) }
}

/** Inverse source face. Scope and envelope provenance come from the same options. */
export function printLaw(def: Omit<LawDef, "_tag">): string {
  return `import { Norn } from "@norn/authoring"\nexport const law = Norn.law(${JSON.stringify(toJson(def), null, 2)})\n`
}

/** Separate display artifacts; no wording member enters LawIR or its semantic digest. */
export function wordingArtifacts(def: LawDef): Json[] {
  return Object.entries(def.wordings ?? {}).map(([id, template]) => toJson({
    format: "norn.law_wording/1", artifactId: `${id}.wording`, lawArtifactId: def.id, lawSha256: template.lawDigest,
    requirement: {
      norm: { law: def.law, modality: def.law.modality.kind === "never" ? "prohibition" : "obligation",
        authorityId: def.law.owner, authorityVersion: def.version,
        source: { artifactId: `${id}.wording`, pointer: "/requirement/norm" }, exceptions: [], defeatedBy: [] },
      timing: "every_committed_state", wording: template.slots,
    },
  }))
}
