import type { Schemas } from "../../norn-sdk/src/index.ts"
import { escape, renderShell, UNLABELLED_READ } from "./shell.ts"

type Artifact = Schemas["NornArtifactEnvelope"]
type LawArtifact = Schemas["NornArtifactEnvelopeLaw"]
type RelationArtifact = Schemas["NornArtifactEnvelopeRelation"]
type PropertyType = Schemas["NornIrPropertyType"]
type Constraints = Schemas["NornIrPropertyConstraints"]
type Property = { readonly id: string; readonly type: PropertyType; readonly constraints?: Constraints; readonly description?: string; readonly computed?: string }
type Link = Schemas["NornIrInterfaceLink"]
type Lifecycle = Schemas["NornIrLifecycle"]
type Role = Schemas["NornIrConceptRole"]

interface Concept {
  readonly anchor: string
  readonly name: string
  readonly kind: "interface" | "object type" | "extension"
  readonly role: Role | undefined
  readonly description: string | undefined
  readonly parents: readonly { readonly name: string; readonly via: "extends" | "implements" }[]
  readonly properties: readonly Property[]
  readonly links: readonly Link[]
  readonly lifecycles: readonly Lifecycle[]
  readonly artifact: Artifact
}

interface ValueType {
  readonly anchor: string
  readonly def: Schemas["NornIrPropertyTypeDef"]
}

interface Index {
  readonly concepts: readonly Concept[]
  readonly valueTypes: readonly ValueType[]
  readonly laws: readonly LawArtifact[]
  readonly relations: readonly RelationArtifact[]
  readonly other: readonly Artifact[]
  readonly conceptAnchor: ReadonlyMap<string, string>
  readonly valueAnchor: ReadonlyMap<string, string>
}

const code = (value: string): string => `<code>${escape(value)}</code>`

/** Group the document's own declarations; nothing is inherited, resolved or inferred here. */
function index(artifacts: readonly Artifact[]): Index {
  const concepts: Concept[] = []
  const valueTypes: ValueType[] = []
  const laws: LawArtifact[] = []
  const relations: RelationArtifact[] = []
  const other: Artifact[] = []
  for (const artifact of artifacts) {
    if (artifact.kind === "law") { laws.push(artifact); continue }
    if (artifact.kind === "relation") { relations.push(artifact); continue }
    if (artifact.kind !== "entity") { other.push(artifact); continue }
    const payload = artifact.payload
    const anchor = `concept-${concepts.length}`
    if ("interface" in payload) {
      const def = payload.interface
      concepts.push({ anchor, name: def.name, kind: "interface", role: def.role, description: def.description,
        parents: (def.extends ?? []).map((name) => ({ name, via: "extends" as const })),
        properties: def.properties ?? [], links: def.links ?? [], lifecycles: [], artifact })
    } else if ("objectType" in payload) {
      const def = payload.objectType
      concepts.push({ anchor, name: def.name, kind: "object type", role: def.role, description: def.description,
        parents: [...(def.extends ? [{ name: def.extends, via: "extends" as const }] : []),
          ...(def.implements ?? []).map((name) => ({ name, via: "implements" as const }))],
        properties: def.properties ?? [], links: def.links ?? [], lifecycles: def.lifecycles ?? [], artifact })
    } else if ("objectTypeExtension" in payload) {
      const def = payload.objectTypeExtension
      concepts.push({ anchor, name: def.target, kind: "extension", role: undefined, description: def.description,
        parents: [], properties: def.properties ?? [], links: def.links ?? [], lifecycles: [], artifact })
    } else if ("propertyTypes" in payload) {
      for (const def of payload.propertyTypes) valueTypes.push({ anchor: `value-${valueTypes.length}`, def })
    } else {
      other.push(artifact)
    }
  }
  const conceptAnchor = new Map<string, string>()
  for (const concept of concepts) if (concept.kind !== "extension" && !conceptAnchor.has(concept.name)) conceptAnchor.set(concept.name, concept.anchor)
  const valueAnchor = new Map<string, string>()
  for (const value of valueTypes) if (!valueAnchor.has(value.def.name)) valueAnchor.set(value.def.name, value.anchor)
  return { concepts, valueTypes, laws, relations, other, conceptAnchor, valueAnchor }
}

const conceptRef = (name: string, catalog: Index): string => {
  const anchor = catalog.conceptAnchor.get(name)
  return anchor ? `<a href="#${anchor}">${code(name)}</a>` : code(name)
}

function typeLabel(type: PropertyType, catalog: Index): string {
  switch (type.kind) {
    case "decimal": {
      const unit = type.unit ? ` <span class="unit">${escape(type.unit.code)}${type.perUnit ? ` / ${escape(type.perUnit.code)}` : ""}</span>` : ""
      const extra = [
        type.rounding ? `rounding ${escape(type.rounding.replaceAll("_", " "))}` : "",
        type.currencySource ? `currency from ${code([...(type.currencySource.links ?? []), type.currencySource.property].join("."))}` : "",
      ].filter(Boolean).join(" · ")
      return `decimal, scale ${escape(String(type.scale))}${unit}${extra ? ` <span class="sub">${extra}</span>` : ""}`
    }
    case "enum": return `one of ${type.values.map(({ value }) => code(value)).join(" ")}`
    case "ref": {
      const anchor = catalog.valueAnchor.get(type.name)
      return anchor ? `<a href="#${anchor}">${code(type.name)}</a>` : code(type.name)
    }
    case "record": return `record of ${type.members.map(({ id }) => code(id)).join(" ")}`
    case "json-ext": return "JSON"
    case "text": return type.format ? `text (${escape(type.format)})` : "text"
    default: return escape(type.kind)
  }
}

function constraintLabel(constraints: Constraints | undefined): string {
  if (!constraints) return ""
  return [
    constraints.nullable ? "optional" : "",
    constraints.min !== undefined ? `≥ ${escape(constraints.min)}` : "",
    constraints.max !== undefined ? `≤ ${escape(constraints.max)}` : "",
    constraints.minLength !== undefined ? `length ≥ ${constraints.minLength}` : "",
    constraints.maxLength !== undefined ? `length ≤ ${constraints.maxLength}` : "",
    constraints.pattern !== undefined ? `pattern ${code(constraints.pattern)}` : "",
  ].filter(Boolean).join(" · ")
}

const cardinality = ({ from, to, required }: Link["cardinality"]): string =>
  `${from} → ${to}${required ? " · required" : ""}`

function propertiesTable(properties: readonly Property[], catalog: Index): string {
  if (!properties.length) return ""
  const rows = properties.map((property) => `<tr><td>${code(property.id)}${property.computed !== undefined ? ' <span class="chip">derived</span>' : ""}</td><td>${typeLabel(property.type, catalog)}</td><td class="sub">${constraintLabel(property.constraints)}${property.description ? `${property.constraints ? " · " : ""}${escape(property.description)}` : ""}</td></tr>`).join("")
  return `<h4>Properties</h4><div class="scroll"><table><thead><tr><th>Name</th><th>Type</th><th>Notes</th></tr></thead><tbody>${rows}</tbody></table></div>`
}

function linksTable(links: readonly Link[], catalog: Index): string {
  if (!links.length) return ""
  const rows = links.map((link) => `<tr><td>${code(link.id)}</td><td>${conceptRef(link.target, catalog)}</td><td>${cardinality(link.cardinality)}</td></tr>`).join("")
  return `<h4>Links</h4><div class="scroll"><table><thead><tr><th>Link</th><th>To</th><th>Cardinality</th></tr></thead><tbody>${rows}</tbody></table></div>`
}

function lifecycleTable(lifecycle: Lifecycle): string {
  const marks = (state: string): string => [
    lifecycle.initial.includes(state) ? "initial" : "",
    lifecycle.terminal?.includes(state) ? "terminal" : "",
    lifecycle.immutable?.includes(state) ? "immutable" : "",
  ].filter(Boolean).map((mark) => `<span class="chip">${mark}</span>`).join(" ")
  const states = [...new Set([...lifecycle.initial, ...(lifecycle.transitions ?? []).flatMap(({ from, to }) => [...from, to])])]
  const rows = (lifecycle.transitions ?? []).map((transition) => `<tr><td>${code(transition.id)}</td><td>${transition.from.map(code).join(" ")}</td><td>${code(transition.to)}</td><td class="sub">${transition.guards?.length ? `${transition.guards.length} guard${transition.guards.length === 1 ? "" : "s"}` : ""}${transition.description ? `${transition.guards?.length ? " · " : ""}${escape(transition.description)}` : ""}</td></tr>`).join("")
  return `<h4>Lifecycle of ${code(lifecycle.property)}</h4>
<div class="chips" aria-label="States">${states.map((state) => `<span>${code(state)} ${marks(state)}</span>`).join('<span class="sub" aria-hidden="true">·</span>')}</div>
${rows ? `<div class="scroll"><table><thead><tr><th>Transition</th><th>From</th><th>To</th><th>Notes</th></tr></thead><tbody>${rows}</tbody></table></div>` : ""}`
}

function conceptCard(concept: Concept, catalog: Index): string {
  const chips = [
    concept.role ? `<span class="chip ${concept.role}">${concept.role}</span>` : "",
    `<span class="chip">${concept.kind}</span>`,
  ].join("")
  const parents = concept.parents.length
    ? `<dl>${(["extends", "implements"] as const).map((via) => {
      const names = concept.parents.filter((parent) => parent.via === via)
      return names.length ? `<dt>${via === "extends" ? "Extends" : "Implements"}</dt><dd>${names.map(({ name }) => conceptRef(name, catalog)).join(", ")}</dd>` : ""
    }).join("")}</dl>`
    : ""
  const extension = concept.kind === "extension" ? `<dl><dt>Adds to</dt><dd>${conceptRef(concept.name, catalog)}</dd></dl>` : ""
  const laws = catalog.laws.filter((law) => law.payload.subject === concept.name)
  const lawList = laws.length && concept.kind !== "extension"
    ? `<h4>Declared laws</h4><ul>${laws.map((law) => `<li><a href="#law-${catalog.laws.indexOf(law)}">${escape(law.name)}</a></li>`).join("")}</ul>`
    : ""
  return `<article class="card" id="${concept.anchor}" aria-labelledby="${concept.anchor}-name">
  <header><div><h3 id="${concept.anchor}-name">${escape(concept.name)}</h3><span class="sub">${code(concept.artifact.artifactId)} · ${escape(concept.artifact.version)}</span></div><div class="chips">${chips}</div></header>
  ${concept.description ? `<p class="desc">${escape(concept.description)}</p>` : ""}
  ${parents}${extension}
  ${propertiesTable(concept.properties, catalog)}
  ${linksTable(concept.links, catalog)}
  ${concept.lifecycles.map(lifecycleTable).join("")}
  ${lawList}
</article>`
}

const NODE_W = 172
const NODE_H = 46
const GAP_X = 20
const GAP_Y = 64
const PAD = 12
const COLUMNS = 5

/**
 * Concept graph: declared `extends`/`implements` edges, parents above children.
 * Supertypes outside this document are drawn dashed and are not expanded.
 * Concepts without any declared supertype edge are listed, not drawn.
 */
function conceptGraph(catalog: Index): string {
  const declared = catalog.concepts.filter((concept) => concept.kind !== "extension")
  if (!declared.length) return ""
  const roles = new Map<string, Role | undefined>()
  const parents = new Map<string, readonly Concept["parents"][number][]>()
  for (const concept of declared) {
    if (!parents.has(concept.name)) { parents.set(concept.name, concept.parents); roles.set(concept.name, concept.role) }
  }
  const connected = new Set<string>()
  for (const [child, ups] of parents) for (const { name } of ups) { connected.add(child); connected.add(name) }
  const order = [...parents.keys(), ...[...connected].filter((name) => !parents.has(name))].filter((name) => connected.has(name))
  const standalone = [...parents.keys()].filter((name) => !connected.has(name))
  const list = standalone.length
    ? `<p class="sub">Without declared supertypes or subtypes: ${standalone.map((name) => conceptRef(name, catalog)).join(", ")}</p>`
    : ""
  if (!order.length) return list
  const rank = new Map<string, number>()
  const depth = (name: string, seen: ReadonlySet<string>): number => {
    const known = rank.get(name)
    if (known !== undefined) return known
    const ups = (parents.get(name) ?? []).filter(({ name: up }) => !seen.has(up))
    const value = ups.length ? 1 + Math.max(...ups.map(({ name: up }) => depth(up, new Set([...seen, name])))) : 0
    rank.set(name, value)
    return value
  }
  for (const name of order) depth(name, new Set())
  const ranked: string[][] = []
  for (const name of order) (ranked[rank.get(name) ?? 0] ??= []).push(name)
  // A cyclic declaration (refused by the checker) can leave an empty rank.
  const layers = ranked.filter((layer): layer is string[] => layer !== undefined)
  const width = Math.min(COLUMNS, Math.max(...layers.map((layer) => layer.length))) * (NODE_W + GAP_X) - GAP_X + PAD * 2
  const x = new Map<string, number>()
  const y = new Map<string, number>()
  let row = 0
  for (const [level, layer] of layers.entries()) {
    if (level > 0) {
      const centre = (name: string): number => {
        const ups = (parents.get(name) ?? []).map(({ name: up }) => x.get(up)).filter((value): value is number => value !== undefined)
        return ups.length ? ups.reduce((sum, value) => sum + value, 0) / ups.length : Number.POSITIVE_INFINITY
      }
      layer.sort((a, b) => centre(a) - centre(b))
    }
    for (let start = 0; start < layer.length; start += COLUMNS, row += 1) {
      const chunk = layer.slice(start, start + COLUMNS)
      const offset = (width - (chunk.length * (NODE_W + GAP_X) - GAP_X)) / 2
      chunk.forEach((name, position) => {
        x.set(name, offset + position * (NODE_W + GAP_X))
        y.set(name, PAD + row * (NODE_H + GAP_Y))
      })
    }
  }
  const height = PAD * 2 + row * (NODE_H + GAP_Y) - GAP_Y
  const edges = [...parents.entries()].flatMap(([child, ups]) => ups.map(({ name: up, via }) => {
    const x1 = (x.get(up) ?? 0) + NODE_W / 2, y1 = (y.get(up) ?? 0) + NODE_H
    const x2 = (x.get(child) ?? 0) + NODE_W / 2, y2 = y.get(child) ?? 0
    const bend = (y2 - y1) / 2
    return `<path class="edge ${via}" d="M${x1} ${y1} C${x1} ${y1 + bend} ${x2} ${y2 - bend} ${x2} ${y2}"><title>${escape(child)} ${via} ${escape(up)}</title></path>`
  })).join("")
  const nodes = order.map((name) => {
    const dot = name.lastIndexOf(".")
    const local = dot < 0 ? name : name.slice(dot + 1)
    const namespace = dot < 0 ? "" : name.slice(0, dot)
    const anchor = catalog.conceptAnchor.get(name)
    const kind = anchor ? (roles.get(name) ?? "") : "external"
    const box = `<g class="node ${kind}" transform="translate(${x.get(name)} ${y.get(name)})"><rect width="${NODE_W}" height="${NODE_H}" rx="10"/><text x="12" y="20">${escape(local.length > 22 ? `${local.slice(0, 21)}…` : local)}</text><text class="ns" x="12" y="35">${escape(namespace.length > 28 ? `${namespace.slice(0, 27)}…` : namespace)}</text><title>${escape(name)}${anchor ? "" : " (not in this document)"}</title></g>`
    return anchor ? `<a href="#${anchor}" aria-label="${escape(name)}">${box}</a>` : box
  }).join("")
  const edgeCount = [...parents.values()].reduce((sum, ups) => sum + ups.length, 0)
  return `<figure class="diagram" aria-labelledby="graph-caption">
<svg role="img" aria-labelledby="graph-caption" viewBox="0 0 ${width} ${height}" width="${width}" height="${height}">${edges}${nodes}</svg>
<figcaption id="graph-caption" class="sr-only">Concept graph: ${order.length} concepts and ${edgeCount} declared supertype edges</figcaption>
<ul class="legend"><li><i class="thing"></i>Thing</li><li><i class="happening"></i>Happening</li><li><i></i>No declared role</li><li><i class="external"></i>Not in this document</li><li><i class="line"></i>extends</li><li><i class="line dashed"></i>implements</li></ul>
${list}
</figure>`
}

function relationsSection(catalog: Index): string {
  const linkRows = catalog.concepts.flatMap((concept) => concept.links.map((link) =>
    `<tr><td>${conceptRef(concept.name, catalog)}</td><td>${code(link.id)}</td><td>${conceptRef(link.target, catalog)}</td><td>${cardinality(link.cardinality)}</td></tr>`))
  const relationRows = catalog.relations.map((relation) =>
    `<tr><td>${conceptRef(relation.payload.from, catalog)}</td><td>${escape(relation.name)}</td><td>${conceptRef(relation.payload.to, catalog)}</td><td>${relation.payload.cardinality ? escape(relation.payload.cardinality.replaceAll("_", " ")) : ""}</td></tr>`)
  const rows = [...linkRows, ...relationRows]
  return rows.length
    ? `<div class="card scroll"><table><thead><tr><th>From</th><th>Relation</th><th>To</th><th>Cardinality</th></tr></thead><tbody>${rows.join("")}</tbody></table></div>`
    : '<p class="empty">This document declares no relations.</p>'
}

function modality(modality: Schemas["NornIrLawModality"]): string {
  return modality.kind === "until" ? `until ${code(modality.predicate)}` : modality.kind
}

function response(response: Schemas["NornIrLawResponse"], catalog: Index): string {
  switch (response.kind) {
    case "predicate": return code(response.expression)
    case "invariant": return `invariant ${code(response.invariantId)} of ${conceptRef(response.objectType, catalog)}`
    case "action": return `action ${code(response.artifactId)}`
    case "transition": return `transition ${code(response.transitionId)} of ${conceptRef(response.objectType, catalog)}.${code(response.property)}`
  }
}

function timing(timing: Schemas["NornIrLawTiming"] | undefined): string {
  if (!timing) return ""
  return `<dt>Timing</dt><dd>${timing.kind === "within" ? `within ${escape(timing.millis)} ms` : `before ${code(timing.property)}`}</dd>`
}

const absent = (what: string): string => `<p class="absent">${escape(what)} not exposed by the server.</p>`

function lawCard(law: LawArtifact, position: number, catalog: Index): string {
  const payload = law.payload
  const anchor = `law-${position}`
  const states = payload.scope.lifecycleStates?.map((scope) => `${conceptRef(scope.objectType, catalog)}.${code(scope.property)} in ${scope.states.map(code).join(" ")}`).join("; ")
  return `<article class="card" id="${anchor}" aria-labelledby="${anchor}-name">
  <header><div><h3 id="${anchor}-name">${escape(law.name)}</h3><span class="sub">${code(payload.id)} · ${escape(law.version)}</span></div>
  <div class="chips"><span class="chip">${modality(payload.modality)}</span><span class="chip${payload.severity === "error" ? " error" : ""}">${payload.severity}</span></div></header>
  <dl>
    <dt>Governs</dt><dd>${conceptRef(payload.subject, catalog)}</dd>
    ${payload.condition ? `<dt>When</dt><dd>${code(payload.condition)}</dd>` : ""}
    <dt>Requires</dt><dd>${response(payload.response, catalog)}</dd>
    ${timing(payload.timing)}
    ${states ? `<dt>In states</dt><dd>${states}</dd>` : ""}
    <dt>Declared at</dt><dd>${escape(payload.scope.root.kind)} ${code(payload.scope.root.id)} · owner ${code(payload.owner)}</dd>
    <dt>Origin</dt><dd>${absent("The inherited origin chain is")}</dd>
    <dt>Source</dt><dd>${escape(payload.provenance.source.replaceAll("_", " "))}${payload.provenance.sourcePath ? ` · ${code(payload.provenance.sourcePath)}` : ""}</dd>
  </dl>
  <h4>Rationale</h4><p class="desc">${escape(payload.rationale)}</p>
  <div class="wording">
    <div lang="en" dir="ltr"><h4>English wording</h4>${absent("Reviewed English wording is")}</div>
    <div lang="ar" dir="rtl"><h4>الصياغة العربية</h4><p class="absent" lang="en" dir="ltr">Reviewed Arabic wording is not exposed by the server.</p></div>
  </div>
</article>`
}

function valueTypesSection(catalog: Index): string {
  const rows = catalog.valueTypes.map(({ anchor, def }) => `<tr id="${anchor}"><td>${code(def.name)}</td><td>${typeLabel(def.type, catalog)}</td><td class="sub">${constraintLabel(def.constraints)}${def.description ? `${def.constraints ? " · " : ""}${escape(def.description)}` : ""}</td></tr>`).join("")
  return `<div class="card scroll"><table><thead><tr><th>Value type</th><th>Representation</th><th>Notes</th></tr></thead><tbody>${rows}</tbody></table></div>`
}

function otherSection(catalog: Index): string {
  const rows = catalog.other.map((artifact) => `<tr><td>${escape(artifact.name)}</td><td>${code(artifact.artifactId)}</td><td>${escape(artifact.kind.replaceAll("_", " "))}</td><td>${escape(artifact.version)}</td></tr>`).join("")
  return `<div class="card scroll"><table><thead><tr><th>Name</th><th>Identity</th><th>Kind</th><th>Version</th></tr></thead><tbody>${rows}</tbody></table></div>`
}

const plural = (count: number, one: string, many: string): string => `${count} ${count === 1 ? one : many}`

/**
 * Catalog browser over an authorized definitions document. It shows only what
 * the artifacts declare, in server order; authorization, admission and law
 * inheritance stay with the server.
 */
export function renderCatalogDocument({ artifacts, title = "Catalog" }: {
  readonly artifacts: readonly Artifact[]
  readonly title?: string
}): string {
  const catalog = index(artifacts)
  const relationCount = catalog.concepts.reduce((sum, concept) => sum + concept.links.length, 0) + catalog.relations.length
  const sections = [
    { id: "concepts", label: "Concepts", show: catalog.concepts.length > 0 },
    { id: "relations", label: "Relations", show: relationCount > 0 },
    { id: "laws", label: "Laws", show: catalog.laws.length > 0 },
    { id: "value-types", label: "Value types", show: catalog.valueTypes.length > 0 },
    { id: "other", label: "Other definitions", show: catalog.other.length > 0 },
  ].filter(({ show }) => show)
  const stats = `<ul class="stats">
  <li><b>${catalog.concepts.length}</b><span>${catalog.concepts.length === 1 ? "concept" : "concepts"}</span></li>
  <li><b>${relationCount}</b><span>${relationCount === 1 ? "relation" : "relations"}</span></li>
  <li><b>${catalog.laws.length}</b><span>${catalog.laws.length === 1 ? "law" : "laws"}</span></li>
  <li><b>${catalog.valueTypes.length}</b><span>${catalog.valueTypes.length === 1 ? "value type" : "value types"}</span></li>
  <li><b>${catalog.other.length}</b><span>other</span></li>
</ul>`
  const body = artifacts.length ? `${stats}
${catalog.concepts.length ? `<h2 id="concepts">Concepts</h2><p class="lede">${plural(catalog.concepts.length, "concept", "concepts")} with the supertypes each one declares. Select a concept to open it.</p>${conceptGraph(catalog)}<div style="height:1rem"></div>${catalog.concepts.map((concept) => conceptCard(concept, catalog)).join("")}` : ""}
${relationCount ? `<h2 id="relations">Relations</h2><p class="lede">Declared links and relations between concepts.</p>${relationsSection(catalog)}` : ""}
${catalog.laws.length ? `<h2 id="laws">Laws</h2><p class="lede">Each law, the concept it governs and where it was declared.</p>${catalog.laws.map((law, position) => lawCard(law, position, catalog)).join("")}` : ""}
${catalog.valueTypes.length ? `<h2 id="value-types">Value types</h2>${valueTypesSection(catalog)}` : ""}
${catalog.other.length ? `<h2 id="other">Other definitions</h2>${otherSection(catalog)}` : ""}`
    : '<p class="empty">This document contains no definitions.</p>'
  return renderShell({
    title,
    eyebrow: "What built this application",
    intro: "The concepts, relations and laws in this server document, exactly as declared.",
    observation: UNLABELLED_READ,
    sections,
    body,
  })
}
