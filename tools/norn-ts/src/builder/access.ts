/**
 * Closure-free AccessIR builder (T7-TS-2, #2690): the lowering target of
 * `Norn.access`. Mirrors `AccessIr` in `crates/norn-ir/src/access.rs`: permit
 * rules, each a conjunction of typed conditions under a mandatory structural
 * scope conjunct. The grammar is closed (no expression node); the Rust
 * checker validates it and compiles it to Cedar.
 */
import type { Json } from "../canonical.ts"
import { type BuildDiagnostic, Diagnostics, type SourceLocation, located } from "./diagnostics.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"

export type ScopeDimension = "org" | "tenant" | "workspace" | "environment" | "businessUnit"
export type CompareOp = "eq" | "ne" | "lt" | "le" | "gt" | "ge"

export interface ContextComparison {
  readonly name: string
  readonly op: CompareOp
  readonly value: boolean | number | string
}

export type ResourceCompareOp = CompareOp | "in"
export type ResourceOperand = boolean | number | string | { readonly principalAttribute: string }

export interface ResourceComparison {
  readonly name: string
  readonly op: ResourceCompareOp
  readonly value: ResourceOperand
}

/** One typed conjunct (`AccessCondition`), externally tagged. */
export type AccessCondition =
  | { readonly marking: { readonly anyOf: ReadonlyArray<string> } }
  | { readonly role: { readonly anyOf: ReadonlyArray<string> } }
  | { readonly rel: { readonly from: "resource" | "principal"; readonly role: string } }
  | { readonly context: ContextComparison }
  | { readonly resource: ResourceComparison }

export interface AccessRuleDecl {
  readonly ruleId: string
  /** ActionIR ids this rule permits. */
  readonly actions: ReadonlyArray<string>
  /** The EntityIR v2 object type of the resource. */
  readonly resourceType: string
  readonly scope: { readonly dimensions: ReadonlyArray<ScopeDimension> }
  readonly allOf?: ReadonlyArray<AccessCondition>
}

export interface AccessDef {
  readonly _tag: "Access"
  /** `policyId`; the artifact is `<id>.policy` in the id's first namespace segment. */
  readonly id: string
  readonly title: string
  readonly version: string
  readonly rules: ReadonlyArray<AccessRuleDecl>
  readonly at?: SourceLocation
}

export function access(meta: Omit<AccessDef, "_tag" | "at"> & { readonly at?: SourceLocation | undefined }): AccessDef {
  const { at, rules, ...rest } = meta
  return located({ _tag: "Access" as const, ...rest, rules: [...rules] }, at)
}

export type AccessBuildResult =
  | { readonly ok: true; readonly artifact: Json }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<BuildDiagnostic> }

const POLICY_ID = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/

/** Lower one access policy to its `policy` artifact (`<id>.policy`). */
export function lowerAccess(def: AccessDef, options: EnvelopeOptions): AccessBuildResult {
  const diagnostics = new Diagnostics()
  if (!POLICY_ID.test(def.id)) diagnostics.report("NORN-FLOW-LITERAL", `policy id ${JSON.stringify(def.id)} must be dotted lower-case segments (commerce.sales_order.access)`, def.at)
  if (!diagnostics.empty) return { ok: false, diagnostics: diagnostics.items }
  const payload = {
    accessVersion: "0.1",
    policyId: def.id,
    rules: def.rules.map((r) => ({
      ruleId: r.ruleId,
      actions: r.actions,
      resourceType: r.resourceType,
      scope: { dimensions: r.scope.dimensions },
      allOf: r.allOf === undefined || r.allOf.length === 0 ? undefined : r.allOf,
    })),
  }
  const artifact = envelope(
    { kind: "policy", artifactId: `${def.id}.policy`, namespace: def.id.slice(0, def.id.indexOf(".")), name: def.title, version: def.version },
    payload,
    options,
  )
  return { ok: true, artifact }
}
