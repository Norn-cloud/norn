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
import type {
  CheckerAbi,
  RippleBudgetExceeded,
  RippleEntry,
  RippleImpactKind,
  RippleStatus,
} from "./checker-abi.ts"
import { artifactId, isArtifact } from "./pack.ts"

export type PlanRipple = RippleEntry

export interface PlanRippleBudget {
  readonly maxNodes: number
  readonly maxEdges: number
  readonly maxSteps: number
}

export interface PlanDiffOptions {
  /** Authoritative Rust checker WASM ABI, when loaded. */
  readonly checker?: Pick<CheckerAbi, "rippleEffects">
  /** Explicit graph budget ceiling for offline/fallback ripple projection. */
  readonly budget?: PlanRippleBudget
}

const DEFAULT_RIPPLE_BUDGET: PlanRippleBudget = {
  maxNodes: 16_384,
  maxEdges: 131_072,
  maxSteps: 1_048_576,
}

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
  /** Exact transitive ripple effects caused by added, changed, or removed artifacts. */
  readonly ripples: ReadonlyArray<PlanRipple>
  /** Completeness of `ripples`: `"exact"` within budget, or `"unknown"` on budget exhaustion. */
  readonly rippleStatus: RippleStatus
  /** Explicit budget exhaustion metadata when `rippleStatus === "unknown"`. */
  readonly rippleBudgetExceeded?: RippleBudgetExceeded
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
export function planDiff(local: ReadonlyArray<Json>, remote: ReadonlyArray<Json>, options: PlanDiffOptions = {}): Plan {
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
  const sortedAdded = added.sort(byId)
  const sortedRemoved = removed.sort(byId)
  const sortedChanged = changed.sort(byId)
  const rootIds = [...new Set([
    ...sortedAdded.map((a) => a.artifactId),
    ...sortedRemoved.map((r) => r.artifactId),
    ...sortedChanged.map((c) => c.artifactId),
  ])].sort()
  const rippleReport = options.checker?.rippleEffects
    ? options.checker.rippleEffects(remote, local, rootIds)
    : computeLocalRipples(remote, local, rootIds, options.budget ?? DEFAULT_RIPPLE_BUDGET)
  return {
    added: sortedAdded,
    removed: sortedRemoved,
    changed: sortedChanged,
    provenanceOnly: provenanceOnly.sort(),
    unchanged: unchanged.sort(),
    warnings,
    narrowing,
    ripples: rippleReport.ripples,
    rippleStatus: rippleReport.status,
    ...(rippleReport.budgetExceeded ? { rippleBudgetExceeded: rippleReport.budgetExceeded } : {}),
  }
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

interface LocalRippleResult {
  readonly status: RippleStatus
  readonly ripples: ReadonlyArray<PlanRipple>
  readonly budgetExceeded?: RippleBudgetExceeded
}

function budgetExceeded(resource: "nodes" | "edges" | "steps", limit: number): LocalRippleResult {
  return {
    status: "unknown",
    ripples: [],
    budgetExceeded: { resource, limit, detail: `ripple graph ${resource} budget (${limit}) exceeded` },
  }
}

function computeLocalRipples(
  beforeArtifacts: ReadonlyArray<Json>,
  afterArtifacts: ReadonlyArray<Json>,
  changedIds: ReadonlyArray<string>,
  budget: PlanRippleBudget,
): LocalRippleResult {
  const before = index(beforeArtifacts), after = index(afterArtifacts)
  const allIds = new Set<string>([...before.keys(), ...after.keys()])
  const removedIds = new Set<string>([...before.keys()].filter((id) => !after.has(id)))
  const rootSet = new Set<string>(removedIds)
  for (const id of changedIds) if (allIds.has(id)) rootSet.add(id)
  if (allIds.size > budget.maxNodes) return budgetExceeded("nodes", budget.maxNodes)
  if (rootSet.size === 0) return { status: "exact", ripples: [] }

  const kinds = new Map<string, string>()
  const typeOwners = new Map<string, string>()
  const actionOwners = new Map<string, string>()
  const ruleOwners = new Map<string, string>()
  const lawOwners = new Map<string, string>()
  const widgetOwners = new Map<string, string>()
  const hasLifecycle = new Set<string>()
  const typeParents = new Map<string, Set<string>>()
  const typeWriters = new Map<string, Set<string>>()
  const lawSubjects: Array<readonly [string, string]> = []
  const edges = new Set<string>()
  const specializationEdges = new Set<string>()

  const addToSetMap = (map: Map<string, Set<string>>, key: string, value: string) => {
    let bucket = map.get(key)
    if (!bucket) { bucket = new Set(); map.set(key, bucket) }
    bucket.add(value)
  }

  const indexArtifact = (id: string, art: Obj) => {
    const k = typeof art.kind === "string" ? art.kind : "unknown"
    kinds.set(id, k)
    const payload = obj(art.payload)
    if (!payload) return
    if (k === "entity") {
      const ot = obj(payload.objectType)
      if (ot && typeof ot.name === "string") {
        typeOwners.set(ot.name, id)
        if (Array.isArray(ot.lifecycles) && ot.lifecycles.length > 0) hasLifecycle.add(id)
      }
      const iface = obj(payload.interface)
      if (iface && typeof iface.name === "string") {
        typeOwners.set(iface.name, id)
        if (Array.isArray(iface.lifecycles) && iface.lifecycles.length > 0) hasLifecycle.add(id)
      }
      if (typeof payload.objectKind === "string") typeOwners.set(payload.objectKind, id)
    } else if (k === "action" && typeof payload.actionId === "string") actionOwners.set(payload.actionId, id)
    else if (k === "rule" && typeof payload.ruleId === "string") ruleOwners.set(payload.ruleId, id)
    else if (k === "law" && typeof payload.id === "string") lawOwners.set(payload.id, id)
    else if (k === "widget" && typeof payload.widgetId === "string") widgetOwners.set(payload.widgetId, id)
  }

  for (const [id, art] of before) indexArtifact(id, art)
  for (const [id, art] of after) indexArtifact(id, art)

  let steps = 0
  const tick = (): LocalRippleResult | undefined => {
    steps++
    return steps > budget.maxSteps ? budgetExceeded("steps", budget.maxSteps) : undefined
  }
  const addEdge = (dependent: string, dependency: string): LocalRippleResult | undefined => {
    const err = tick()
    if (err) return err
    if (dependent === dependency || !allIds.has(dependent) || !allIds.has(dependency)) return undefined
    edges.add(`${dependent}\0${dependency}`)
    return edges.size > budget.maxEdges ? budgetExceeded("edges", budget.maxEdges) : undefined
  }
  const addSpec = (dependent: string, dependency: string): LocalRippleResult | undefined => {
    if (dependent !== dependency && allIds.has(dependent) && allIds.has(dependency)) {
      specializationEdges.add(`${dependent}\0${dependency}`)
    }
    return addEdge(dependent, dependency)
  }
  const addType = (dependent: string, name: unknown): LocalRippleResult | undefined => {
    if (typeof name !== "string") return undefined
    const owner = typeOwners.get(name)
    return owner ? addEdge(dependent, owner) : undefined
  }
  const addAction = (dependent: string, actionId: unknown): LocalRippleResult | undefined => {
    if (typeof actionId !== "string") return undefined
    return addEdge(dependent, actionOwners.get(actionId) ?? actionId)
  }

  for (const [id, art] of after) {
    if (Array.isArray(art.dependencies)) {
      for (const dep of art.dependencies) {
        const d = obj(dep)
        if (d && typeof d.artifactId === "string") {
          const err = addEdge(id, d.artifactId)
          if (err) return err
        }
      }
    }
    const k = kinds.get(id)
    const payload = obj(art.payload)
    if (!payload) continue
    if (k === "entity") {
      const ot = obj(payload.objectType)
      if (ot && typeof ot.name === "string") {
        const parents = [...(typeof ot.extends === "string" ? [ot.extends] : []), ...strings(ot.implements)]
        for (const p of parents) {
          addToSetMap(typeParents, ot.name, p)
          const owner = typeOwners.get(p)
          if (owner) { const err = addSpec(id, owner); if (err) return err }
        }
        for (const link of Array.isArray(ot.links) ? ot.links.map(obj) : []) {
          const err = addType(id, link?.target); if (err) return err
        }
      }
      const iface = obj(payload.interface)
      if (iface && typeof iface.name === "string") {
        for (const p of strings(iface.extends)) {
          addToSetMap(typeParents, iface.name, p)
          const owner = typeOwners.get(p)
          if (owner) { const err = addSpec(id, owner); if (err) return err }
        }
        for (const link of Array.isArray(iface.links) ? iface.links.map(obj) : []) {
          const err = addType(id, link?.target); if (err) return err
        }
      }
      const ext = obj(payload.objectTypeExtension)
      if (ext && typeof ext.target === "string") {
        const owner = typeOwners.get(ext.target)
        if (owner) { const err = addSpec(id, owner); if (err) return err }
        for (const link of Array.isArray(ext.links) ? ext.links.map(obj) : []) {
          const err = addType(id, link?.target); if (err) return err
        }
      }
    } else if (k === "relation") {
      const err1 = addType(id, payload.from); if (err1) return err1
      const err2 = addType(id, payload.to); if (err2) return err2
    } else if (k === "action") {
      for (const write of Array.isArray(payload.writes) ? payload.writes.map(obj) : []) {
        if (write && typeof write.entityKind === "string") {
          const err = addType(id, write.entityKind); if (err) return err
          addToSetMap(typeWriters, write.entityKind, id)
        }
      }
      const prog = obj(payload.effectProgram)
      if (prog) {
        if (typeof prog.entityArtifactId === "string") {
          const err = addEdge(id, prog.entityArtifactId); if (err) return err
        }
        if (typeof prog.objectKind === "string") {
          const err = addType(id, prog.objectKind); if (err) return err
          addToSetMap(typeWriters, prog.objectKind, id)
        }
      }
      for (const s of Array.isArray(payload.slice) ? payload.slice.map(obj) : []) {
        const err = addType(id, s?.objectType); if (err) return err
      }
      for (const a of Array.isArray(payload.aggregates) ? payload.aggregates.map(obj) : []) {
        const err = addType(id, a?.objectType); if (err) return err
      }
      const syncRule = obj(payload.synchronousRule)
      if (syncRule && typeof syncRule.artifactId === "string") { const err = addEdge(id, syncRule.artifactId); if (err) return err }
      const access = obj(payload.access)
      if (access && typeof access.artifactId === "string") { const err = addEdge(id, access.artifactId); if (err) return err }
      const comp = obj(payload.compensation)
      if (comp) { const err = addAction(id, comp.actionId); if (err) return err }
    } else if (k === "rule") {
      for (const s of Array.isArray(payload.slice) ? payload.slice.map(obj) : []) {
        const err = addType(id, s?.objectType); if (err) return err
      }
      for (const cmd of Array.isArray(payload.sourceCommands) ? payload.sourceCommands.map(obj) : []) {
        if (cmd && typeof cmd.connector === "string") { const err = addEdge(id, cmd.connector); if (err) return err }
        if (cmd && typeof cmd.mapping === "string") { const err = addEdge(id, cmd.mapping); if (err) return err }
      }
    } else if (k === "policy") {
      for (const r of Array.isArray(payload.rules) ? payload.rules.map(obj) : []) {
        if (!r) continue
        const err = addType(id, r.resourceType); if (err) return err
        for (const act of strings(r.actions)) { const err2 = addAction(id, act); if (err2) return err2 }
      }
      if (typeof payload.appliesTo === "string") {
        const err = addType(id, payload.appliesTo) ?? addAction(id, payload.appliesTo); if (err) return err
      }
    } else if (k === "law") {
      if (typeof payload.subject === "string") {
        const err = addType(id, payload.subject); if (err) return err
        lawSubjects.push([id, payload.subject])
      }
      const scope = obj(payload.scope)
      for (const ty of [...strings(scope?.objectTypes), ...strings(scope?.commitments)]) {
        const err = addType(id, ty); if (err) return err
      }
      const resp = obj(payload.response)
      if (resp) {
        if (typeof resp.artifactId === "string") { const err = addEdge(id, resp.artifactId); if (err) return err }
        const err = addType(id, resp.objectType) ?? addType(id, resp.targetType); if (err) return err
      }
    } else if (k === "derived") {
      for (const inp of Array.isArray(payload.inputs) ? payload.inputs.map(obj) : []) {
        if (inp && typeof inp.artifactId === "string") { const err = addEdge(id, inp.artifactId); if (err) return err }
      }
    } else if (k === "mapping") {
      const src = obj(payload.source)
      if (src && typeof src.schemaId === "string") { const err = addEdge(id, src.schemaId); if (err) return err }
      for (const iface of strings(payload.interfaces)) { const err = addEdge(id, iface); if (err) return err }
      for (const m of Array.isArray(payload.maps) ? payload.maps.map(obj) : []) {
        const target = obj(m?.target)
        if (target && typeof target.entity === "string") { const err = addEdge(id, target.entity); if (err) return err }
      }
    } else if (k === "defined_concept") {
      const base = obj(payload.base)
      if (base && typeof base.artifactId === "string") { const err = addSpec(id, base.artifactId); if (err) return err }
      for (const dep of Array.isArray(payload.dependencies) ? payload.dependencies.map(obj) : []) {
        if (dep && typeof dep.artifactId === "string") { const err = addEdge(id, dep.artifactId); if (err) return err }
      }
    } else if (k === "widget") {
      for (const dd of Array.isArray(payload.dataDependencies) ? payload.dataDependencies.map(obj) : []) {
        const err = addType(id, dd?.objectKind); if (err) return err
      }
      for (const ab of Array.isArray(payload.actionBindings) ? payload.actionBindings.map(obj) : []) {
        const err = addAction(id, ab?.actionId); if (err) return err
      }
      const ux = obj(payload.ux)
      if (ux) {
        for (const db of Array.isArray(ux.dataBindings) ? ux.dataBindings.map(obj) : []) {
          const objectRef = obj(db?.object)
          const targetId = typeof objectRef?.id === "string" ? objectRef.id : typeof objectRef?.artifactId === "string" ? objectRef.artifactId : undefined
          if (targetId) { const err = addEdge(id, targetId); if (err) return err }
        }
        for (const ab of Array.isArray(ux.actionBindings) ? ux.actionBindings.map(obj) : []) {
          const actionRef = obj(ab?.action)
          const targetId = typeof actionRef?.id === "string" ? actionRef.id : typeof actionRef?.artifactId === "string" ? actionRef.artifactId : undefined
          if (targetId) { const err = addEdge(id, targetId); if (err) return err }
        }
      }
    } else if (k === "model") {
      const err1 = addType(id, obj(payload.input)?.objectType); if (err1) return err1
      const err2 = addType(id, obj(payload.predictions)?.objectType); if (err2) return err2
    } else if (k === "customization") {
      const target = obj(payload.targetArtifact)
      if (target && typeof target.artifactId === "string") { const err = addEdge(id, target.artifactId); if (err) return err }
    } else if (k === "labels") {
      for (const entry of Array.isArray(payload.entries) ? payload.entries.map(obj) : []) {
        const target = obj(entry?.target)
        if (!target) continue
        if (typeof target.objectType === "string") { const err = addType(id, target.objectType); if (err) return err }
        if (typeof target.actionId === "string") { const err = addAction(id, target.actionId); if (err) return err }
        if (typeof target.ruleId === "string" && ruleOwners.has(target.ruleId)) { const err = addEdge(id, ruleOwners.get(target.ruleId)!); if (err) return err }
        if (typeof target.lawId === "string" && lawOwners.has(target.lawId)) { const err = addEdge(id, lawOwners.get(target.lawId)!); if (err) return err }
        if (typeof target.widgetId === "string" && widgetOwners.has(target.widgetId)) { const err = addEdge(id, widgetOwners.get(target.widgetId)!); if (err) return err }
      }
    } else if (k === "business_pack") {
      for (const member of Array.isArray(payload.artifacts) ? payload.artifacts.map(obj) : []) {
        if (member && typeof member.artifactId === "string") { const err = addEdge(id, member.artifactId); if (err) return err }
      }
    }
  }

  const childrenByParent = new Map<string, string[]>()
  for (const [child, parents] of typeParents) {
    for (const parent of parents) {
      let list = childrenByParent.get(parent)
      if (!list) { list = []; childrenByParent.set(parent, list) }
      list.push(child)
    }
  }
  for (const [lawId, subject] of lawSubjects) {
    const covered = new Set<string>([subject])
    const queue = [subject]
    while (queue.length > 0) {
      const err = tick(); if (err) return err
      const cur = queue.shift()!
      for (const child of childrenByParent.get(cur) ?? []) {
        if (!covered.has(child)) { covered.add(child); queue.push(child) }
      }
    }
    for (const ty of covered) {
      for (const writerId of typeWriters.get(ty) ?? []) {
        const err = addEdge(lawId, writerId); if (err) return err
      }
    }
  }

  const dependents = new Map<string, string[]>()
  for (const edge of [...edges].sort()) {
    const [dependent, dependency] = edge.split("\0") as [string, string]
    let bucket = dependents.get(dependency)
    if (!bucket) { bucket = []; dependents.set(dependency, bucket) }
    bucket.push(dependent)
  }

  const triggeredBy = new Map<string, Set<string>>()
  const bestVia = new Map<string, string[]>()
  const sortedRoots = [...rootSet].sort()
  for (const root of sortedRoots) {
    const parent = new Map<string, string | null>([[root, null]])
    const queue = [root]
    while (queue.length > 0) {
      const err = tick(); if (err) return err
      const cur = queue.shift()!
      addToSetMap(triggeredBy, cur, root)
      const path: string[] = [cur]
      let cursor = cur
      while (parent.get(cursor) !== null && parent.get(cursor) !== undefined) {
        cursor = parent.get(cursor)!
        path.push(cursor)
      }
      path.reverse()
      const existing = bestVia.get(cur)
      if (!existing || path.length < existing.length || (path.length === existing.length && path.join("\0") < existing.join("\0"))) {
        bestVia.set(cur, path)
      }
      for (const next of dependents.get(cur) ?? []) {
        const err2 = tick(); if (err2) return err2
        if (!parent.has(next)) { parent.set(next, cur); queue.push(next) }
      }
    }
  }

  const classify = (id: string, via: ReadonlyArray<string>): RippleImpactKind => {
    if (rootSet.has(id)) return "direct_change"
    for (const removed of removedIds) {
      if (edges.has(`${id}\0${removed}`)) return "removed_dependency_broken"
    }
    const k = kinds.get(id)
    if (k === "law") return "law_affected"
    if (k === "policy") return "policy_affected"
    if (k === "action" || k === "rule" || k === "connector" || k === "mapping") return "writer_affected"
    if (k === "defined_concept") return "specialization_affected"
    if (k === "relation") return "lifecycle_affected"
    if (k === "entity") {
      const prev = via.length >= 2 ? via[via.length - 2] : undefined
      if (prev && specializationEdges.has(`${id}\0${prev}`)) return "specialization_affected"
      return hasLifecycle.has(id) ? "lifecycle_affected" : "specialization_affected"
    }
    return "projection_or_ui_affected"
  }

  const ripples: PlanRipple[] = [...triggeredBy.keys()].map((id) => {
    const via = bestVia.get(id) ?? [id]
    return {
      artifactId: id,
      kind: kinds.get(id) ?? "unknown",
      triggeredBy: [...(triggeredBy.get(id) ?? [])].sort(),
      via,
      impactKind: classify(id, via),
    }
  })
  ripples.sort((left, right) => (left.via.length - right.via.length) || left.artifactId.localeCompare(right.artifactId))
  return { status: "exact", ripples }
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
  if (plan.rippleStatus === "unknown") {
    lines.push(`transitive ripples: UNKNOWN (${plan.rippleBudgetExceeded?.detail ?? "graph budget exceeded"})`)
  } else {
    section(
      "transitive ripples",
      plan.ripples.map((r) => `[${r.impactKind}] ${r.artifactId} [${r.kind}] (triggered by: ${r.triggeredBy.join(", ")}; via: ${r.via.join(" -> ")})`),
    )
  }
  lines.push(`unchanged: ${plan.unchanged.length}`)
  return lines.join("\n")
}
