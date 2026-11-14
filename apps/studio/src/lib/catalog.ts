import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import type { Artifact } from "./transport.ts"
import { labelIndex, pick } from "./labels.ts"

type PropertyType = Schemas["NornIrPropertyType"]
type PropertyTypeDef = Schemas["NornIrPropertyTypeDef"]
export type Lifecycle = Schemas["NornIrLifecycle"]

export interface Field {
  readonly id: string
  /** The reader's label for the fact, or the id in words when it has none in their language. */
  readonly label?: string
  /** The language `label` is written in. */
  readonly labelLang?: string
  /**
   * Enum values' labels in the reader's language, by stored value. It holds only the values that have
   * one: a renderer walks `scalar.values` and shows every value, as its label or as stored.
   */
  readonly valueLabels?: ReadonlyMap<string, string>
  readonly type: string
  readonly optional: boolean
  /** Declared exact-decimal facts, with the unit or currency fact their type names. */
  readonly decimal: {
    readonly unit: string | undefined
    readonly perUnit: string | undefined
    readonly currencyFrom: string | undefined
    readonly scale?: number
    /** The properties whose value names this value's unit, in order (the use-site `unitSource`): the first one set is its unit. */
    readonly unitFrom?: readonly string[]
  } | undefined
  /** The declared value kind after resolving named property types; `enum` carries its declared values in order. */
  readonly scalar: Scalar
}

/** A property's value kind as the admitted definition declares it, with named types resolved. */
export type Scalar =
  | { readonly kind: "text" | "integer" | "decimal" | "boolean" | "timestamp" | "date" | "duration" | "record" | "json-ext" | "decimal-binding" }
  | { readonly kind: "enum"; readonly values: readonly string[] }
  | { readonly kind: "unresolved"; readonly name: string }

export interface Relationship {
  readonly id: string
  readonly target: string
  readonly description: string | undefined
  /** How many targets one record may link to: "many", "one", or "at most one" when the link is optional. */
  readonly count: "many" | "one" | "at most one"
}

/** Where one fact's value comes from in one source record, as the mapping declares it. */
export type Source =
  | { readonly kind: "field"; readonly field: string }
  | { readonly kind: "fixed"; readonly value: string }
  | { readonly kind: "empty" }
  | { readonly kind: "computed"; readonly expression: string }
  | { readonly kind: "nested"; readonly dataset: string; readonly collection: string; readonly where: string; readonly value: Source }
  | { readonly kind: "transition"; readonly expression: string }
  | { readonly kind: "gap"; readonly reason: "not_in_source" | "defaulted" | "deferred" | "derived_elsewhere"; readonly note: string | undefined }

export interface Feed {
  readonly system: string
  readonly dataset: string
  /** Declared sources per fact or relationship id (a link may be declared more than once); one missing here is not mapped by this system. */
  readonly facts: ReadonlyMap<string, readonly Source[]>
  /** Source fields the mapping leaves out on purpose, each with the reason it declares. */
  readonly ignored: readonly { readonly field: string; readonly reason: string }[]
}

/** Read one declared mapping expression: `input.code`, `'on_hand'`, `null` or anything else. */
export function sourceOf(expression: string): Source {
  const field = /^input\.([A-Za-z_][A-Za-z0-9_]*)$/.exec(expression)
  if (field?.[1]) return { kind: "field", field: field[1] }
  const fixed = /^'([^']*)'$/.exec(expression)
  if (fixed?.[1] !== undefined) return { kind: "fixed", value: fixed[1] }
  return expression === "null" ? { kind: "empty" } : { kind: "computed", expression }
}

export interface Concept {
  readonly name: string
  readonly title: string
  /** The language `title` is written in: the reader's when the type has a label in it, else `en`. */
  readonly titleLang: string
  /** The label awaits a person's review (machine-proposed). */
  readonly titleNeedsReview: boolean
  /** A concept some connected system feeds, or a core idea the concepts build on. */
  readonly kind: "concept" | "core idea"
  /** What the definition declares: an object type (it can have records) or a shared interface. Feed presence is `kind`. */
  readonly definition: "object type" | "interface"
  /** The definition's one-line name when it says more than the title, e.g. "Owner and location partitioned holding". Business view. */
  readonly summary: string | undefined
  /** The language `summary` is written in: the label's tag when it is the label's description, else `en`. */
  readonly summaryLang: string
  /** The registry's full description; it may name fields and laws, so Studio shows it in the Technical view only. */
  readonly description: string | undefined
  readonly parents: readonly string[]
  readonly fields: readonly Field[]
  readonly relationships: readonly Relationship[]
  readonly rules: readonly string[]
  /** The admitted laws scoped to this type, as declared. */
  readonly laws: readonly Law[]
  readonly feeds: readonly Feed[]
  /** The lifecycles an object type declares; interfaces declare none. */
  readonly lifecycles: readonly Lifecycle[]
}

/** One admitted law, as its LawIR declares it. */
export interface Law {
  readonly id: string
  readonly subject: string
  readonly rationale: string
  readonly severity: "error" | "warning"
  /** The temporal wrapper (`always`, `never`, …) and the response kind (`predicate`, `invariant`, …). */
  readonly modality: string
  readonly response: string
  readonly owner: string
  /**
   * The two facts a predicate law holds equal, when it reads `self.a == self.b` (or `self.a == (self.b ?? …)`), or
   * a total held to the sum over its lines, `self.total == sum(map(self.lines, #.amount))` (then the second is the
   * path `lines.amount`): what a broken record shows side by side.
   */
  readonly compares?: readonly [string, string]
  /** The concept that declares the law, when a concept inherits it (an interface it implements, a supertype). */
  readonly inheritedFrom?: string
}

const IDENT = "[A-Za-z_][A-Za-z0-9_]*"
const PAIR = new RegExp(`^\\s*self\\.(${IDENT})\\s*==\\s*\\(?\\s*self\\.(${IDENT})\\b`)
const SUM = new RegExp(`^\\s*self\\.(${IDENT})\\s*==\\s*sum\\(\\s*map\\(\\s*self\\.(${IDENT})\\s*,\\s*#\\.(${IDENT})\\s*\\)\\s*\\)\\s*$`)

/** The two facts an equality law compares, read from its declared predicate; anything else compares nothing. */
export function comparedFacts(expression: unknown): readonly [string, string] | undefined {
  if (typeof expression !== "string") return undefined
  const sum = SUM.exec(expression)
  if (sum) return [sum[1]!, `${sum[2]!}.${sum[3]!}`]
  const match = PAIR.exec(expression)
  return match ? [match[1]!, match[2]!] : undefined
}

export interface SourceSystem {
  readonly id: string
  readonly name: string
  readonly datasets: readonly string[]
  readonly feeds: readonly string[]
  /** The source schema this mapping reads, as its schema declares it: the instance, schema version and when it was observed. */
  readonly schema: { readonly id: string; readonly instance: string | undefined; readonly version: string | undefined; readonly observedAt: string | undefined } | undefined
}

export interface Catalog {
  readonly concepts: readonly Concept[]
  readonly systems: readonly SourceSystem[]
  readonly byName: ReadonlyMap<string, Concept>
}

const SYSTEM_NAMES: Record<string, string> = { erpnext: "ERPNext", shopify: "Shopify", odoo: "Odoo", "norn-connect": "Norn" }

/**
 * Whether the cell itself writes this source: its schema names the instance `cell` (the sync
 * reports behind each connection's status). Such a source is Norn's own record, not a connected
 * system; uploads and outside systems name their own instance.
 */
export const cellOwned = (system: SourceSystem): boolean => system.schema?.instance === "cell"

/** `synthetic-erpnext` → `ERPNext`; an unknown system keeps the name the server sent. */
export function systemName(system: string): string {
  const bare = system.replace(/^synthetic-/, "")
  return SYSTEM_NAMES[bare] ?? bare
}

/** `canon.SettlementObservation` → `Settlement observation`. */
export function conceptTitle(name: string): string {
  const last = name.slice(name.lastIndexOf(".") + 1).replace(/([a-z])([A-Z])/g, "$1 $2").replaceAll("_", " ")
  return last.charAt(0).toUpperCase() + last.slice(1).toLowerCase()
}

const optionText = (value: string): string => value.replaceAll("_", " ")

function typeText(type: PropertyType, types: ReadonlyMap<string, PropertyTypeDef>): string {
  switch (type.kind) {
    case "decimal": {
      const unit = type.unit ? ` in ${type.unit.code}${type.perUnit ? ` per ${type.perUnit.code}` : ""}` : ""
      const currency = type.currencySource ? `, currency from ${type.currencySource.property.replaceAll("_", " ")}` : ""
      return `Exact number, ${type.scale} decimal places${unit}${currency}`
    }
    case "enum": return `One of: ${type.values.map(({ value }) => optionText(value)).join(", ")}`
    case "ref": {
      const def = types.get(type.name)
      return def ? typeText(def.type, types) : conceptTitle(type.name)
    }
    case "record": return `Record of ${type.members.map(({ id }) => id).join(", ")}`
    case "text": return type.format ? `Text (${type.format})` : "Text"
    case "json-ext": return "Structured data"
    default: return conceptTitle(type.kind)
  }
}

function scalarOf(type: PropertyType, types: ReadonlyMap<string, PropertyTypeDef>, seen: ReadonlySet<string> = new Set()): Scalar {
  if (type.kind === "ref") {
    const def = types.get(type.name)
    return def === undefined || seen.has(type.name) ? { kind: "unresolved", name: type.name } : scalarOf(def.type, types, new Set([...seen, type.name]))
  }
  if (type.kind === "enum") return { kind: "enum", values: type.values.map(({ value }) => value) }
  return { kind: type.kind }
}

function decimalOf(type: PropertyType, types: ReadonlyMap<string, PropertyTypeDef>): Field["decimal"] {
  if (type.kind === "ref") {
    const def = types.get(type.name)
    const resolved = def ? decimalOf(def.type, types) : undefined
    const from = (type as { unitSource?: unknown }).unitSource
    const unitFrom = Array.isArray(from) && from.every((item) => typeof item === "string") ? from as readonly string[] : undefined
    return resolved && unitFrom ? { ...resolved, unitFrom } : resolved
  }
  if (type.kind !== "decimal") return undefined
  const scale = (type as { scale?: unknown }).scale
  return { unit: type.unit?.code, perUnit: type.perUnit?.code, currencyFrom: type.currencySource?.property, ...typeof scale === "number" ? { scale } : {} }
}

/**
 * Index the server's admitted artifacts; nothing is inferred beyond what they declare.
 * Titles and fact names come from `labels` artifacts in `locale` when they have one there.
 */
export function catalogOf(artifacts: readonly Artifact[], locale = "en"): Catalog {
  const labels = labelIndex(artifacts)
  const types = new Map<string, PropertyTypeDef>()
  const rules = new Map<string, string[]>()
  const laws = new Map<string, Law[]>()
  const feeds = new Map<string, Feed[]>()
  const systems: SourceSystem[] = []
  const sourceSystem = new Map<string, string>()
  const sourceSchema = new Map<string, NonNullable<SourceSystem["schema"]>>()
  const entityName = new Map<string, string>()
  for (const artifact of artifacts) {
    if (artifact.kind === "entity" && "propertyTypes" in artifact.payload) for (const def of artifact.payload.propertyTypes) types.set(def.name, def)
    if (artifact.kind === "entity") {
      const payload = artifact.payload
      const name = "objectType" in payload ? payload.objectType.name : "interface" in payload ? payload.interface.name : undefined
      if (name) entityName.set(artifact.artifactId, name)
    }
    if (artifact.kind === "source_schema") {
      const { source, observedAt } = artifact.payload as { source: { system: string; instance?: string; version?: string }; observedAt?: string }
      sourceSystem.set(artifact.artifactId, source.system)
      sourceSchema.set(artifact.artifactId, { id: artifact.artifactId, instance: source.instance, version: source.version, observedAt })
    }
    if (artifact.kind === "law") {
      const law = artifact.payload
      for (const target of law.scope.objectTypes) rules.set(target, [...rules.get(target) ?? [], law.rationale])
      const declared: Law = { id: law.id, subject: law.subject, rationale: law.rationale, severity: law.severity, modality: String((law.modality as { kind?: unknown }).kind ?? ""), response: String((law.response as { kind?: unknown }).kind ?? ""), owner: law.owner, ...(comparedFacts((law.response as { expression?: unknown }).expression) ? { compares: comparedFacts((law.response as { expression?: unknown }).expression)! } : {}) }
      for (const target of new Set([law.subject, ...law.scope.objectTypes])) laws.set(target, [...laws.get(target) ?? [], declared])
    }
  }
  for (const artifact of artifacts) {
    // Interpretation documents carry no source maps; only source mappings feed concepts.
    if (artifact.kind !== "mapping" || !("maps" in artifact.payload)) continue
    const system = systemName(sourceSystem.get(artifact.payload.source.schemaId) ?? artifact.payload.source.schemaId)
    const targets = new Set<string>()
    for (const map of artifact.payload.maps) {
      const target = entityName.get(map.target.entity) ?? map.target.entity
      targets.add(target)
      const facts = new Map<string, Source[]>()
      const add = (id: string, source: Source) => facts.set(id, [...facts.get(id) ?? [], source])
      for (const { property, value, nested } of map.properties ?? []) {
        add(property, nested ? { kind: "nested", dataset: nested.dataset, collection: nested.field, where: nested.where, value: sourceOf(value) } : sourceOf(value))
      }
      for (const { link, key } of map.links ?? []) add(link, sourceOf(key))
      for (const { property, state, transition } of map.lifecycles ?? []) {
        if (state !== undefined) add(property, sourceOf(state))
        else if (transition !== undefined) add(property, { kind: "transition", expression: transition })
      }
      for (const { member, note, reason } of map.gaps ?? []) facts.set(member, [{ kind: "gap", reason, note }])
      const ignored = (map.ignoredFields ?? []).map(({ field, reason }) => ({ field, reason }))
      feeds.set(target, [...feeds.get(target) ?? [], { system, dataset: map.source.dataset, facts, ignored }])
    }
    const datasets = [...new Set(artifact.payload.maps.map(({ source }) => source.dataset))]
    systems.push({ id: artifact.artifactId, name: system, datasets, feeds: [...targets], schema: sourceSchema.get(artifact.payload.source.schemaId) })
  }
  const concepts: Concept[] = []
  for (const artifact of artifacts) {
    if (artifact.kind !== "entity") continue
    const payload = artifact.payload
    let def: Schemas["NornIrObjectTypeDef"] | Schemas["NornIrInterfaceDef"]
    let parents: string[]
    if ("objectType" in payload) {
      def = payload.objectType
      parents = [...def.extends ? [def.extends] : [], ...def.implements ?? []]
    } else if ("interface" in payload) {
      def = payload.interface
      parents = def.extends ?? []
    } else continue
    const invariants = (def.invariants ?? []).map(({ description, message }) => description ?? message)
    const own = pick(labels, `objectType:${def.name}`, locale)
    const title = own?.text ?? conceptTitle(def.name)
    const fed = feeds.get(def.name) ?? []
    concepts.push({
      name: def.name,
      title,
      titleLang: own?.tag ?? "en",
      titleNeedsReview: own?.needsReview ?? false,
      kind: fed.length > 0 ? "concept" : "core idea",
      definition: "objectType" in payload ? "object type" : "interface",
      // The Business summary: the type's description in the reader's language when its label has one,
      // else the definition's own one-line name when it says more than its title in any language.
      summary: own?.description ?? ([def.name, conceptTitle(def.name), title].includes(artifact.name) ? undefined : artifact.name),
      summaryLang: own?.description === undefined ? "en" : own.tag,
      description: def.description,
      parents,
      fields: (def.properties ?? []).map(({ id, type, constraints }) => {
        const label = pick(labels, `property:${def.name}.${id}`, locale)
        const scalar = scalarOf(type, types)
        const valueLabels = new Map(scalar.kind === "enum"
          ? scalar.values.flatMap((value) => { const text = pick(labels, `enumValue:${def.name}.${id}=${value}`, locale)?.text; return text === undefined ? [] : [[value, text] as const] })
          : [])
        return { id, label: label?.text ?? id.replaceAll("_", " "), labelLang: label?.tag ?? "en", valueLabels, type: typeText(type, types), optional: constraints?.nullable === true, decimal: decimalOf(type, types), scalar }
      }),
      relationships: (def.links ?? []).map(({ id, target, description, cardinality }) => ({ id, target, description, count: cardinality.to === "many" ? "many" : cardinality.required === true ? "one" : "at most one" })),
      rules: [...invariants, ...rules.get(def.name) ?? []],
      laws: laws.get(def.name) ?? [],
      feeds: fed,
      lifecycles: "lifecycles" in def ? def.lifecycles ?? [] : [],
    })
  }
  // LAW-INTERFACE-1: a law declared on a concept governs every concept that implements or extends
  // it, so each one lists the laws (and rules) of its ancestors too, marked with where they come from.
  const parentsOf = new Map(concepts.map((concept) => [concept.name, concept.parents]))
  const ancestors = (name: string): string[] => {
    const seen = new Set<string>()
    const pending = [...parentsOf.get(name) ?? []]
    while (pending.length > 0) {
      const next = pending.shift()!
      if (seen.has(next) || next === name) continue
      seen.add(next)
      pending.push(...parentsOf.get(next) ?? [])
    }
    return [...seen]
  }
  const governed = concepts.map((concept) => {
    const inherited = ancestors(concept.name).flatMap((ancestor) => (laws.get(ancestor) ?? []).map((law) => ({ ...law, inheritedFrom: ancestor })))
    const fresh = inherited.filter((law, index) => !concept.laws.some(({ id }) => id === law.id) && inherited.findIndex(({ id }) => id === law.id) === index)
    if (fresh.length === 0) return concept
    const rationales = fresh.map(({ rationale }) => rationale).filter((rationale) => !concept.rules.includes(rationale))
    return { ...concept, laws: [...concept.laws, ...fresh], rules: [...concept.rules, ...rationales] }
  })
  governed.sort((a, b) => a.title.localeCompare(b.title, locale))
  return { concepts: governed, systems, byName: new Map(governed.map((concept) => [concept.name, concept])) }
}
