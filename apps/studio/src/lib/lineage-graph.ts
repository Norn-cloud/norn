import { systemName } from "./catalog.ts"
import type { SourceLineage } from "./transport.ts"

/** A record version the lineage names: the root, or an upstream fact a transform read. */
export interface FactRef { readonly objectType: string; readonly key: string; readonly version: number }

export interface LineageNode {
  readonly id: string
  readonly kind: "record" | "fact" | "source" | "document"
  readonly title: string
  readonly detail: string
  /** Hops from the root: 0 is the record itself. */
  readonly hop: number
  /** For a fact node: the record version it names. */
  readonly ref?: FactRef
}

export interface LineageEdge {
  readonly from: string
  readonly to: string
  /** The mapping rules that read across it, in declaration order. */
  readonly rules: readonly string[]
  /** The facts it feeds at its target. */
  readonly facts: readonly string[]
}

export interface LineageGraph {
  readonly nodes: readonly LineageNode[]
  readonly edges: readonly LineageEdge[]
  /** Whether the lineage goes on past the hops shown. */
  readonly further: boolean
  /** Upstream facts at the edge of the hops shown whose lineage is not read yet. */
  readonly unread: readonly FactRef[]
}

export const factKey = ({ objectType, key, version }: FactRef): string => `fact:${objectType}/${key}@${version}`
const sourceKey = (system: string, record: string) => `source:${system}/${record}`
const documentKey = (system: string, record: string) => `document:${system}/${record}`

/** The most nodes one graph draws; past it the list twin carries the rest. */
export const NODE_CAP = 60

/**
 * The lineage of one record version, `hops` deep, from the server's lineage documents: the source
 * records each fact was read from (one hop), the source document behind them and the upstream facts
 * a transform read (two), and those facts' own lineage after that. `lineages` holds the documents
 * read so far for upstream facts, by {@link factKey}; a fact whose lineage is `null` has none.
 */
export function lineageGraph(root: FactRef, lineage: SourceLineage, lineages: ReadonlyMap<string, SourceLineage | null>, hops: number): LineageGraph {
  const nodes = new Map<string, LineageNode>()
  const edges = new Map<string, { from: string; to: string; rules: string[]; facts: string[] }>()
  const unread: FactRef[] = []
  let further = false
  const node = (entry: LineageNode) => { if (!nodes.has(entry.id) && nodes.size < NODE_CAP) nodes.set(entry.id, entry); return nodes.has(entry.id) }
  const edge = (from: string, to: string, rule: string | null, fact: string | null) => {
    const key = `${from}→${to}`
    const current = edges.get(key) ?? { from, to, rules: [], facts: [] }
    if (rule !== null && !current.rules.includes(rule)) current.rules.push(rule)
    if (fact !== null && !current.facts.includes(fact)) current.facts.push(fact)
    edges.set(key, current)
  }
  const visit = (target: string, document: SourceLineage, hop: number, seen: ReadonlySet<string>) => {
    if (hop > hops) { further = true; return }
    for (const [fact, field] of Object.entries(document.fieldProvenance)) {
      const source = sourceKey(field.source.system, field.source.record)
      if (node({ id: source, kind: "source", title: systemName(field.source.system), detail: field.source.record, hop })) edge(source, target, field.transform ?? null, fact)
      for (const upstream of field.upstream ?? []) {
        const id = factKey(upstream)
        if (seen.has(id)) continue
        if (hop + 1 > hops) { further = true; continue }
        if (!node({ id, kind: "fact", title: upstream.key, detail: `${upstream.objectType} · version ${upstream.version}`, hop: hop + 1, ref: upstream })) continue
        edge(id, target, field.transform ?? null, fact)
        const read = lineages.get(id)
        if (read === undefined) { if (hop + 2 <= hops) unread.push(upstream); else further = true; continue }
        if (read !== null) visit(id, read, hop + 2, new Set([...seen, id]))
      }
    }
    const { system, record } = document.documentRef
    const sources = [...nodes.values()].filter((entry) => entry.kind === "source" && entry.hop === hop && edges.has(`${entry.id}→${target}`))
    if (hop + 1 > hops) { if (sources.length > 0) further = true; return }
    const doc = documentKey(system, record)
    if (sources.length > 0 && node({ id: doc, kind: "document", title: `${systemName(system)} document`, detail: record, hop: hop + 1 })) {
      for (const source of sources) edge(doc, source.id, null, null)
    }
  }
  node({ id: factKey(root), kind: "record", title: root.key, detail: `${root.objectType} · version ${root.version}`, hop: 0, ref: root })
  visit(factKey(root), lineage, 1, new Set([factKey(root)]))
  return { nodes: [...nodes.values()], edges: [...edges.values()].filter(({ from, to }) => nodes.has(from) && nodes.has(to)), further: further || nodes.size >= NODE_CAP, unread }
}
