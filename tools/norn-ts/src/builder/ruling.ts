/**
 * Claims and rulings (norn#62): typed constructors lowered to the Rust-owned
 * ClaimIR and RulingIR contracts. Authors never write the IR: members,
 * evidence, statements and outcomes are built with the handles below, and a
 * ruling's ratification is its envelope lifecycle, which authoring never sets
 * past `draft`.
 */
import { type Json, toJson } from "../canonical.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"

/** An exact artifact identity. */
export interface Pin { readonly artifactId: string; readonly version: string }

export type Member =
  | { readonly kind: "artifact" }
  | { readonly kind: "type"; readonly typeName: string }
  | { readonly kind: "property"; readonly typeName: string; readonly property: string }
  | { readonly kind: "link"; readonly typeName: string; readonly link: string }
  | { readonly kind: "state"; readonly typeName: string; readonly lifecycle: string; readonly state: string }
  | { readonly kind: "transition"; readonly typeName: string; readonly lifecycle: string; readonly transition: string }
  | { readonly kind: "object_map"; readonly map: string }
  | { readonly kind: "law"; readonly law: string }

/** A member of one exact artifact (the catalog's `MemberRef`). */
export interface MemberRef extends Pin { readonly member: Member }

export type EvidenceStrength = "structural" | "exhaustive_finite" | "bounded" | "checked_proof" | "runtime_enforced" | "sampled" | "refuted" | "unknown"
export type EvidenceSource =
  | { readonly kind: "artifact"; readonly artifactId: string; readonly version: string; readonly sha256: string }
  | { readonly kind: "upstream"; readonly system: string; readonly release: string; readonly locator: string }
export interface Evidence { readonly source: EvidenceSource; readonly strength: EvidenceStrength; readonly note?: string }

export interface Observation { readonly mapping: Pin; readonly map: string; readonly record: Readonly<Record<string, Json>> }
export type ClaimStatement =
  | { readonly kind: "derived_state"; readonly state: MemberRef; readonly from: ReadonlyArray<MemberRef> }
  | { readonly kind: "divergence"; readonly witness: { readonly target: MemberRef; readonly left: Observation; readonly right: Observation } }

export interface ClaimIR {
  readonly claimVersion: "0.1"
  readonly id: string
  readonly statement: ClaimStatement
  readonly evidence: ReadonlyArray<Evidence>
  readonly note?: string
}

export type ParameterAuthority = "tenant" | "jurisdiction" | "normative_profile"
export type RevisitTrigger = { readonly kind: "date"; readonly on: string } | { readonly kind: "evidence"; readonly source: EvidenceSource }
export type Outcome =
  | { readonly kind: "choose"; readonly definition: MemberRef }
  | { readonly kind: "parameterise"; readonly parameter: MemberRef; readonly default: Json; readonly setBy: ReadonlyArray<ParameterAuthority> }
  | { readonly kind: "scoped"; readonly cases: ReadonlyArray<{ readonly scope: Readonly<Record<string, string>>; readonly definition: MemberRef }> }
  | { readonly kind: "defer"; readonly revisit: RevisitTrigger }
  | { readonly kind: "reject" }
export type DecisionAuthority = "owner" | "lead" | "agent"

export interface RulingIR {
  readonly rulingVersion: "0.1"
  readonly id: string
  readonly resolves: ReadonlyArray<Pin>
  readonly outcome: Outcome
  readonly affects: ReadonlyArray<MemberRef>
  readonly decidedBy: { readonly principal: string; readonly authority: DecisionAuthority }
  readonly decidedAt: string
  readonly evidence?: ReadonlyArray<Evidence>
  readonly alternatives?: ReadonlyArray<{ readonly outcome: Outcome; readonly note?: string }>
  readonly supersedes?: Pin
  readonly note?: string
}

// ------------------------------------------------------------- handles

export const pin = (artifactId: string, version: string): Pin => ({ artifactId, version })

/** Definition-level members of an exact artifact. */
export const meta = {
  artifact: (at: Pin): MemberRef => ({ ...at, member: { kind: "artifact" } }),
  type: (at: Pin, typeName: string): MemberRef => ({ ...at, member: { kind: "type", typeName } }),
  property: (at: Pin, typeName: string, property: string): MemberRef => ({ ...at, member: { kind: "property", typeName, property } }),
  link: (at: Pin, typeName: string, link: string): MemberRef => ({ ...at, member: { kind: "link", typeName, link } }),
  state: (at: Pin, typeName: string, lifecycle: string, state: string): MemberRef => ({ ...at, member: { kind: "state", typeName, lifecycle, state } }),
  transition: (at: Pin, typeName: string, lifecycle: string, transition: string): MemberRef => ({ ...at, member: { kind: "transition", typeName, lifecycle, transition } }),
  objectMap: (at: Pin, map: string): MemberRef => ({ ...at, member: { kind: "object_map", map } }),
  law: (at: Pin, law: string): MemberRef => ({ ...at, member: { kind: "law", law } }),
} as const

const withNote = <T extends object>(value: T, note: string | undefined): T & { readonly note?: string } => (note === undefined ? value : { ...value, note })

export const evidence = {
  upstream: (system: string, release: string, locator: string, strength: EvidenceStrength, note?: string): Evidence =>
    withNote({ source: { kind: "upstream", system, release, locator }, strength }, note),
  artifact: (at: Pin, sha256: string, strength: EvidenceStrength, note?: string): Evidence =>
    withNote({ source: { kind: "artifact", artifactId: at.artifactId, version: at.version, sha256 }, strength }, note),
} as const

export const statement = {
  derivedState: (state: MemberRef, from: ReadonlyArray<MemberRef>): ClaimStatement => ({ kind: "derived_state", state, from }),
  divergence: (target: MemberRef, left: Observation, right: Observation): ClaimStatement => ({ kind: "divergence", witness: { target, left, right } }),
} as const

export const observe = (mapping: Pin, map: string, record: Readonly<Record<string, Json>>): Observation => ({ mapping, map, record })

export const outcome = {
  choose: (definition: MemberRef): Outcome => ({ kind: "choose", definition }),
  parameterise: (parameter: MemberRef, defaultValue: Json, setBy: ReadonlyArray<ParameterAuthority>): Outcome => ({ kind: "parameterise", parameter, default: defaultValue, setBy }),
  scoped: (cases: ReadonlyArray<{ readonly scope: Readonly<Record<string, string>>; readonly definition: MemberRef }>): Outcome => ({ kind: "scoped", cases }),
  deferUntil: (on: string): Outcome => ({ kind: "defer", revisit: { kind: "date", on } }),
  reject: (): Outcome => ({ kind: "reject" }),
} as const

// ---------------------------------------------------------- definitions

interface Base { readonly id: string; readonly title: string; readonly version: string }

export interface ClaimMeta extends Base {
  readonly statement: ClaimStatement
  readonly evidence: ReadonlyArray<Evidence>
  readonly note?: string
}
export interface ClaimDef extends ClaimMeta { readonly _tag: "Claim" }

export interface RulingMeta extends Base {
  readonly resolves: ReadonlyArray<Pin>
  readonly outcome: Outcome
  readonly affects: ReadonlyArray<MemberRef>
  readonly decidedBy: RulingIR["decidedBy"]
  readonly decidedAt: string
  readonly evidence?: ReadonlyArray<Evidence>
  readonly alternatives?: RulingIR["alternatives"]
  readonly supersedes?: Pin
  readonly note?: string
}
export interface RulingDef extends RulingMeta { readonly _tag: "Ruling" }

/** The stable id is the artifact id without its kind suffix. */
const stableId = (artifactId: string, kind: string): string => artifactId.endsWith(`.${kind}`) ? artifactId.slice(0, -(kind.length + 1)) : artifactId

export function claim(meta: ClaimMeta): ClaimDef {
  toJson(meta)
  return { _tag: "Claim", ...meta }
}

export function ruling(meta: RulingMeta): RulingDef {
  toJson(meta)
  return { _tag: "Ruling", ...meta }
}

export function claimIR(def: ClaimMeta): ClaimIR {
  return withNote({ claimVersion: "0.1" as const, id: stableId(def.id, "claim"), statement: def.statement, evidence: def.evidence }, def.note)
}

export function rulingIR(def: RulingMeta): RulingIR {
  return {
    rulingVersion: "0.1",
    id: stableId(def.id, "ruling"),
    resolves: def.resolves,
    outcome: def.outcome,
    affects: def.affects,
    decidedBy: def.decidedBy,
    decidedAt: def.decidedAt,
    ...(def.evidence === undefined ? {} : { evidence: def.evidence }),
    ...(def.alternatives === undefined ? {} : { alternatives: def.alternatives }),
    ...(def.supersedes === undefined ? {} : { supersedes: def.supersedes }),
    ...(def.note === undefined ? {} : { note: def.note }),
  }
}

export function lowerClaim(def: ClaimDef, options: EnvelopeOptions): { readonly ok: true; readonly artifact: Json } {
  return { ok: true, artifact: envelope({ kind: "claim", artifactId: def.id, namespace: def.id.split(".")[0]!, name: def.title, version: def.version }, claimIR(def), options) }
}

export function lowerRuling(def: RulingDef, options: EnvelopeOptions): { readonly ok: true; readonly artifact: Json } {
  return { ok: true, artifact: envelope({ kind: "ruling", artifactId: def.id, namespace: def.id.split(".")[0]!, name: def.title, version: def.version }, rulingIR(def), options) }
}
