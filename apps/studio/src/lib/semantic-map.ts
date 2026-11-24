import { cellOwned, type Catalog, type Concept, type Law, type SourceSystem } from "./catalog.ts"
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
  { id: "attention", label: "Attention", detail: "Spotlight islands and types with held records or sync errors" },
]

/**
 * Honest node/island status precedence (DESIGN.md §5):
 * quarantined/held > source_error > active (never invents "verified" from empty arrays).
 */
export type MapAttentionTone = "none" | "warning" | "critical"

export interface SemanticNode {
  readonly id: string
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
  readonly heldCount: number
  readonly heldApproximate: boolean
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
  readonly heldCount: number
  readonly heldApproximate: boolean
  readonly sourceErrors: number
  readonly attention: MapAttentionTone
}

export interface SemanticMapProjection {
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
    readonly held: number
    readonly sourceErrors: number
  }
}

export interface NodeProvenanceDossier {
  readonly node: SemanticNode
  readonly island: SemanticIsland
  readonly incoming: readonly SemanticBridgeEdge[]
  readonly outgoing: readonly SemanticBridgeEdge[]
  readonly laws: readonly Law[]
  readonly sourceFeeds: readonly { readonly system: string; readonly dataset: string; readonly mappedFields: number }[]
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

function toneOf(held: number, sourceErrors: number, hasWarningLaw: boolean): MapAttentionTone {
  if (held > 0 || sourceErrors > 0) return "critical"
  if (hasWarningLaw) return "warning"
  return "none"
}

/**
 * Deterministically project the flat, cell-filtered SoR ontology into monochrome structural
 * Islands + bundled cross-island Bridges (#1664). Never invents hidden counts or unserved entities.
 */
export function buildSemanticMap(catalog: Catalog, inbox?: Inbox): SemanticMapProjection {
  const objectTypes = catalog.concepts.filter(({ definition }) => definition === "object type")
  const externalSystems = new Map<string, SourceSystem>()
  for (const system of catalog.systems) {
    if (cellOwned(system)) continue
    externalSystems.set(system.id, system)
    externalSystems.set(system.name, system)
  }

  const heldByType = new Map<string, { count: number; approximate: boolean }>()
  const sourceErrorsBySystem = new Map<string, number>()
  for (const item of inbox?.items ?? []) {
    if (item.kind === "held_back") {
      const current = heldByType.get(item.objectType) ?? { count: 0, approximate: false }
      heldByType.set(item.objectType, {
        count: current.count + item.count,
        approximate: current.approximate || Boolean(item.approximate),
      })
    } else if (item.kind === "source_error") {
      const systemId = typeof item.values.system === "string"
        ? item.values.system
        : typeof item.values.source_system === "string"
          ? item.values.source_system
          : item.id
      sourceErrorsBySystem.set(systemId, (sourceErrorsBySystem.get(systemId) ?? 0) + 1)
    }
  }

  // Assign each object type deterministically to its primary external feeding system when present,
  // else its canonical namespace (so multi-system topologies cluster by operational domain without hairballs).
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

  for (const concept of [...objectTypes].sort((a, b) => a.title.localeCompare(b.title))) {
    const primaryFeed = concept.feeds.find(({ system }) => externalSystems.has(system))
    if (primaryFeed) {
      const sys = externalSystems.get(primaryFeed.system)!
      ensureBucket(`system:${sys.id}`, sys.name, "system", sys).concepts.push(concept)
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
    return a.title.localeCompare(b.title)
  })) {
    const nodes: SemanticNode[] = bucket.concepts.map((concept) => {
      const markings = [...new Set(concept.fields.flatMap((field) => field.markings ?? []))]
      const feeds = [...new Set(concept.feeds.map((feed) => feed.system))]
      const held = heldByType.get(concept.name) ?? { count: 0, approximate: false }
      const node: SemanticNode = {
        id: concept.name,
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
        heldCount: held.count,
        heldApproximate: held.approximate,
        attention: toneOf(held.count, 0, false),
        concept,
      }
      nodesById.set(node.id, node)
      return node
    })

    const lawCount = nodes.reduce((sum, node) => sum + node.lawCount, 0)
    const markingCount = new Set(nodes.flatMap((node) => node.markings)).size
    const heldCount = nodes.reduce((sum, node) => sum + node.heldCount, 0)
    const heldApproximate = nodes.some((node) => node.heldApproximate)
    const sourceErrors = bucket.system
      ? (sourceErrorsBySystem.get(bucket.system.id) ?? 0) + (bucket.system.name !== bucket.system.id ? (sourceErrorsBySystem.get(bucket.system.name) ?? 0) : 0)
      : 0
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
      heldCount,
      heldApproximate,
      sourceErrors,
      attention: toneOf(heldCount, sourceErrors, false),
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

  return {
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
      held: islands.reduce((sum, island) => sum + island.heldCount, 0),
      sourceErrors: islands.reduce((sum, island) => sum + island.sourceErrors, 0),
    },
  }
}

/** Whether a node is highlighted by the active switchable lens. */
export function nodeMatchesLens(node: SemanticNode, lens: SemanticLens): boolean {
  switch (lens) {
    case "all": return true
    case "flow": return node.feeds.length > 0 || node.relationshipCount > 0
    case "governance": return node.lawCount > 0 || node.markingCount > 0 || node.lifecycleCount > 0
    case "attention": return node.attention !== "none"
  }
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

/** Build the Zoom-2 Living Dossier / Provenance Side Sheet view for one selected node. */
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
    sourceFeeds: node.concept.feeds.map((feed) => ({
      system: feed.system,
      dataset: feed.dataset,
      mappedFields: feed.facts.size,
    })),
  }
}
