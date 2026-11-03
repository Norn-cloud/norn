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
  readonly many: boolean
}

export interface Feed {
  readonly system: string
  readonly dataset: string
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
  for (const artifact of artifacts) {
    if (artifact.kind === "entity" && "propertyTypes" in artifact.payload) for (const def of artifact.payload.propertyTypes) types.set(def.name, def)
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
      const target = map.target.entity.replace(/\.entity$/, "")
      targets.add(target)
      feeds.set(target, [...feeds.get(target) ?? [], { system, dataset: map.source.dataset }])
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
    let invariants: string[] = []
    if ("objectType" in payload) {
      def = payload.objectType
      parents = [...def.extends ? [def.extends] : [], ...def.implements ?? []]
      invariants = (def.invariants ?? []).map(({ description, message }) => description ?? message)
    } else if ("interface" in payload) {
      def = payload.interface
      parents = def.extends ?? []
    } else continue
    const title = conceptTitle(def.name)
    const fed = feeds.get(def.name) ?? []
    concepts.push({
      name: def.name,
      title,
      kind: fed.length > 0 ? "concept" : "core idea",
      description: def.description ?? ([def.name, title].includes(artifact.name) ? undefined : artifact.name),
      parents,
      fields: (def.properties ?? []).map(({ id, type, constraints }) => ({ id, type: typeText(type, types), optional: constraints?.nullable === true })),
      relationships: (def.links ?? []).map(({ id, target, description, cardinality }) => ({ id, target, description, many: cardinality.to === "many" })),
      rules: [...invariants, ...rules.get(def.name) ?? []],
      feeds: fed,
    })
  }
  concepts.sort((a, b) => a.title.localeCompare(b.title))
  return { concepts, systems, byName: new Map(concepts.map((concept) => [concept.name, concept])) }
}
