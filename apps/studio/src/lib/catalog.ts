import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import type { Artifact } from "./transport.ts"

type PropertyType = Schemas["NornIrPropertyType"]
type PropertyTypeDef = Schemas["NornIrPropertyTypeDef"]

export interface Field {
  readonly id: string
  readonly type: string
  readonly optional: boolean
}

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
  | { readonly kind: "nested"; readonly dataset: string; readonly field: string; readonly where: string }
  | { readonly kind: "gap"; readonly reason: "not_in_source" | "defaulted" | "deferred" | "derived_elsewhere"; readonly note: string | undefined }

export interface Feed {
  readonly system: string
  readonly dataset: string
  /** Declared sources per fact or relationship id (a link may be declared more than once); one missing here is not mapped by this system. */
  readonly facts: ReadonlyMap<string, readonly Source[]>
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
  /** A concept some connected system feeds, or a core idea the concepts build on. */
  readonly kind: "concept" | "core idea"
  readonly description: string | undefined
  readonly parents: readonly string[]
  readonly fields: readonly Field[]
  readonly relationships: readonly Relationship[]
  readonly rules: readonly string[]
  readonly feeds: readonly Feed[]
}

export interface SourceSystem {
  readonly id: string
  readonly name: string
  readonly datasets: readonly string[]
  readonly feeds: readonly string[]
}

export interface Catalog {
  readonly concepts: readonly Concept[]
  readonly systems: readonly SourceSystem[]
  readonly byName: ReadonlyMap<string, Concept>
}

const SYSTEM_NAMES: Record<string, string> = { erpnext: "ERPNext", shopify: "Shopify", odoo: "Odoo" }

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

/** Index the server's admitted artifacts; nothing is inferred beyond what they declare. */
export function catalogOf(artifacts: readonly Artifact[]): Catalog {
  const types = new Map<string, PropertyTypeDef>()
  const rules = new Map<string, string[]>()
  const feeds = new Map<string, Feed[]>()
  const systems: SourceSystem[] = []
  const sourceSystem = new Map<string, string>()
  const entityName = new Map<string, string>()
  for (const artifact of artifacts) {
    if (artifact.kind === "entity" && "propertyTypes" in artifact.payload) for (const def of artifact.payload.propertyTypes) types.set(def.name, def)
    if (artifact.kind === "entity") {
      const payload = artifact.payload
      const name = "objectType" in payload ? payload.objectType.name : "interface" in payload ? payload.interface.name : undefined
      if (name) entityName.set(artifact.artifactId, name)
    }
    if (artifact.kind === "source_schema") sourceSystem.set(artifact.artifactId, artifact.payload.source.system)
    if (artifact.kind === "law") {
      const law = artifact.payload
      for (const target of law.scope.objectTypes) rules.set(target, [...rules.get(target) ?? [], law.rationale])
    }
  }
  for (const artifact of artifacts) {
    if (artifact.kind !== "mapping") continue
    const system = systemName(sourceSystem.get(artifact.payload.source.schemaId) ?? artifact.payload.source.schemaId)
    const targets = new Set<string>()
    for (const map of artifact.payload.maps) {
      const target = entityName.get(map.target.entity) ?? map.target.entity
      targets.add(target)
      const facts = new Map<string, Source[]>()
      const add = (id: string, source: Source) => facts.set(id, [...facts.get(id) ?? [], source])
      for (const { property, value, nested } of map.properties ?? []) {
        add(property, nested ? { kind: "nested", dataset: nested.dataset, field: nested.field, where: nested.where } : sourceOf(value))
      }
      for (const { link, key } of map.links ?? []) add(link, sourceOf(key))
      for (const { property, state } of map.lifecycles ?? []) if (state !== undefined) add(property, sourceOf(state))
      for (const { member, note, reason } of map.gaps ?? []) facts.set(member, [{ kind: "gap", reason, note }])
      feeds.set(target, [...feeds.get(target) ?? [], { system, dataset: map.source.dataset, facts }])
    }
    const datasets = [...new Set(artifact.payload.maps.map(({ source }) => source.dataset))]
    systems.push({ id: artifact.artifactId, name: system, datasets, feeds: [...targets] })
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
    const title = conceptTitle(def.name)
    const fed = feeds.get(def.name) ?? []
    concepts.push({
      name: def.name,
      title,
      kind: fed.length > 0 ? "concept" : "core idea",
      description: def.description ?? ([def.name, title].includes(artifact.name) ? undefined : artifact.name),
      parents,
      fields: (def.properties ?? []).map(({ id, type, constraints }) => ({ id, type: typeText(type, types), optional: constraints?.nullable === true })),
      relationships: (def.links ?? []).map(({ id, target, description, cardinality }) => ({ id, target, description, count: cardinality.to === "many" ? "many" : cardinality.required === true ? "one" : "at most one" })),
      rules: [...invariants, ...rules.get(def.name) ?? []],
      feeds: fed,
    })
  }
  concepts.sort((a, b) => a.title.localeCompare(b.title))
  return { concepts, systems, byName: new Map(concepts.map((concept) => [concept.name, concept])) }
}
