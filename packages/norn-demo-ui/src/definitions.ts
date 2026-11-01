import type { Schemas } from "../../norn-sdk/src/index.ts"

type Artifact = Schemas["NornArtifactEnvelope"]
type Definition = { [K in Artifact["kind"]]: Pick<Extract<Artifact, { kind: K }>, "kind" | "payload" | "dependencies"> }[Artifact["kind"]]
type Metadata = Pick<Artifact, "artifactId" | "kind" | "name" | "version">

/** Metadata and exact JSON text from an already-authorized definition read. */
export interface DefinitionEntry {
  readonly metadata: Metadata
  readonly source: string
  readonly definition?: Definition
}

const escape = (value: string): string => value.replace(/[&<>"'\r]/g, (character) => {
  switch (character) {
    case "\r": return "&#13;"
    case "&": return "&amp;"
    case "<": return "&lt;"
    case ">": return "&gt;"
    case '"': return "&quot;"
    case "'": return "&#39;"
    default: return character
  }
})

const styles = `
:root { color-scheme: light; font-family: "Segoe UI", sans-serif; color: #202c29; background: #f4f5f1; }
* { box-sizing: border-box; }
body { margin: 0; }
a { color: inherit; text-underline-offset: .2em; }
a:focus-visible, summary:focus-visible { outline: 3px solid #23664c; outline-offset: 5px; }
.skip { position: absolute; left: 1rem; top: -5rem; }
.skip:focus { top: 1rem; background: white; padding: 1rem; }
header { border-bottom: 1px solid #d5dcd4; padding: 1.25rem max(1.5rem, calc((100vw - 1120px)/2)); background: #fff; }
.brand { font-weight: 650; letter-spacing: .06em; }
main { max-width: 1120px; margin: auto; padding: 3rem 1.5rem 5rem; }
h1 { font-family: Georgia, serif; font-weight: 400; font-size: clamp(2rem, 5vw, 3rem); margin: .4rem 0 1rem; }
.eyebrow, .meta { color: #52635b; font-size: .875rem; }
.eyebrow { text-transform: uppercase; letter-spacing: .12em; }
.intro { max-width: 65ch; line-height: 1.65; margin-bottom: 2.5rem; }
.count { font-variant-numeric: tabular-nums; margin-bottom: 1rem; }
article { background: #fff; border: 1px solid #d5dcd4; border-radius: 4px; margin-bottom: .75rem; }
summary { cursor: pointer; padding: 1.25rem; line-height: 1.6; overflow-wrap: anywhere; }
.name { font-weight: 600; }
.meta { display: block; margin-left: 1rem; }
code, pre { font-family: "Cascadia Code", "SFMono-Regular", Consolas, monospace; font-size: .85rem; }
pre { margin: 0; padding: 1.25rem; border-top: 1px solid #e0e5de; background: #fafbf8; overflow: auto; max-height: 65vh; line-height: 1.6; tab-size: 2; }
h2 { font-size: 1.2rem; font-weight: 500; margin-top: 2rem; }
small { color: #52635b; margin-left: .5rem; }
h3 { font-size: .9rem; margin-top: 1.5rem; }
.structure { padding: 0 1.25rem 1.25rem; line-height: 1.6; overflow-wrap: anywhere; }
table { border-collapse: collapse; width: 100%; font-size: .9rem; }
th, td { text-align: left; padding: .5rem; border-bottom: 1px solid #e0e5de; }
th { color: #52635b; font-weight: 500; }
.states, .transitions { display: flex; flex-wrap: wrap; gap: .6rem; padding: 0; list-style: none; }
.states li, .transitions li { padding: .5rem .75rem; border: 1px solid #d5dcd4; border-radius: 4px; }
.states .initial { border-left: 3px solid #23664c; }
.states .terminal { border-style: double; border-width: 3px; }
.transitions li { display: flex; gap: .75rem; align-items: center; background: #fafbf8; }
.edge { color: #52635b; text-align: center; font-size: .8rem; }
.source { border-top: 1px solid #e0e5de; }
.empty { padding: 2rem; border: 1px dashed #a4b1a7; line-height: 1.6; }
@media (max-width: 600px) { main { padding-top: 2rem; } summary { padding: 1rem; } }
@media print { body { background: white; } pre { max-height: none; white-space: pre-wrap; overflow-wrap: anywhere; } }
`

/**
 * A read-only inspector. Authorization and admission happen before calling it.
 * Exact source is never parsed or rewritten; structured data is the admitted server payload.
 */
export function renderDefinitionsDocument({ entries, title = "Definitions" }: {
  readonly entries: readonly DefinitionEntry[]
  readonly title?: string
}): string {
  const groups = [
    ["Entities", ["entity", "relation"]], ["Actions", ["action"]],
    ["Rules", ["rule", "derived", "law"]], ["Policies", ["policy"]],
    ["Other definitions", []],
  ] satisfies [string, string[]][]
  const namedKinds = groups.flatMap(([, kinds]) => kinds)
  const cards = groups.map(([label, kinds]) => {
    const members = entries.filter(({ metadata }) => kinds.length ? kinds.includes(metadata.kind) : !namedKinds.includes(metadata.kind))
    if (!members.length) return ""
    return `<section><h2>${label} <small>${members.length}</small></h2>${members.map((entry) => {
      const index = entries.indexOf(entry)
      return `<article aria-labelledby="definition-${index}">
        <details><summary id="definition-${index}"><span class="name">${escape(entry.metadata.name)}</span>
          <span class="meta"><code>${escape(entry.metadata.artifactId)}</code> · ${escape(entry.metadata.kind)} · ${escape(entry.metadata.version)}</span></summary>
          <div class="structure">${describe(entry, entries)}</div>
          <details class="source"><summary>Source</summary><pre tabindex="0" aria-label="Definition source"><code>${escape(entry.source)}</code></pre></details>
        </details></article>`
    }).join("")}</section>`
  }).join("")
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'">
  <title>${escape(title)} · Norn</title>
  <style>${styles}</style>
</head>
<body>
  <a class="skip" href="#content">Skip to definitions</a>
  <header><span class="brand">NORN</span></header>
  <main id="content">
    <div class="eyebrow">What built this application</div>
    <h1>${escape(title)}</h1>
    <p class="intro">Entities, relationships, actions, lifecycle and rules, as returned by the server. Open a definition to see the structure and constraints that drive the application.</p>
    <p class="count">${entries.length} ${entries.length === 1 ? "definition" : "definitions"}</p>
    ${entries.length ? cards : '<p class="empty">This document contains no definitions.</p>'}
  </main>
</body>
</html>\n`
}

const list = (title: string, items: readonly string[]): string => items.length
  ? `<h3>${escape(title)}</h3><ul>${items.map((item) => `<li>${item}</li>`).join("")}</ul>` : ""
const text = (value: string): string => `<code>${escape(value)}</code>`

function typeLabel(type: Schemas["NornIrPropertyType"]): string {
  switch (type.kind) {
    case "decimal": return `decimal(${type.scale})${type.unit ? ` · ${type.unit.system}:${type.unit.code}` : ""}`
    case "ref": return type.name
    case "enum": return type.values.map((value) => value.value).join(" | ")
    default: return type.kind
  }
}

function tree(value: unknown): string {
  if (value === null) return text("null")
  if (Array.isArray(value)) return `<ul>${value.map((item: unknown) => `<li>${tree(item)}</li>`).join("")}</ul>`
  if (typeof value === "object") return `<dl>${Object.entries(value).map(([key, child]) => `<dt>${escape(key)}</dt><dd>${tree(child)}</dd>`).join("")}</dl>`
  return text(String(value))
}

function transitionActions(entries: readonly DefinitionEntry[], entity: string, property: string, transition: string): string[] {
  return entries.flatMap(({ metadata, definition }) => {
    if (definition?.kind !== "action") return []
    const effect = definition.payload.effectProgram
    return effect?.kind === "fire_transition" && effect.entityArtifactId === entity && effect.lifecycle === property && effect.transition === transition
      ? [metadata.name] : []
  })
}

function lifecycleView(object: Schemas["NornIrObjectTypeDef"], entity: string, entries: readonly DefinitionEntry[]): string {
  return (object.lifecycles ?? []).map((cycle) => {
    const property = object.properties?.find((item) => item.id === cycle.property)
    const states = property?.type.kind === "enum" ? property.type.values.map((value) => value.value) : []
    return `<h3>Lifecycle · ${escape(cycle.property)}</h3>
      <ul class="states" aria-label="Lifecycle states">${states.map((state) => {
        const initial = cycle.initial.includes(state)
        const terminal = cycle.terminal?.includes(state)
        return `<li class="${initial ? "initial" : ""} ${terminal ? "terminal" : ""}">${escape(state)}${initial ? " · initial" : ""}${terminal ? " · terminal" : ""}${cycle.immutable?.includes(state) ? " · immutable" : ""}</li>`
      }).join("")}</ul>
      <ul class="transitions" aria-label="Lifecycle transitions">${(cycle.transitions ?? []).flatMap((transition) => transition.from.map((from) => {
        const actions = transitionActions(entries, entity, cycle.property, transition.id)
        return `<li><span>${escape(from)}</span><span class="edge">${escape(actions.join(", ") || "No action supplied")}<br>— ${escape(transition.id)} →</span><span>${escape(transition.to)}</span>${list("Guards", (transition.guards ?? []).map(text))}</li>`
      })).join("")}</ul>`
  }).join("")
}

function describe({ metadata, definition }: DefinitionEntry, entries: readonly DefinitionEntry[]): string {
  if (!definition) return '<p class="meta">Structured definition not supplied. Exact source is available below.</p>'
  const dependencies = list("Referenced definitions", definition.dependencies.map((item) => text(`${item.artifactId} · ${item.kind}${item.version ? ` · ${item.version}` : ""}`)))
  switch (definition.kind) {
    case "entity": {
      const payload = definition.payload
      if (!("objectType" in payload)) return tree(payload) + dependencies
      const object = payload.objectType
      return `${object.description ? `<p>${escape(object.description)}</p>` : ""}
        ${object.extends ? `<p>Extends ${text(object.extends)}</p>` : ""}
        <h3>Properties</h3><table><thead><tr><th>Name</th><th>Type / unit</th><th>Derivation</th></tr></thead><tbody>${(object.properties ?? []).map((property) => `<tr><td>${escape(property.id)}</td><td>${escape(typeLabel(property.type))}</td><td>${property.computed ? text(property.computed) : ""}</td></tr>`).join("")}</tbody></table>
        ${list("Relationships", (object.links ?? []).map((link) => `${text(link.id)} → ${text(link.target)} · ${escape(link.cardinality.from)}:${escape(link.cardinality.to)}${link.cardinality.required ? " · required" : ""}`))}
        ${lifecycleView(object, metadata.artifactId, entries)}
        ${list("Protects", (object.invariants ?? []).map((law) => `${text(law.id)} · ${escape(law.message)} — ${text(law.expression)}`))}${dependencies}`
    }
    case "action": {
      const action = definition.payload
      const effect = action.effectProgram
      const transitions = effect?.kind === "fire_transition" ? entries.flatMap(({ metadata: target, definition: entity }) => {
        if (entity?.kind !== "entity" || !("objectType" in entity.payload) || target.artifactId !== effect.entityArtifactId || target.version !== effect.entityArtifactVersion) return []
        return (entity.payload.objectType.lifecycles ?? []).filter((cycle) => cycle.property === effect.lifecycle).flatMap((cycle) =>
          (cycle.transitions ?? []).filter((transition) => transition.id === effect.transition).map((transition) => `${transition.from.join(", ")} → ${transition.to}`))
      }) : []
      return `${action.description ? `<p>${escape(action.description)}</p>` : ""}
        ${effect?.kind === "fire_transition" ? `<h3>Performs transition</h3><p>${text(effect.objectKind)} · ${text(effect.lifecycle)} · ${text(effect.transition)}</p>${list("State change", transitions.map(text))}` : ""}
        <h3>Inputs</h3>${tree(action.inputSchema)}
        ${list("Must satisfy", (action.preconditions ?? []).map(text))}
        ${action.access ? `<h3>Access policy</h3><p>${text(action.access.artifactId)}</p>` : ""}${dependencies}`
    }
    case "rule": {
      const rule = definition.payload
      return `${rule.appliesTo ? `<h3>Applies to</h3><p>${text(rule.appliesTo)}</p>` : ""}
        ${rule.expression ? `<h3>Derives / checks</h3><p>${text(rule.expression)}</p>` : ""}
        ${rule.triggers?.length ? `<h3>Runs on</h3>${tree(rule.triggers)}` : ""}
        ${rule.outputSchema ? `<h3>Declared outputs</h3>${tree(rule.outputSchema)}` : ""}
        ${rule.decision || rule.graph ? `<h3>Decision</h3>${tree(rule.decision ?? rule.graph)}` : ""}${dependencies}`
    }
    case "law": return `<p>${escape(definition.payload.rationale)}</p><h3>Protects</h3>${tree(definition.payload.response)}${dependencies}`
    case "derived": return `<h3>Derives</h3>${tree(definition.payload.calc)}<h3>Outputs</h3>${tree(definition.payload.outputSchema)}${dependencies}`
    default: return tree(definition.payload) + dependencies
  }
}
