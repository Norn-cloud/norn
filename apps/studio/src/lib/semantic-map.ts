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
      readonly heldBack: number
      readonly heldBackApproximate: boolean
      readonly sourceErrors: number
      readonly sourceErrorsApproximate: boolean
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

function countFeedFacts(facts: ReadonlyMap<string, readonly Source[]>): { mappedFields: number; gapCount: number } {
  let mappedFields = 0
  let gapCount = 0
  for (const sources of facts.values()) {
    if (sources.length === 0) continue
    if (sources.some((s) => s.kind !== "gap" && s.kind !== "empty")) {
      mappedFields += 1
    } else if (sources.every((s) => s.kind === "gap")) {
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

  if (liveInbox) {
    for (const item of liveInbox.items) {
      if (item.kind === "held_back") {
        const current = holdingsByType.get(item.objectType) ?? { count: 0, approximate: false }
        holdingsByType.set(item.objectType, {
          count: current.count + item.count,
          approximate: current.approximate || Boolean(item.approximate),
        })
      } else if (item.kind === "source_error") {
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

  for (const bucket of [...islandBuckets.values()].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === "system" ? -1 : b.kind === "system" ? 1 : a.kind === "namespace" ? -1 : 1
    return a.title.localeCompare(b.title) || a.id.localeCompare(b.id)
  })) {
    const nodes: SemanticNode[] = bucket.concepts.map((concept) => {
      const markings = [...new Set(concept.fields.flatMap((field) => field.markings ?? []))]
      const feeds = [...new Set(concept.feeds.map((feed) => feed.system))]
      const held = holdingsByType.get(concept.name) ?? { count: 0, approximate: false }
      const nodeAttention: MapAttentionTone = !liveInbox
        ? "unknown"
        : held.count > 0
          ? "critical"
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
        lawCount: concept.laws.length,
        lifecycleCount: concept.lifecycles.length,
        markingCount: markings.length,
        markings,
        feeds,
        ruleHoldings: held.count,
        ruleHoldingsApproximate: held.approximate,
        attention: nodeAttention,
        concept,
      }
      nodesById.set(node.id, node)
      return node
    })

    const lawCount = nodes.reduce((sum, node) => sum + node.lawCount, 0)
    const markingCount = new Set(nodes.flatMap((node) => node.markings)).size
    const ruleHoldings = nodes.reduce((sum, node) => sum + node.ruleHoldings, 0)
    const ruleHoldingsApproximate = nodes.some((node) => node.ruleHoldingsApproximate)
    const sourceErrors = bucket.system ? (sourceErrorsBySystemId.get(bucket.system.id) ?? 0) : 0
    const islandAttention: MapAttentionTone = !liveInbox
      ? "unknown"
      : (ruleHoldings > 0 || sourceErrors > 0)
        ? "critical"
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
    ? (() => {
        const heldSection = liveInbox.sections.find((s) => s.kind === "held_back")
        const sourceSection = liveInbox.sections.find((s) => s.kind === "source_error")
        return {
          heldBack: heldSection?.count ?? 0,
          heldBackApproximate: Boolean(heldSection?.approximate),
          sourceErrors: sourceSection?.count ?? 0,
          sourceErrorsApproximate: Boolean(sourceSection?.approximate),
          scanLimit: liveInbox.scanLimit,
        }
      })()
    : null

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
      laws: islands.reduce((sum, island) => sum + island.lawCount, 0),
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
  const queryMatchesIsland = q === "" || island.title.toLowerCase().includes(q)
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
