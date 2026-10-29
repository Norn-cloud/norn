/** FRET-style ontology-bound sentence slots, with metadata retained in a sidecar.
 * No English interpretation, predicate normalization, evaluator, or proof claim.
 */
import { canonicalJson, type Json, toJson } from "./canonical.ts"
import type { LawIR, LawModality, LawResponse, LawScope, LawTiming } from "./builder/law.ts"

export type LawMetadata = Pick<LawIR, "lawVersion" | "id" | "owner" | "severity" | "rationale" | "provenance">
export interface LawStatement {
  readonly scope: LawScope
  readonly condition?: string
  readonly component: string
  readonly shall: "shall"
  readonly modality: LawModality
  readonly timing?: LawTiming
  readonly response: LawResponse
  readonly metadata: LawMetadata
}
export class StatementError extends Error {}

export function toStatement(law: LawIR): LawStatement {
  const { lawVersion, id, owner, severity, rationale, provenance, scope, subject, condition, modality, timing, response } = law
  return { scope, ...(condition === undefined ? {} : { condition }), component: subject, shall: "shall", modality, ...(timing === undefined ? {} : { timing }), response, metadata: { lawVersion, id, owner, severity, rationale, provenance } }
}
export function fromStatement(statement: LawStatement): LawIR {
  if (statement.shall !== "shall") throw new StatementError("the obligation word must be shall")
  return { ...statement.metadata, scope: statement.scope, subject: statement.component, ...(statement.condition === undefined ? {} : { condition: statement.condition }), modality: statement.modality, ...(statement.timing === undefined ? {} : { timing: statement.timing }), response: statement.response }
}

/** JSON-quoted atoms preserve ZEN source and ontology identities byte for byte.
 * Slots use JSON data only; predicates stay uninterpreted ZEN strings.
 */
export function renderSentence(statement: LawStatement): string {
  const quote = (value: unknown) => JSON.stringify(toJson(value))
  const prefix = `In ${quote(statement.scope)}, ${statement.condition === undefined ? "" : `When ${quote(statement.condition)}, `}${quote(statement.component)} shall `
  const modality = statement.modality.kind === "until" ? `until ${quote(statement.modality.predicate)}` : statement.modality.kind
  const response = statement.response
  const behavior = response.kind === "predicate" ? `satisfy ${quote(response.expression)}`
    : response.kind === "action" ? `invoke ${quote(response.artifactId)}`
    : response.kind === "invariant" ? `satisfy invariant ${quote({ objectType: response.objectType, invariantId: response.invariantId })}`
    : `take transition ${quote({ objectType: response.objectType, property: response.property, transitionId: response.transitionId })}`
  const bound = statement.timing === undefined ? "" : statement.timing.kind === "within" ? ` within ${quote(statement.timing.millis)} ms` : ` before deadline ${quote(statement.timing.property)}`
  return `${prefix}${modality} ${behavior}${bound}`
}

class Reader {
  private offset = 0
  constructor(private readonly text: string) {
    if (text.length > 262_144) throw new StatementError("sentence exceeds the structural size bound")
  }
  take(token: string): boolean {
    if (!this.text.startsWith(token, this.offset)) return false
    this.offset += token.length
    return true
  }
  need(token: string): void { if (!this.take(token)) throw new StatementError(`expected ${token} at ${this.offset}`) }
  /** Scan one JSON atom; strings and balanced data containers, never expression syntax. */
  atom(): unknown {
    const start = this.offset
    let quoted = false, escaped = false, depth = 0
    for (; this.offset < this.text.length; this.offset++) {
      const char = this.text[this.offset]!
      if (quoted) {
        if (escaped) escaped = false
        else if (char === "\\") escaped = true
        else if (char === '"') {
          quoted = false
          if (depth === 0) { this.offset++; break }
        }
      } else if (char === '"') quoted = true
      else if (char === "{" || char === "[") { depth++; if (depth > 64) throw new StatementError("slot nesting exceeds 64") }
      else if (char === "}" || char === "]") { depth--; if (depth === 0) { this.offset++; break } }
      else if (depth === 0) break
    }
    try { return JSON.parse(this.text.slice(start, this.offset)) as unknown } catch { throw new StatementError(`invalid JSON slot at ${start}`) }
  }
  string(): string { const atom = this.atom(); if (typeof atom !== "string") throw new StatementError("slot must be a string"); return atom }
  end(): void { if (this.offset !== this.text.length) throw new StatementError(`unexpected sentence suffix at ${this.offset}`) }
}
const object = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new StatementError("slot must be a closed object")
  return value as Record<string, unknown>
}
const keys = (value: Record<string, unknown>, allowed: string[], required: string[]): void => {
  if (Object.keys(value).some((key) => !allowed.includes(key)) || required.some((key) => !(key in value))) throw new StatementError("unknown or missing slot member")
}
const stringField = (value: Record<string, unknown>, key: string): string => {
  if (typeof value[key] !== "string") throw new StatementError(`${key} must be a string`)
  return value[key] as string
}
function scopeSlot(value: unknown): LawScope {
  const scope = object(value)
  keys(scope, ["root", "objectTypes", "lifecycleStates", "commitments"], ["root", "objectTypes"])
  const root = object(scope.root); keys(root, ["kind", "id"], ["kind", "id"])
  if (!["company", "app", "cell", "entity"].includes(stringField(root, "kind"))) throw new StatementError("unknown scope root")
  stringField(root, "id")
  const strings = (value: unknown): void => { if (!Array.isArray(value) || !value.every((v) => typeof v === "string")) throw new StatementError("slot must list strings") }
  strings(scope.objectTypes)
  if (scope.commitments !== undefined) strings(scope.commitments)
  if (scope.lifecycleStates !== undefined) {
    if (!Array.isArray(scope.lifecycleStates)) throw new StatementError("lifecycleStates must be an array")
    for (const value of scope.lifecycleStates) { const state = object(value); keys(state, ["objectType", "property", "states"], ["objectType", "property", "states"]); stringField(state, "objectType"); stringField(state, "property"); strings(state.states) }
  }
  return scope as unknown as LawScope
}

/** Parse only this published grammar. Ontology references require ordinary Rust set admission. */
export function parseSentence(sentence: string, metadata: LawMetadata): LawStatement {
  const reader = new Reader(sentence)
  reader.need("In "); const scope = scopeSlot(reader.atom()); reader.need(", ")
  let condition: string | undefined
  if (reader.take("When ")) { condition = reader.string(); reader.need(", ") }
  const component = reader.string(); reader.need(" shall ")
  let modality: LawModality
  if (reader.take("until ")) modality = { kind: "until", predicate: reader.string() }
  else if (reader.take("always")) modality = { kind: "always" }
  else if (reader.take("never")) modality = { kind: "never" }
  else if (reader.take("eventually")) modality = { kind: "eventually" }
  else throw new StatementError("unsupported temporal word")
  reader.need(" ")
  let response: LawResponse
  if (reader.take("satisfy invariant ")) {
    const slot = object(reader.atom()); keys(slot, ["objectType", "invariantId"], ["objectType", "invariantId"])
    response = { kind: "invariant", objectType: stringField(slot, "objectType"), invariantId: stringField(slot, "invariantId") }
  } else if (reader.take("satisfy ")) response = { kind: "predicate", expression: reader.string() }
  else if (reader.take("invoke ")) response = { kind: "action", artifactId: reader.string() }
  else if (reader.take("take transition ")) {
    const slot = object(reader.atom()); keys(slot, ["objectType", "property", "transitionId"], ["objectType", "property", "transitionId"])
    response = { kind: "transition", objectType: stringField(slot, "objectType"), property: stringField(slot, "property"), transitionId: stringField(slot, "transitionId") }
  } else throw new StatementError("unsupported response slot")
  let timing: LawTiming | undefined
  if (reader.take(" within ")) { timing = { kind: "within", millis: reader.string() }; reader.need(" ms") }
  else if (reader.take(" before deadline ")) timing = { kind: "before_deadline", property: reader.string() }
  reader.end()
  return { scope, ...(condition === undefined ? {} : { condition }), component, shall: "shall", modality, ...(timing === undefined ? {} : { timing }), response, metadata }
}

/** Lossless wire model: readable sentence plus protected metadata, no duplicate IR. */
export function serializeStatement(statement: LawStatement): string {
  return canonicalJson(toJson({ statementVersion: "0.1", sentence: renderSentence(statement), metadata: statement.metadata }))
}
export function parseStatement(source: string): LawStatement {
  if (source.length > 524_288) throw new StatementError("statement exceeds size bound")
  const wire = object(JSON.parse(source)); keys(wire, ["statementVersion", "sentence", "metadata"], ["statementVersion", "sentence", "metadata"])
  if (wire.statementVersion !== "0.1") throw new StatementError("unsupported statement version")
  const metadata = object(wire.metadata); keys(metadata, ["lawVersion", "id", "owner", "severity", "rationale", "provenance"], ["lawVersion", "id", "owner", "severity", "rationale", "provenance"])
  for (const key of ["id", "owner", "rationale"]) stringField(metadata, key)
  if (metadata.lawVersion !== "0.1" || !["error", "warning"].includes(stringField(metadata, "severity"))) throw new StatementError("invalid law metadata")
  const provenance = object(metadata.provenance); keys(provenance, ["source", "sourcePath", "actorId", "createdAt"], ["source"])
  if (!["authoring_sdk", "agent", "migration", "import", "system"].includes(stringField(provenance, "source"))) throw new StatementError("invalid provenance source")
  for (const key of ["sourcePath", "actorId", "createdAt"]) if (provenance[key] !== undefined) stringField(provenance, key)
  return parseSentence(stringField(wire, "sentence"), metadata as unknown as LawMetadata)
}

/** Counterexample narrative quotes exact ZEN/values, avoiding guessed English semantics. */
export function renderCounterexample(law: LawIR, witness: { readonly state: Json; readonly trace?: ReadonlyArray<string>; readonly reason: string }): string {
  const behavior = law.response.kind === "predicate" ? `satisfy ${law.response.expression}` : law.response.kind === "action" ? `invoke ${law.response.artifactId}` : law.response.kind === "invariant" ? `satisfy invariant ${law.response.objectType}.${law.response.invariantId}` : `take ${law.response.objectType}.${law.response.property}.${law.response.transitionId}`
  const modality = law.modality.kind === "until" ? `keep ${behavior} until ${law.modality.predicate}` : `${law.modality.kind} ${behavior}`
  const bound = law.timing?.kind === "within" ? ` within ${law.timing.millis} milliseconds` : law.timing?.kind === "before_deadline" ? ` before its ${law.timing.property} deadline` : ""
  return `Law “${law.id}” requires ${law.subject} to ${modality}${bound}${law.condition === undefined ? "" : ` when ${law.condition}`}. ${witness.reason}. Observed state: ${JSON.stringify(toJson(witness.state))}.${witness.trace?.length ? ` Path: ${witness.trace.join(" → ")}.` : ""}`
}
