/** Structured normative authoring data. Rust owns the wire contract and lowering.
 * This builder grants no permissions and verifies no authority signatures. */
import { toJson } from "../canonical.ts"
import type { LawException, LawIR } from "./law.ts"

export interface RuleSourceSpan {
  readonly artifactId: string
  readonly pointer: string
  readonly bytes?: { readonly start: number; readonly end: number }
}
export interface AuthoritySource {
  readonly id: string
  readonly version: string
  readonly documentSha256: string
  readonly signer: string
  readonly signature: string
  readonly source: RuleSourceSpan
}
export interface DeonticRule {
  readonly law: LawIR
  readonly modality: "obligation" | "permission" | "prohibition"
  readonly authorityId: string
  readonly authorityVersion: string
  readonly source: RuleSourceSpan
  /** Exceptions live here or on `law.exceptions`, never both. */
  readonly exceptions?: ReadonlyArray<LawException>
  readonly defeatedBy?: ReadonlyArray<string>
}
export interface DeonticIR {
  readonly deonticVersion: "0.1"
  readonly authorities: ReadonlyArray<AuthoritySource>
  readonly rules: ReadonlyArray<DeonticRule>
}

/** The reviewed slots are authoritative; prose is a projection of these slots. */
export function deontic(definition: DeonticIR): DeonticIR {
  toJson(definition)
  return definition
}

/** A controlled sentence retaining exact ontology identities and ZEN predicates.
 * JSON quoting prevents conditions/identities from masquerading as prose slots. */
export function renderDeontic(rule: DeonticRule): string {
  const quote = JSON.stringify
  const grade = { obligation: "must", permission: "may", prohibition: "must not" }[rule.modality]
  const when = rule.law.condition === undefined ? "" : ` WHEN ${quote(rule.law.condition)}`
  const timing = rule.law.timing === undefined ? "" : ` WITH TIMING ${quote(rule.law.timing)}`
  if (rule.exceptions?.length && rule.law.exceptions?.length) {
    throw new Error("law.exception.two_homes: declare exceptions on the rule or on law.exceptions, not both")
  }
  const declared = rule.exceptions?.length ? rule.exceptions : (rule.law.exceptions ?? [])
  const lifts = (e: LawException) => (e.overrides === rule.law.id ? "" : ` OVERRIDING ${quote(e.overrides)}`)
  const exceptions = declared.map(e => ` EXCEPT ${quote(e.id)}${lifts(e)} WHEN ${quote(e.condition)}`).join("")
  const priority = (rule.defeatedBy ?? []).map(id => ` YIELDING TO ${quote(id)}`).join("")
  return `IN ${quote(rule.law.scope)}${when}, ${quote(rule.law.subject)} ${grade} WITH MODALITY ${quote(rule.law.modality)} ${quote(rule.law.response)}${timing}${exceptions}${priority} ACCORDING TO ${quote(rule.authorityId)} VERSION ${quote(rule.authorityVersion)}.`
}
