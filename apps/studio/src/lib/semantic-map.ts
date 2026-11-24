import { cellOwned, type Catalog, type Concept, type Law, type Source, type SourceSystem } from "./catalog.ts"
import type { Inbox } from "../inbox.tsx"

/** Switchable zero-authority lenses over the exact same flat SoR ontology (#1664). */
export type SemanticLens = "all" | "flow" | "governance" | "attention"

export const SEMANTIC_LENSES: readonly {
  readonly id: SemanticLens
  readonly label: string
  readonly detail: string
}[] = [
  { id: "all", label: "All", detail: "Full ontology grouped into structural islands" },
  { id: "flow", label: "Data flow", detail: "Connected sources, fed object types, and cross-island links" },
  { id: "governance", label: "Governance", detail: "Object types carrying admitted laws, markings, or lifecycles" },
  { id: "attention", label: "Attention", detail: "Spotlight islands and types with rule holdings or sync errors" },
]

/**
 * Honest attention state:
 * - `critical`: live inbox reports rule holdings or source sync errors
 * - `none`: live inbox loaded cleanly and reported zero alerts for this entity
 * - `unknown`: live inbox is loading, unavailable/unserved, or workspace is recorded (never reports healthy/no alerts)
 */
export type MapAttentionTone = "unknown" | "none" | "warning" | "critical"

export type SemanticMapAttentionInput =
  | { readonly kind: "recorded" }
  | { readonly kind: "loading" }
  | { readonly kind: "unavailable" }
  | { readonly kind: "live"; readonly inbox: Inbox }

export interface SemanticNode {
  readonly id: string
  readonly artifactId: string
  readonly title: string
  readonly titleLang: string
  readonly islandId: string
  readonly summary: string | undefined
  readonly fieldCount: number
  readonly relationshipCount: number
  readonly lawCount: number
  readonly lifecycleCount: number
  readonly markingCount: number
  readonly markings: readonly string[]
  readonly feeds: readonly string[]
  /** Sum of per-rule holding counts reported for this type (one record held by two rules appears in both rules). */
  readonly ruleHoldings: number
  readonly ruleHoldingsApproximate: boolean
  /** Count of feeding systems (primary or secondary) on this type reporting a live source sync error. */
  readonly sourceErrors: number
  readonly attention: MapAttentionTone
  readonly concept: Concept
}

export interface SemanticBridgeEdge {
  readonly id: string
  readonly sourceConcept: string
  readonly sourceTitle: string
  readonly targetConcept: string
  readonly targetTitle: string
  readonly relationshipId: string
  readonly count: "many" | "one" | "at most one"
}

export interface SemanticBridge {
  readonly id: string
  readonly fromIsland: string
  readonly toIsland: string
  readonly count: number
  readonly edges: readonly SemanticBridgeEdge[]
}

export interface SemanticIsland {
  readonly id: string
  readonly title: string
  readonly kind: "system" | "namespace" | "core"
  readonly subtitle: string
  readonly system: SourceSystem | undefined
  readonly nodes: readonly SemanticNode[]
  readonly lawCount: number
  readonly markingCount: number
  readonly ruleHoldings: number
  readonly ruleHoldingsApproximate: boolean
  readonly sourceErrors: number
  /** Names and mapping ids of feeding systems on this island currently reporting a live source sync error. */
  readonly errorSystems: readonly string[]
  readonly attention: MapAttentionTone
}

export interface SemanticMapProjection {
  readonly attentionStatus: SemanticMapAttentionInput["kind"]
  readonly islands: readonly SemanticIsland[]
  readonly bridges: readonly SemanticBridge[]
  readonly nodesById: ReadonlyMap<string, SemanticNode>
  readonly islandsById: ReadonlyMap<string, SemanticIsland>
  readonly totals: {
    readonly islands: number
    readonly objectTypes: number
    readonly bridges: number
    readonly links: number
    readonly laws: number
    /** Workspace-level alert summary directly from server inbox sections (null when live inbox is not available). */
    readonly alerts: {
      /** Sum of per-rule holdings reported by inbox `held_back` section (one record held by two rules counts twice). */
      readonly ruleHoldings: number
      readonly ruleHoldingsApproximate: boolean
      readonly sourceErrors: number
      readonly sourceErrorsApproximate: boolean
      /** True when inbox sections/items are capped or scanned with a bound (unseen islands cannot be marked healthy). */
      readonly bounded: boolean
      readonly scanLimit: number | undefined
    } | null
  }
}

export interface NodeProvenanceDossier {
  readonly node: SemanticNode
  readonly island: SemanticIsland
  readonly incoming: readonly SemanticBridgeEdge[]
  readonly outgoing: readonly SemanticBridgeEdge[]
  readonly laws: readonly Law[]
  readonly sourceFeeds: readonly {
    readonly system: string
    readonly mappingId: string | undefined
    readonly dataset: string
    readonly mappedFields: number
    readonly gapCount: number
  }[]
}

function namespaceOf(conceptName: string): string {
  const dot = conceptName.indexOf(".")
  return dot > 0 ? conceptName.slice(0, dot) : "core"
}

function titleCase(token: string): string {
  return token
    .split(/[._-]+/)
    .filter(Boolean)
    .map((part) => `${part[0]!.toUpperCase()}${part.slice(1)}`)
    .join(" ")
}

function isMappedSource(source: Source): boolean {
  if (source.kind === "gap" || source.kind === "empty") return false
  if (source.kind === "nested") return isMappedSource(source.value)
  return true
}

function isGapSource(source: Source): boolean {
  if (source.kind === "gap") return true
  if (source.kind === "nested") return isGapSource(source.value)
  return false
}

function countFeedFacts(facts: ReadonlyMap<string, readonly Source[]>): { mappedFields: number; gapCount: number } {
  let mappedFields = 0
  let gapCount = 0
  for (const sources of facts.values()) {
    if (sources.length === 0) continue
    if (sources.some(isMappedSource)) {
      mappedFields += 1
    } else if (sources.every(isGapSource)) {
      gapCount += 1
    }
  }
  return { mappedFields, gapCount }
}

/**
 * Deterministically project the caller's admitted `/v1/definitions` catalog into monochrome
 * structural Islands + bundled cross-island Bridges.
 * Never fabricates live alert counts when inbox attention data is pending, unavailable, or recorded.
 */
export function buildSemanticMap(
  catalog: Catalog,
  attention: SemanticMapAttentionInput = { kind: "recorded" },
): SemanticMapProjection {
  const objectTypes = catalog.concepts.filter(({ definition }) => definition === "object type")
  const externalById = new Map<string, SourceSystem>()
  const nameCounts = new Map<string, number>()
  for (const system of catalog.systems) {
    if (cellOwned(system)) continue
    externalById.set(system.id, system)
    nameCounts.set(system.name, (nameCounts.get(system.name) ?? 0) + 1)
  }
  // Name lookup is used only as a fallback for feeds lacking `mappingId`, and only when unambiguous.
  const uniqueExternalByName = new Map<string, SourceSystem>()
  for (const system of externalById.values()) {
    if (nameCounts.get(system.name) === 1) {
      uniqueExternalByName.set(system.name, system)
    }
  }

  const resolveSystem = (mappingId: string | undefined, systemName: string | undefined): SourceSystem | undefined => {
    if (mappingId && externalById.has(mappingId)) return externalById.get(mappingId)
    if (systemName && externalById.has(systemName)) return externalById.get(systemName)
    if (systemName && uniqueExternalByName.has(systemName)) return uniqueExternalByName.get(systemName)
    return undefined
  }

  const liveInbox = attention.kind === "live" ? attention.inbox : undefined
  const holdingsByType = new Map<string, { count: number; approximate: boolean }>()
  const sourceErrorsBySystemId = new Map<string, number>()
  let returnedHoldingsSum = 0
  let returnedSourceErrors = 0

  if (liveInbox) {
    for (const item of liveInbox.items) {
      if (item.kind === "held_back") {
        returnedHoldingsSum += item.count
        const current = holdingsByType.get(item.objectType) ?? { count: 0, approximate: false }
        holdingsByType.set(item.objectType, {
          count: current.count + item.count,
          approximate: current.approximate || Boolean(item.approximate),
        })
      } else if (item.kind === "source_error") {
        returnedSourceErrors += 1
        const mappingId = typeof item.values.mapping_id === "string" ? item.values.mapping_id : undefined
        const systemName = typeof item.values.system === "string"
          ? item.values.system
          : typeof item.values.source_system === "string"
            ? item.values.source_system
            : undefined
        const matched = resolveSystem(mappingId, systemName) ?? resolveSystem(item.id, undefined)
        if (matched) {
          sourceErrorsBySystemId.set(matched.id, (sourceErrorsBySystemId.get(matched.id) ?? 0) + 1)
        }
      }
    }
  }

  const heldSection = liveInbox?.sections.find((s) => s.kind === "held_back")
  const sourceSection = liveInbox?.sections.find((s) => s.kind === "source_error")
  // Inbox caps returned groups/source rows even when section totals are exact; never mark
  // unseen islands/nodes green if any alert fell outside the returned slice or was approximate.
  // Note: `liveInbox.scanLimit` is always emitted as a constant server scan budget (e.g. 500)
  // and does not indicate truncation unless `approximate` is true or returned rows < section count.
  const holdingsBounded = Boolean(
    liveInbox && (
      heldSection?.approximate
      || (heldSection?.count ?? 0) > returnedHoldingsSum
    ),
  )
  const sourcesBounded = Boolean(
    liveInbox && (
      sourceSection?.approximate
      || (sourceSection?.count ?? 0) > returnedSourceErrors
    ),
  )
  const inboxBounded = holdingsBounded || sourcesBounded

  const islandBuckets = new Map<string, {
    id: string
    title: string
    kind: "system" | "namespace" | "core"
    system: SourceSystem | undefined
    concepts: Concept[]
  }>()

  const ensureBucket = (id: string, title: string, kind: "system" | "namespace" | "core", system?: SourceSystem) => {
    let bucket = islandBuckets.get(id)
    if (!bucket) {
      bucket = { id, title, kind, system, concepts: [] }
      islandBuckets.set(id, bucket)
    }
    return bucket
  }

  for (const concept of [...objectTypes].sort((a, b) => a.title.localeCompare(b.title) || a.name.localeCompare(b.name))) {
    // Deterministically choose primary feeding system by canonical mappingId (or system name) order.
    const matchedFeeds = concept.feeds
      .map((feed) => ({ feed, system: resolveSystem(feed.mappingId, feed.system) }))
      .filter((entry): entry is { readonly feed: typeof entry.feed; readonly system: SourceSystem } => entry.system !== undefined)
      .sort((a, b) => a.system.id.localeCompare(b.system.id))

    const primary = matchedFeeds[0]?.system
    if (primary) {
      const displayTitle = (nameCounts.get(primary.name) ?? 0) > 1
        ? `${primary.name} (${primary.id})`
        : primary.name
      ensureBucket(`system:${primary.id}`, displayTitle, "system", primary).concepts.push(concept)
      continue
    }
    const ns = namespaceOf(concept.name)
    if (ns === "core" || ns === "std") {
      ensureBucket("core:model", "Core model", "core").concepts.push(concept)
    } else {
      ensureBucket(`ns:${ns}`, titleCase(ns), "namespace").concepts.push(concept)
    }
  }

  const nodesById = new Map<string, SemanticNode>()
  const islands: SemanticIsland[] = []

  const lawIdentity = (law: Law) => (law.artifactId ? `${law.artifactId}:${law.id}` : law.id)

  for (const bucket of [...islandBuckets.values()].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === "system" ? -1 : b.kind === "system" ? 1 : a.kind === "namespace" ? -1 : 1
    return a.title.localeCompare(b.title) || a.id.localeCompare(b.id)
  })) {
    const nodes: SemanticNode[] = bucket.concepts.map((concept) => {
      const markings = [...new Set(concept.fields.flatMap((field) => field.markings ?? []))]
      const feeds = [...new Set(concept.feeds.map((feed) => feed.system))]
      const held = holdingsByType.get(concept.name) ?? { count: 0, approximate: false }
      const nodeSystemIds = new Set<string>()
      for (const feed of concept.feeds) {
        const matched = resolveSystem(feed.mappingId, feed.system)
        if (matched) nodeSystemIds.add(matched.id)
      }
      let nodeSourceErrors = 0
      for (const sysId of nodeSystemIds) {
        nodeSourceErrors += sourceErrorsBySystemId.get(sysId) ?? 0
      }
      // Use `inboxBounded` (both held-back and source-error bounds) so a node never paints green
      // when any section of the inbox is approximate or truncated.
      const nodeAttention: MapAttentionTone = !liveInbox
        ? "unknown"
        : (held.count > 0 || nodeSourceErrors > 0)
          ? "critical"
          : inboxBounded
            ? "unknown"
            : "none"
      const node: SemanticNode = {
        id: concept.name,
        artifactId: concept.artifactId ?? concept.name,
        title: concept.title,
        titleLang: concept.titleLang,
        islandId: bucket.id,
        summary: concept.summary,
        fieldCount: concept.fields.length,
        relationshipCount: concept.relationships.length,
        lawCount: new Set(concept.laws.map(lawIdentity)).size,
        lifecycleCount: concept.lifecycles.length,
        markingCount: markings.length,
        markings,
        feeds,
        ruleHoldings: held.count,
        ruleHoldingsApproximate: held.approximate,
        sourceErrors: nodeSourceErrors,
        attention: nodeAttention,
        concept,
      }
      nodesById.set(node.id, node)
      return node
    })

    const lawCount = new Set(nodes.flatMap((node) => node.concept.laws.map(lawIdentity))).size
    const markingCount = new Set(nodes.flatMap((node) => node.markings)).size
    const ruleHoldings = nodes.reduce((sum, node) => sum + node.ruleHoldings, 0)
    const ruleHoldingsApproximate = nodes.some((node) => node.ruleHoldingsApproximate)
    // Count sync errors from the island's primary system AND any secondary feeding systems on its concepts
    // (deduplicated by system id so a failed secondary feed never disappears from Attention).
    const islandSystems = new Map<string, SourceSystem>()
    if (bucket.system) islandSystems.set(bucket.system.id, bucket.system)
    for (const concept of bucket.concepts) {
      for (const feed of concept.feeds) {
        const matched = resolveSystem(feed.mappingId, feed.system)
        if (matched) islandSystems.set(matched.id, matched)
      }
    }
    let sourceErrors = 0
    const errorSystems = new Set<string>()
    for (const [sysId, sys] of islandSystems) {
      const count = sourceErrorsBySystemId.get(sysId) ?? 0
      if (count > 0) {
        sourceErrors += count
        errorSystems.add(sys.name)
        errorSystems.add(sys.id)
      }
    }
    const islandAttention: MapAttentionTone = !liveInbox
      ? "unknown"
      : (ruleHoldings > 0 || sourceErrors > 0)
        ? "critical"
        : inboxBounded
          ? "unknown"
          : "none"
    const subtitle = bucket.kind === "system" && bucket.system
      ? `${bucket.system.datasets.length} source ${bucket.system.datasets.length === 1 ? "dataset" : "datasets"}`
      : bucket.kind === "core"
        ? "Shared model primitives"
        : "Model namespace"

    islands.push({
      id: bucket.id,
      title: bucket.title,
      kind: bucket.kind,
      subtitle,
      system: bucket.system,
      nodes,
      lawCount,
      markingCount,
      ruleHoldings,
      ruleHoldingsApproximate,
      sourceErrors,
      errorSystems: [...errorSystems],
      attention: islandAttention,
    })
  }

  const islandsById = new Map(islands.map((island) => [island.id, island] as const))

  // Bundle every relationship that crosses between two distinct Islands into a single aggregate bridge.
  const bridgeMap = new Map<string, { fromIsland: string; toIsland: string; edges: SemanticBridgeEdge[] }>()
  let totalLinks = 0
  for (const node of nodesById.values()) {
    for (const rel of node.concept.relationships) {
      const target = nodesById.get(rel.target)
      if (!target) continue
      totalLinks += 1
      if (target.islandId === node.islandId) continue
      const key = `${node.islandId}->${target.islandId}`
      let entry = bridgeMap.get(key)
      if (!entry) {
        entry = { fromIsland: node.islandId, toIsland: target.islandId, edges: [] }
        bridgeMap.set(key, entry)
      }
      entry.edges.push({
        id: `${node.id}.${rel.id}->${target.id}`,
        sourceConcept: node.id,
        sourceTitle: node.title,
        targetConcept: target.id,
        targetTitle: target.title,
        relationshipId: rel.id,
        count: rel.count,
      })
    }
  }

  const bridges: SemanticBridge[] = [...bridgeMap.entries()]
    .map(([id, entry]) => ({
      id,
      fromIsland: entry.fromIsland,
      toIsland: entry.toIsland,
      count: entry.edges.length,
      edges: entry.edges,
    }))
    .sort((a, b) => b.count - a.count || a.id.localeCompare(b.id))

  const alerts = liveInbox
    ? {
        ruleHoldings: heldSection?.count ?? 0,
        ruleHoldingsApproximate: Boolean(heldSection?.approximate),
        sourceErrors: sourceSection?.count ?? 0,
        sourceErrorsApproximate: Boolean(sourceSection?.approximate),
        bounded: inboxBounded,
        scanLimit: liveInbox.scanLimit,
      }
    : null

  const distinctLaws = new Set(
    [...nodesById.values()].flatMap((node) => node.concept.laws.map(lawIdentity)),
  ).size

  return {
    attentionStatus: attention.kind,
    islands,
    bridges,
    nodesById,
    islandsById,
    totals: {
      islands: islands.length,
      objectTypes: nodesById.size,
      bridges: bridges.length,
      links: totalLinks,
      laws: distinctLaws,
      alerts,
    },
  }
}

/** Whether a node is highlighted by the active switchable lens. */
export function nodeMatchesLens(node: SemanticNode, lens: SemanticLens): boolean {
  switch (lens) {
    case "all": return true
    case "flow": return node.feeds.length > 0 || node.relationshipCount > 0
    case "governance": return node.lawCount > 0 || node.markingCount > 0 || node.lifecycleCount > 0
    case "attention": return node.attention === "critical" || node.attention === "warning"
  }
}

/** Whether an island has any matching node OR an island-level sync error under the active lens. */
export function islandMatchesLens(island: SemanticIsland, lens: SemanticLens, query: string): boolean {
  const q = query.trim().toLowerCase()
  const queryMatchesIsland = q === ""
    || island.title.toLowerCase().includes(q)
    || island.errorSystems.some((name) => name.toLowerCase().includes(q))
  if (lens === "attention" && island.sourceErrors > 0 && queryMatchesIsland) return true
  return island.nodes.some((node) => nodeMatchesLens(node, lens) && nodeMatchesQuery(node, query))
}

/** Whether a node matches a free-text Foundry-style object search query. */
export function nodeMatchesQuery(node: SemanticNode, query: string): boolean {
  const q = query.trim().toLowerCase()
  if (q === "") return true
  return (
    node.title.toLowerCase().includes(q)
    || node.id.toLowerCase().includes(q)
    || (node.summary?.toLowerCase().includes(q) ?? false)
    || node.feeds.some((feed) => feed.toLowerCase().includes(q))
    || node.markings.some((marking) => marking.toLowerCase().includes(q))
  )
}

/** Build the Zoom-2 Schema & Provenance Side Sheet view for one selected node. */
export function provenanceForNode(projection: SemanticMapProjection, nodeId: string): NodeProvenanceDossier | null {
  const node = projection.nodesById.get(nodeId)
  if (!node) return null
  const island = projection.islandsById.get(node.islandId)
  if (!island) return null

  const outgoing: SemanticBridgeEdge[] = node.concept.relationships
    .map((rel) => {
      const target = projection.nodesById.get(rel.target)
      if (!target) return null
      return {
        id: `${node.id}.${rel.id}->${target.id}`,
        sourceConcept: node.id,
        sourceTitle: node.title,
        targetConcept: target.id,
        targetTitle: target.title,
        relationshipId: rel.id,
        count: rel.count,
      }
    })
    .filter((edge): edge is SemanticBridgeEdge => edge !== null)

  const incoming: SemanticBridgeEdge[] = []
  for (const candidate of projection.nodesById.values()) {
    for (const rel of candidate.concept.relationships) {
      if (rel.target !== node.id) continue
      incoming.push({
        id: `${candidate.id}.${rel.id}->${node.id}`,
        sourceConcept: candidate.id,
        sourceTitle: candidate.title,
        targetConcept: node.id,
        targetTitle: node.title,
        relationshipId: rel.id,
        count: rel.count,
      })
    }
  }

  return {
    node,
    island,
    incoming,
    outgoing,
    laws: node.concept.laws,
    sourceFeeds: node.concept.feeds.map((feed) => {
      const { mappedFields, gapCount } = countFeedFacts(feed.facts)
      return {
        system: feed.system,
        mappingId: feed.mappingId,
        dataset: feed.dataset,
        mappedFields,
        gapCount,
      }
    }),
  }
}
