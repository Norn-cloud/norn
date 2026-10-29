/**
 * `norn plan` (T7-TS-4, #2692): what installing a checked definition set
 * would change in a cell. A pure diff of two artifact sets, the local one
 * (compiled and admitted) against the cell's (`GET /v1/defs`) or a directory:
 * added, removed, and changed artifacts, plus lifecycle warnings. A
 * narrowing (a removed state, transition, or source state, a new guard, a new
 * terminal state, a lifecycle added to an existing type) can strand live
 * objects or refuse in-flight work; a widening (the reverse) admits behaviour
 * the cell did not have.
 *
 * `provenance` names where an artifact was authored; a difference only there
 * is reported, but it changes no behaviour.
 */
import { type Json, canonicalJson } from "./canonical.ts"
import { artifactId, isArtifact } from "./pack.ts"

export interface ArtifactChange {
  readonly artifactId: string
  readonly kind: string
  /** Top-level envelope and payload members that differ (`version`, `payload.objectType`, …). */
  readonly members: ReadonlyArray<string>
  readonly fromVersion?: string
  readonly toVersion?: string
}

export interface LifecycleWarning {
  readonly artifactId: string
  readonly objectType: string
  readonly property: string
  readonly kind: "narrowing" | "widening"
  readonly category: string
  readonly ids: ReadonlyArray<string>
  readonly detail: string
}

export interface PlanNarrowing {
  readonly kind: string
  readonly artifactId: string
  readonly objectType?: string
  readonly property?: string
  readonly ids: ReadonlyArray<string>
  readonly detail: string
}

export interface Plan {
  readonly added: ReadonlyArray<{ readonly artifactId: string; readonly kind: string }>
  readonly removed: ReadonlyArray<{ readonly artifactId: string; readonly kind: string }>
  readonly changed: ReadonlyArray<ArtifactChange>
  /** Identical except for `provenance`. */
  readonly provenanceOnly: ReadonlyArray<string>
  readonly unchanged: ReadonlyArray<string>
  readonly warnings: ReadonlyArray<LifecycleWarning>
  /** Structural removals and lifecycle narrowing, ready for install gating. */
  readonly narrowing: ReadonlyArray<PlanNarrowing>
}

type Obj = Readonly<Record<string, unknown>>
const obj = (v: unknown): Obj | undefined => (typeof v === "object" && v !== null && !Array.isArray(v) ? (v as Obj) : undefined)
const strings = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [])
const same = (a: unknown, b: unknown): boolean => canonicalJson(a ?? null) === canonicalJson(b ?? null)

function index(artifacts: ReadonlyArray<Json>): Map<string, Obj> {
  const out = new Map<string, Obj>()
  for (const a of artifacts) if (isArtifact(a)) out.set(artifactId(a), a as unknown as Obj)
  return out
}

function differingMembers(a: Obj, b: Obj): string[] {
  const out: string[] = []
  for (const key of [...new Set([...Object.keys(a), ...Object.keys(b)])].sort()) {
    if (key === "provenance" || key === "payload") continue
    if (!same(a[key], b[key])) out.push(key)
  }
  const pa = obj(a.payload) ?? {}, pb = obj(b.payload) ?? {}
  for (const key of [...new Set([...Object.keys(pa), ...Object.keys(pb)])].sort()) if (!same(pa[key], pb[key])) out.push(`payload.${key}`)
  return out
}

/** Diff the local (to install) set against the remote (installed) set. */
export function planDiff(local: ReadonlyArray<Json>, remote: ReadonlyArray<Json>): Plan {
  const next = index(local), prev = index(remote)
  const kind = (a: Obj): string => (typeof a.kind === "string" ? a.kind : "?")
  const added = [...next].filter(([id]) => !prev.has(id)).map(([id, a]) => ({ artifactId: id, kind: kind(a) }))
  const removed = [...prev].filter(([id]) => !next.has(id)).map(([id, a]) => ({ artifactId: id, kind: kind(a) }))
  const changed: ArtifactChange[] = [], provenanceOnly: string[] = [], unchanged: string[] = []
  const warnings: LifecycleWarning[] = []
  for (const [id, a] of next) {
    const b = prev.get(id)
    if (!b) {
      warnings.push(...lifecycleWarnings(id, undefined, a))
      continue
    }
    if (same(a, b)) { unchanged.push(id); continue }
    const members = differingMembers(b, a)
    if (members.length === 0) { provenanceOnly.push(id); continue }
    changed.push({
      artifactId: id,
      kind: kind(a),
      members,
      ...(typeof b.version === "string" ? { fromVersion: b.version } : {}),
      ...(typeof a.version === "string" ? { toVersion: a.version } : {}),
    })
    warnings.push(...lifecycleWarnings(id, b, a))
  }
  for (const [id, b] of prev) if (!next.has(id)) warnings.push(...lifecycleWarnings(id, b, undefined))
  const byId = (x: { artifactId: string }, y: { artifactId: string }) => x.artifactId.localeCompare(y.artifactId)
  const narrowing = structuralNarrowings(remote, local)
  narrowing.sort((left, right) =>
    `${left.artifactId}:${left.objectType ?? ""}:${left.property ?? ""}:${left.kind}:${left.ids.join(",")}`
      .localeCompare(`${right.artifactId}:${right.objectType ?? ""}:${right.property ?? ""}:${right.kind}:${right.ids.join(",")}`),
  )
  return { added: added.sort(byId), removed: removed.sort(byId), changed: changed.sort(byId), provenanceOnly: provenanceOnly.sort(), unchanged: unchanged.sort(), warnings, narrowing }
}

interface TypeSnapshot {
  readonly artifactId: string
  readonly extends?: string
  readonly properties: ReadonlyMap<string, Obj>
  readonly links: ReadonlyMap<string, Obj>
  readonly lifecycles: ReadonlyMap<string, Obj>
}

interface ExtensionSnapshot {
  readonly properties: ReadonlyMap<string, Obj>
  readonly links: ReadonlyMap<string, Obj>
}

interface EntityModelSnapshot {
  readonly types: ReadonlyMap<string, TypeSnapshot>
  readonly extensions: ReadonlyMap<string, ReadonlyArray<ExtensionSnapshot>>
}

function named(value: unknown, key: string): Map<string, Obj> {
  const out = new Map<string, Obj>()
  if (!Array.isArray(value)) return out
  for (const member of value) {
    const entry = obj(member)
    if (entry && typeof entry[key] === "string") out.set(entry[key] as string, entry)
  }
  return out
}

function entityModel(artifacts: ReadonlyArray<Json>): EntityModelSnapshot {
  const types = new Map<string, TypeSnapshot>()
  const extensions = new Map<string, ExtensionSnapshot[]>()
  for (const artifact of artifacts) {
    if (!isArtifact(artifact) || artifact.kind !== "entity") continue
    const payload = obj((artifact as unknown as Obj).payload)
    const objectType = obj(payload?.objectType)
    if (objectType && typeof objectType.name === "string") {
      types.set(objectType.name, {
        artifactId: artifactId(artifact),
        ...(typeof objectType.extends === "string" ? { extends: objectType.extends } : {}),
        properties: named(objectType.properties, "id"),
        links: named(objectType.links, "id"),
        lifecycles: named(objectType.lifecycles, "property"),
      })
      continue
    }
    if (payload && typeof payload.objectKind === "string") {
      const fields = obj(payload.fields) ?? {}
      types.set(payload.objectKind, {
        artifactId: artifactId(artifact),
        properties: new Map(Object.entries(fields).map(([key, value]) => [key, obj(value) ?? {}])),
        links: new Map(),
        lifecycles: new Map(),
      })
      continue
    }
    const extension = obj(payload?.objectTypeExtension)
    if (!extension || typeof extension.target !== "string") continue
    const list = extensions.get(extension.target) ?? []
    list.push({ properties: named(extension.properties, "id"), links: named(extension.links, "id") })
    extensions.set(extension.target, list)
  }
  return { types, extensions }
}

function effectiveMembers(
  model: EntityModelSnapshot,
  typeName: string,
  member: "properties" | "links",
  seen: Set<string> = new Set(),
): Map<string, Obj> {
  if (seen.has(typeName)) return new Map()
  seen.add(typeName)
  const type = model.types.get(typeName)
  if (!type) { seen.delete(typeName); return new Map() }
  const out = type.extends ? effectiveMembers(model, type.extends, member, seen) : new Map<string, Obj>()
  for (const [id, value] of type[member]) out.set(id, value)
  for (const extension of model.extensions.get(typeName) ?? []) {
    for (const [id, value] of extension[member]) out.set(id, value)
  }
  seen.delete(typeName)
  return out
}

function structuralNarrowings(previous: ReadonlyArray<Json>, candidate: ReadonlyArray<Json>): PlanNarrowing[] {
  const old = index(previous), next = index(candidate), out: PlanNarrowing[] = []
  const add = (kind: string, artifactId: string, ids: ReadonlyArray<string>, detail: string, objectType?: string, property?: string) => {
    out.push({ kind, artifactId, ids, detail, ...(objectType ? { objectType } : {}), ...(property ? { property } : {}) })
  }
  const artifactPayload = (artifact: Obj): Obj => obj(artifact.payload) ?? {}
  const mappingIds = (payload: Obj): string[] => Array.isArray(payload.maps)
    ? payload.maps.map(obj).filter((map): map is Obj => Boolean(map && typeof map.id === "string")).map((map) => map.id as string)
    : []

  for (const [id, artifact] of old) {
    if (next.has(id)) continue
    const kind = typeof artifact.kind === "string" ? artifact.kind : "?"
    add("artifact_removed", id, [id], `${kind} artifact removed`)
    const payload = artifactPayload(artifact)
    if (kind === "action") {
      const actionId = typeof payload.actionId === "string" ? payload.actionId : id
      add("action_removed", id, [actionId], `action ${JSON.stringify(actionId)} removed`)
    } else if (kind === "mapping") {
      const ids = mappingIds(payload)
      add("mapping_removed", id, ids, "mapping artifact removed")
    } else if (kind === "relation") {
      add("link_removed", id, [id], "RelationIR link artifact removed", typeof payload.from === "string" ? payload.from : undefined)
    }
  }

  for (const [id, before] of old) {
    const after = next.get(id)
    if (!after) continue
    const oldKind = typeof before.kind === "string" ? before.kind : "?"
    const newKind = typeof after.kind === "string" ? after.kind : "?"
    const oldPayload = artifactPayload(before), newPayload = artifactPayload(after)
    if (oldKind === "action" && (newKind !== "action" || oldPayload.actionId !== newPayload.actionId)) {
      const actionId = typeof oldPayload.actionId === "string" ? oldPayload.actionId : id
      if (newKind !== "action") add("artifact_removed", id, [id], `${oldKind} artifact removed`)
      add("action_removed", id, [actionId], `action ${JSON.stringify(actionId)} replaced or removed`)
    }
    if (oldKind === "mapping") {
      if (newKind !== "mapping") add("artifact_removed", id, [id], `${oldKind} artifact removed`)
      const newMaps = new Set(mappingIds(newPayload))
      for (const mapId of mappingIds(oldPayload)) {
        if (!newMaps.has(mapId)) add("mapping_removed", id, [mapId], `mapping object map ${JSON.stringify(mapId)} removed`)
      }
    }
  }

  const oldModel = entityModel(previous), nextModel = entityModel(candidate)
  for (const [name, before] of oldModel.types) {
    const after = nextModel.types.get(name)
    if (!after) {
      add("object_type_removed", before.artifactId, [name], `object type ${JSON.stringify(name)} removed`, name)
      continue
    }
    const oldProperties = effectiveMembers(oldModel, name, "properties")
    const newProperties = effectiveMembers(nextModel, name, "properties")
    for (const id of oldProperties.keys()) {
      if (!newProperties.has(id)) add("property_removed", after.artifactId, [id], `property ${name}.${id} removed`, name, id)
    }
    const oldLinks = effectiveMembers(oldModel, name, "links")
    const newLinks = effectiveMembers(nextModel, name, "links")
    for (const [id, oldLink] of oldLinks) {
      const newLink = newLinks.get(id)
      if (!newLink) {
        add("link_removed", after.artifactId, [id], `link ${name}.${id} removed`, name)
        continue
      }
      const oldLinkProperties = named(oldLink.properties, "id")
      const newLinkProperties = named(newLink.properties, "id")
      for (const property of oldLinkProperties.keys()) {
        if (!newLinkProperties.has(property)) {
          add("link_property_removed", after.artifactId, [id], `link property ${name}.${id}.${property} removed`, name, property)
        }
      }
    }
  }
  out.push(...lifecycleNarrowings(oldModel, nextModel))
  return out
}

function effectiveLifecycleSpecs(model: EntityModelSnapshot, typeName: string, seen = new Set<string>()): Map<string, Obj> {
  if (seen.has(typeName)) return new Map()
  seen.add(typeName)
  const type = model.types.get(typeName)
  if (!type) { seen.delete(typeName); return new Map() }
  const out = type.extends ? effectiveLifecycleSpecs(model, type.extends, seen) : new Map<string, Obj>()
  for (const [property, lifecycle] of type.lifecycles) out.set(property, lifecycle)
  seen.delete(typeName)
  return out
}

function effectiveLifecycleViews(model: EntityModelSnapshot, typeName: string): Map<string, LifecycleView> {
  const properties = effectiveMembers(model, typeName, "properties")
  const views = new Map<string, LifecycleView>()
  for (const [property, lifecycle] of effectiveLifecycleSpecs(model, typeName)) {
    const propertyType = obj(properties.get(property)?.type)
    const states = Array.isArray(propertyType?.values)
      ? propertyType.values.map((value) => obj(value)?.value).filter((value): value is string => typeof value === "string")
      : []
    const stateSet = new Set(states)
    const transitions = new Map<string, { from: string[]; to: string; guards: string[] }>()
    for (const transition of Array.isArray(lifecycle.transitions) ? lifecycle.transitions.map(obj) : []) {
      if (!transition || typeof transition.id !== "string") continue
      const from = strings(transition.from).filter((state) => stateSet.has(state))
      const to = typeof transition.to === "string" ? transition.to : ""
      if (stateSet.has(to) && from.length > 0) transitions.set(transition.id, { from, to, guards: strings(transition.guards) })
    }
    views.set(property, {
      states,
      initial: strings(lifecycle.initial).filter((state) => stateSet.has(state)),
      terminal: strings(lifecycle.terminal).filter((state) => stateSet.has(state)),
      transitions,
    })
  }
  return views
}

function lifecycleNarrowings(previous: EntityModelSnapshot, candidate: EntityModelSnapshot): PlanNarrowing[] {
  const out: PlanNarrowing[] = []
  const add = (kind: string, artifactId: string, objectType: string, property: string, ids: ReadonlyArray<string>, detail: string) => {
    out.push({ kind, artifactId, objectType, property, ids, detail })
  }
  for (const objectType of previous.types.keys()) {
    const afterType = candidate.types.get(objectType)
    if (!afterType) continue
    const oldLifecycles = effectiveLifecycleViews(previous, objectType)
    const newLifecycles = effectiveLifecycleViews(candidate, objectType)
    for (const [property, next] of newLifecycles) {
      const old = oldLifecycles.get(property)
      if (!old) {
        if (effectiveMembers(previous, objectType, "properties").has(property)) {
          add("lifecycle_added", afterType.artifactId, objectType, property, [property], `lifecycle added to existing type ${JSON.stringify(objectType)}.${property}`)
        }
        continue
      }
      const statesRemoved = minus(old.states, next.states)
      if (statesRemoved.length > 0) add("lifecycle_state_removed", afterType.artifactId, objectType, property, statesRemoved, `lifecycle states removed from ${objectType}.${property}`)
      const initialRemoved = minus(old.initial, next.initial)
      if (initialRemoved.length > 0) add("lifecycle_initial_removed", afterType.artifactId, objectType, property, initialRemoved, `initial states removed from ${objectType}.${property}`)
      const terminalAdded = minus(next.terminal, old.terminal)
      if (terminalAdded.length > 0) add("lifecycle_terminal_added", afterType.artifactId, objectType, property, terminalAdded, `terminal states added to ${objectType}.${property}`)
      for (const [transitionId, oldTransition] of old.transitions) {
        const newTransition = next.transitions.get(transitionId)
        if (!newTransition) {
          add("lifecycle_transition_removed", afterType.artifactId, objectType, property, [transitionId], `transition ${JSON.stringify(transitionId)} removed from ${objectType}.${property}`)
          continue
        }
        const sourceStatesRemoved = minus(oldTransition.from, newTransition.from)
        if (sourceStatesRemoved.length > 0) add("lifecycle_source_state_removed", afterType.artifactId, objectType, property, sourceStatesRemoved, `transition ${JSON.stringify(transitionId)} loses source states`)
        if (oldTransition.to !== newTransition.to) {
          add("lifecycle_transition_target_changed", afterType.artifactId, objectType, property, [transitionId], `transition ${JSON.stringify(transitionId)} changes target from ${JSON.stringify(oldTransition.to)} to ${JSON.stringify(newTransition.to)}`)
        }
        const guardsAdded = minus(newTransition.guards, oldTransition.guards)
        if (guardsAdded.length > 0) add("lifecycle_guard_added", afterType.artifactId, objectType, property, [transitionId], `transition ${JSON.stringify(transitionId)} adds ${guardsAdded.length} guard(s)`)
      }
    }
  }
  return out
}

interface LifecycleView {
  readonly states: ReadonlyArray<string>
  readonly initial: ReadonlyArray<string>
  readonly terminal: ReadonlyArray<string>
  readonly transitions: ReadonlyMap<string, { readonly from: ReadonlyArray<string>; readonly to: string; readonly guards: ReadonlyArray<string> }>
}

/** The declared lifecycles of an entity artifact's object type, by property, with the property's enum values as states. */
function lifecycles(artifact: Obj | undefined): { readonly name: string; readonly byProperty: ReadonlyMap<string, LifecycleView> } | undefined {
  const type = obj(obj(artifact?.payload)?.objectType)
  if (!type || typeof type.name !== "string") return undefined
  const properties = Array.isArray(type.properties) ? type.properties.map(obj) : []
  const byProperty = new Map<string, LifecycleView>()
  for (const l of Array.isArray(type.lifecycles) ? type.lifecycles.map(obj) : []) {
    if (!l || typeof l.property !== "string") continue
    const prop = properties.find((p) => p?.id === l.property)
    const values = obj(prop?.type)?.values
    const transitions = new Map<string, { from: string[]; to: string; guards: string[] }>()
    for (const t of Array.isArray(l.transitions) ? l.transitions.map(obj) : []) {
      if (t && typeof t.id === "string") transitions.set(t.id, { from: strings(t.from), to: typeof t.to === "string" ? t.to : "", guards: strings(t.guards) })
    }
    byProperty.set(l.property, {
      states: Array.isArray(values) ? values.map((v) => obj(v)?.value).filter((v): v is string => typeof v === "string") : [],
      initial: strings(l.initial),
      terminal: strings(l.terminal),
      transitions,
    })
  }
  return { name: type.name, byProperty }
}

const minus = (a: ReadonlyArray<string>, b: ReadonlyArray<string>): string[] => a.filter((x) => !b.includes(x))
const list = (xs: ReadonlyArray<string>): string => xs.map((x) => JSON.stringify(x)).join(", ")

/** Narrowing and widening of every declared lifecycle between two versions of one entity artifact. */
export function lifecycleWarnings(id: string, before: Obj | undefined, after: Obj | undefined): LifecycleWarning[] {
  const was = lifecycles(before), is = lifecycles(after)
  const objectType = is?.name ?? was?.name
  if (!objectType) return []
  const out: LifecycleWarning[] = []
  const warn = (property: string, kind: "narrowing" | "widening", category: string, ids: ReadonlyArray<string>, detail: string) =>
    out.push({ artifactId: id, objectType, property, kind, category, ids, detail })
  const properties = [...new Set([...(was?.byProperty.keys() ?? []), ...(is?.byProperty.keys() ?? [])])].sort()
  for (const property of properties) {
    const a = was?.byProperty.get(property), b = is?.byProperty.get(property)
    // A new lifecycle on an existing type restricts the property to declared transitions and creation to initial states.
    if (!a && b) { if (was) warn(property, "narrowing", "lifecycle_added", [property], "lifecycle added: writes must now follow declared transitions from declared initial states"); continue }
    if (a && !b) {
      if (is) warn(property, "widening", "lifecycle_removed", [property], "lifecycle removed: the property is no longer restricted to declared transitions")
      else warn(property, "narrowing", "object_type_removed", [objectType], "type removed: its objects lose their lifecycle")
      continue
    }
    if (!a || !b) continue
    const pair = (label: string, removed: string[], added: string[], removedKind: "narrowing" | "widening", removedCategory: string, addedCategory: string) => {
      if (removed.length > 0) warn(property, removedKind, removedCategory, removed, `${label} removed: ${list(removed)}`)
      if (added.length > 0) warn(property, removedKind === "narrowing" ? "widening" : "narrowing", addedCategory, added, `${label} added: ${list(added)}`)
    }
    pair("states", minus(a.states, b.states), minus(b.states, a.states), "narrowing", "lifecycle_state_removed", "lifecycle_state_added")
    pair("initial states", minus(a.initial, b.initial), minus(b.initial, a.initial), "narrowing", "lifecycle_initial_removed", "lifecycle_initial_added")
    pair("terminal states", minus(a.terminal, b.terminal), minus(b.terminal, a.terminal), "widening", "lifecycle_terminal_removed", "lifecycle_terminal_added")
    pair("transitions", minus([...a.transitions.keys()], [...b.transitions.keys()]), minus([...b.transitions.keys()], [...a.transitions.keys()]), "narrowing", "lifecycle_transition_removed", "lifecycle_transition_added")
    for (const [tid, ta] of a.transitions) {
      const tb = b.transitions.get(tid)
      if (!tb) continue
      pair(`transition ${JSON.stringify(tid)} source states`, minus(ta.from, tb.from), minus(tb.from, ta.from), "narrowing", "lifecycle_source_state_removed", "lifecycle_source_state_added")
      if (ta.to !== tb.to) warn(property, "narrowing", "lifecycle_transition_target_changed", [tid], `transition ${JSON.stringify(tid)} now targets ${JSON.stringify(tb.to)} instead of ${JSON.stringify(ta.to)}`)
      pair(`transition ${JSON.stringify(tid)} guards`, minus(ta.guards, tb.guards), minus(tb.guards, ta.guards), "widening", "lifecycle_guard_removed", "lifecycle_guard_added")
    }
  }
  return out
}

/** Human-readable plan. */
export function renderPlan(plan: Plan): string {
  const lines: string[] = []
  const section = (title: string, rows: ReadonlyArray<string>) => {
    if (rows.length === 0) return
    lines.push(`${title} (${rows.length}):`, ...rows.map((r) => `  ${r}`))
  }
  section("added", plan.added.map((a) => `+ ${a.artifactId} [${a.kind}]`))
  section("removed", plan.removed.map((a) => `- ${a.artifactId} [${a.kind}]`))
  section(
    "changed",
    plan.changed.map((c) => `~ ${c.artifactId} [${c.kind}]${c.fromVersion !== c.toVersion ? ` ${c.fromVersion ?? "?"} -> ${c.toVersion ?? "?"}` : ""}: ${c.members.join(", ")}`),
  )
  section("provenance only", plan.provenanceOnly)
  section("warnings", plan.warnings.map((w) => `${w.kind.toUpperCase()} ${w.objectType}.${w.property}: ${w.detail}`))
  section("narrowing changes", plan.narrowing.map((change) => `${change.kind} ${change.objectType ?? change.artifactId}${change.property ? `.${change.property}` : ""}: ${change.detail}`))
  lines.push(`unchanged: ${plan.unchanged.length}`)
  return lines.join("\n")
}
