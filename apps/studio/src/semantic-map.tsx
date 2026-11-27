import { useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { ArrowLeft, ArrowRight, Database, ExternalLink, Map as MapIcon, Search, Waypoints, X } from "lucide-react"
import { useEffect, useMemo, useRef, useState } from "react"
import { LoadState } from "@/components/load-state"
import { PageHeader } from "@/components/panel"
import { Panel, Well } from "@/components/system/panel"
import { StatStrip, KpiTile } from "@/components/system/kpi-tile"
import { StatusDot, type DotTone } from "@/components/system/status-dot"
import { ListRow, RowList } from "@/components/system/list-row"
import { SourceTag } from "@/components/system/source-tag"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { cn } from "@/lib/utils"
import { lawHeadline } from "./lib/catalog.ts"
import { retryWorkspace, useMode } from "./lib/live-transport.ts"
import { useCatalog } from "./lib/use-catalog.ts"
import {
  SEMANTIC_LENSES,
  buildSemanticMap,
  islandMatchesLens,
  nodeMatchesLens,
  nodeMatchesQuery,
  provenanceForNode,
  type MapAttentionTone,
  type NodeProvenanceDossier,
  type SemanticBridge,
  type SemanticIsland,
  type SemanticLens,
  type SemanticMapAttentionInput,
  type SemanticNode,
} from "./lib/semantic-map.ts"
import { useInbox } from "./inbox.tsx"

/** The dot a map attention takes: colour only for live attention (DESIGN.md 17); unknown or recorded stays quiet. */
const ATTENTION_DOT: Readonly<Record<MapAttentionTone, DotTone>> = { critical: "bad", warning: "held", none: "live", unknown: "quiet" }

/** Colour strictly reserved for verified live attention status dots — unknown/recorded stays monochrome. */
function AttentionDot({ tone, label }: { readonly tone: MapAttentionTone; readonly label: string }) {
  return <StatusDot tone={ATTENTION_DOT[tone]}>{label}</StatusDot>
}

/** A count as the inbox reported it: `20+` when the inbox capped or bounded what it returned. */
const atLeast = (count: number, approximate: boolean): string => `${count}${approximate ? "+" : ""}`

/** A type's rule holdings and sync errors, each a red dot with its count and noun; nothing when it has neither. */
function AttentionWords({ node }: { readonly node: SemanticNode }) {
  return (
    <>
      {node.ruleHoldings > 0 && (
        <StatusDot tone="bad" className="tabular-nums">
          {atLeast(node.ruleHoldings, node.ruleHoldingsApproximate)} {node.ruleHoldings === 1 && !node.ruleHoldingsApproximate ? "holding" : "holdings"}
        </StatusDot>
      )}
      {node.sourceErrors > 0 && (
        <StatusDot tone="bad" className="tabular-nums">
          {atLeast(node.sourceErrors, node.sourceErrorsApproximate)} sync {node.sourceErrors === 1 && !node.sourceErrorsApproximate ? "error" : "errors"}
        </StatusDot>
      )}
    </>
  )
}

/** A node's or island's attention in words, for when no count badge says it. */
function attentionWords(tone: MapAttentionTone): string {
  return tone === "none" ? "No open alerts" : tone === "unknown" ? "Alerts not known" : "Has alerts"
}

/** The cards outside the active lens or search: only their frame fades, never their text, ids or focus rings. */
const OUTSIDE_LENS = "border-dashed bg-transparent shadow-none"

/** A row outside the active lens or search: a dashed hairline around it, its text and focus ring untouched. */
const OUTSIDE_LENS_ROW = "rounded-(--radius-row) outline-1 outline-dashed -outline-offset-1 outline-line"

/** Arrows that point along the reading direction flip in right-to-left text. */
const FLOW_ARROW = "rtl:-scale-x-100"

/** A quiet section label (step 2, muted, sentence case) with its count as a muted metric. */
function SectionLabel({ children, count }: { readonly children: string; readonly count?: number }) {
  return (
    <h3 className="flex items-baseline gap-1.5 text-body text-muted-foreground">
      {children}{count !== undefined && <span className="text-meta tabular-nums">{count}</span>}
    </h3>
  )
}

function islandStatusLabel(island: SemanticIsland, attentionStatus: SemanticMapAttentionInput["kind"]): string {
  if (attentionStatus === "recorded") return "Recorded schema"
  if (attentionStatus === "loading") return "Loading alerts…"
  if (attentionStatus === "unavailable") return "Alerts unavailable"
  const holdings = atLeast(island.ruleHoldings, island.ruleHoldingsApproximate)
  const sync = atLeast(island.sourceErrors, island.sourceErrorsApproximate)
  if (island.ruleHoldings > 0 && island.sourceErrors > 0) {
    return `${holdings} rule holdings · ${sync} sync`
  }
  if (island.ruleHoldings > 0) {
    return `${holdings} rule ${island.ruleHoldings === 1 ? "holding" : "holdings"}`
  }
  if (island.sourceErrors > 0) {
    return `${sync} sync ${island.sourceErrors === 1 && !island.sourceErrorsApproximate ? "error" : "errors"}`
  }
  if (island.attention === "unknown") return "None in returned slice"
  return "No open alerts"
}

/** Zoom 0 — Monochrome structural Island card (a ShellCard) with honest attention words and bridge count. */
function IslandCard({
  island,
  attentionStatus,
  bridgeCount,
  dimmed,
  onOpen,
  onSelectNode,
}: {
  readonly island: SemanticIsland
  readonly attentionStatus: SemanticMapAttentionInput["kind"]
  readonly bridgeCount: number
  readonly dimmed: boolean
  readonly onOpen: () => void
  readonly onSelectNode: (nodeId: string) => void
}) {
  const preview = island.nodes.slice(0, 4)
  const overflow = island.nodes.length - preview.length
  const statusText = islandStatusLabel(island, attentionStatus)

  return (
    <article
      data-slot="semantic-island"
      data-island-id={island.id}
      data-outside-lens={dimmed || undefined}
      className={cn(
        "flex flex-col rounded-(--radius-panel) border border-(--panel-border) bg-card p-(--panel-inset) shadow-(--panel-shadow) transition-colors",
        dimmed && OUTSIDE_LENS,
      )}
    >
      <div className="flex items-start justify-between gap-3 px-3 pt-1.5 pb-2.5">
        <div className="min-w-0">
          <div className="flex items-baseline gap-2">
            <h3 className="truncate text-body text-foreground">{island.title}</h3>
            {dimmed && <span className="sr-only">(outside the current lens)</span>}
            <span className="text-meta text-muted-foreground tabular-nums" title={`${island.nodes.length} object ${island.nodes.length === 1 ? "type" : "types"}`}>
              {island.nodes.length}
            </span>
          </div>
          <p className="mt-0.5 truncate text-meta text-muted-foreground">{island.subtitle}</p>
        </div>
        <AttentionDot tone={island.attention} label={statusText} />
      </div>

      <Well className="flex flex-1 flex-col rounded-(--radius-well) border">
        <RowList className="flex-1">
          {preview.map((node) => (
            <li key={node.id}>
              {/* No lead dot: a type's attention is the end slot's dot and its word, never a colour on its own. */}
              <ListRow
                onClick={() => onSelectNode(node.id)}
                title={node.title}
                titleLang={node.titleLang}
                end={(
                  <>
                    <span className="text-meta">{node.fieldCount} facts{node.lawCount > 0 ? ` · ${node.lawCount} ${node.lawCount === 1 ? "law" : "laws"}` : ""}</span>
                    <AttentionWords node={node} />
                  </>
                )}
              />
            </li>
          ))}
          {overflow > 0 && (
            <li>
              <ListRow
                onClick={onOpen}
                title={<span className="text-meta text-muted-foreground">+{overflow} more object {overflow === 1 ? "type" : "types"}</span>}
                end={<ArrowRight aria-hidden className={cn("size-3.5", FLOW_ARROW)} />}
              />
            </li>
          )}
        </RowList>

        <footer className="flex items-center justify-between gap-2 border-t border-(--well-border) py-1.5 ps-4 pe-2 text-meta text-muted-foreground">
          <span className="tabular-nums">
            {island.lawCount} {island.lawCount === 1 ? "law" : "laws"} · {bridgeCount} {bridgeCount === 1 ? "bridge" : "bridges"}
          </span>
          <Button type="button" variant="ghost" size="xs" onClick={onOpen} data-island-trigger="" className="max-sm:min-h-[44px]">
            Expand island<ArrowRight aria-hidden className={cn("size-3.5", FLOW_ARROW)} />
          </Button>
        </footer>
      </Well>
    </article>
  )
}

/** Zoom 1 — Expanded Island view with its object types as rows and compact stubs for cross-island neighbors. */
function IslandExpansion({
  island,
  bridges,
  islandsById,
  lens,
  query,
  selectedNodeId,
  onBack,
  onSelectNode,
  onJumpIsland,
}: {
  readonly island: SemanticIsland
  readonly bridges: readonly SemanticBridge[]
  readonly islandsById: ReadonlyMap<string, SemanticIsland>
  readonly lens: SemanticLens
  readonly query: string
  readonly selectedNodeId: string | null
  readonly onBack: () => void
  readonly onSelectNode: (nodeId: string) => void
  readonly onJumpIsland: (islandId: string) => void
}) {
  const heading = useRef<HTMLHeadingElement>(null)
  // Drilling in (or jumping to a neighbour) moves focus to the island's heading, so keyboard and
  // screen-reader users land on what just opened instead of on a control that is gone.
  useEffect(() => { heading.current?.focus() }, [island.id])
  const connectedBridges = bridges.filter((bridge) => bridge.fromIsland === island.id || bridge.toIsland === island.id)
  const ghostNeighbors = new Map<string, { island: SemanticIsland; linkCount: number; labels: string[] }>()
  for (const bridge of connectedBridges) {
    const otherId = bridge.fromIsland === island.id ? bridge.toIsland : bridge.fromIsland
    const other = islandsById.get(otherId)
    if (!other) continue
    const prev = ghostNeighbors.get(otherId) ?? { island: other, linkCount: 0, labels: [] }
    prev.linkCount += bridge.count
    for (const edge of bridge.edges.slice(0, 3)) prev.labels.push(edge.relationshipId.replaceAll("_", " "))
    ghostNeighbors.set(otherId, prev)
  }

  return (
    <div data-slot="island-expansion" data-island-id={island.id} className="space-y-5">
      <div className="flex flex-wrap items-center gap-3">
        <Button type="button" variant="outline" size="sm" onClick={onBack} className="max-sm:min-h-[44px]">
          <ArrowLeft aria-hidden className={cn("size-3.5", FLOW_ARROW)} />All islands
        </Button>
        <div className="min-w-0">
          <h2 ref={heading} tabIndex={-1} className="text-heading font-medium text-foreground outline-none">{island.title}</h2>
          <p className="text-meta text-muted-foreground">
            {island.subtitle} · {island.nodes.length} object {island.nodes.length === 1 ? "type" : "types"} · {island.lawCount} {island.lawCount === 1 ? "law" : "laws"}
          </p>
        </div>
      </div>

      {island.sourceErrors > 0 && (
        <p role="status" className="text-body">
          <StatusDot tone="bad" className="text-body whitespace-normal">
            {atLeast(island.sourceErrors, island.sourceErrorsApproximate)} source sync {island.sourceErrors === 1 && !island.sourceErrorsApproximate ? "error reported" : "errors reported"} for this island.
          </StatusDot>
        </p>
      )}

      <Panel icon={MapIcon} label="Object types" action={`${island.nodes.length}`}>
        <Well>
          <RowList>
            {island.nodes.map((node) => {
              const active = selectedNodeId === node.id
              const matched = nodeMatchesLens(node, lens) && nodeMatchesQuery(node, query)
              const facts = [
                `${node.fieldCount} facts`,
                `${node.relationshipCount} links`,
                ...(node.lawCount > 0 ? [`${node.lawCount} ${node.lawCount === 1 ? "law" : "laws"}`] : []),
                ...(node.markingCount > 0 ? [`${node.markingCount} ${node.markingCount === 1 ? "marking" : "markings"}`] : []),
              ].join(" · ")
              return (
                <li
                  key={node.id}
                  data-slot="semantic-node"
                  data-node-id={node.id}
                  data-outside-lens={!matched || undefined}
                  className={cn(!matched && OUTSIDE_LENS_ROW)}
                >
                  <ListRow
                    aria-pressed={active}
                    selected={active}
                    onClick={() => onSelectNode(node.id)}
                    title={node.title}
                    titleLang={node.titleLang}
                    code={node.id}
                    description={(
                      <>
                        {node.summary && <span className="line-clamp-2">{node.summary}</span>}
                        <span className="block tabular-nums">{facts}</span>
                        {!matched && <span className="sr-only">Outside the current lens.</span>}
                      </>
                    )}
                    end={node.ruleHoldings > 0 || node.sourceErrors > 0 ? (
                      <AttentionWords node={node} />
                    ) : (
                      <span data-slot="node-status">
                        <AttentionDot tone={node.attention} label={attentionWords(node.attention)} />
                      </span>
                    )}
                  />
                </li>
              )
            })}
          </RowList>
        </Well>
      </Panel>

      {ghostNeighbors.size > 0 && (
        <Panel icon={Waypoints} label="Cross-island neighbors" action={`${ghostNeighbors.size}`}>
          <Well>
            <RowList>
              {[...ghostNeighbors.values()].map(({ island: other, linkCount, labels }) => (
                <li key={other.id}>
                  <ListRow
                    onClick={() => onJumpIsland(other.id)}
                    title={other.title}
                    description={<span className="block truncate">{linkCount} {linkCount === 1 ? "link" : "links"} · {labels.slice(0, 2).join(", ")}</span>}
                    end={<ArrowRight aria-hidden className={cn("size-4", FLOW_ARROW)} />}
                  />
                </li>
              ))}
            </RowList>
          </Well>
        </Panel>
      )}
    </div>
  )
}

/** Zoom 2 — Foundry-style Schema & Provenance Side Sheet for the selected object type. */
function ProvenanceSideSheet({
  dossier,
  onClose,
  onSelectNode,
}: {
  readonly dossier: NodeProvenanceDossier
  readonly onClose: () => void
  readonly onSelectNode: (nodeId: string) => void
}) {
  const { node, island, incoming, outgoing, laws, sourceFeeds } = dossier
  // Each section: a quiet label, then one well of rows (DESIGN.md 10, Section).
  const well = "rounded-(--radius-well) border"
  return (
    <aside
      data-slot="provenance-sheet"
      aria-label={`Provenance for ${node.title}`}
      className="flex flex-col gap-4 rounded-(--radius-panel) border border-(--panel-border) bg-card p-4 shadow-(--panel-shadow)"
    >
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <Link
            to="/sources/$concept"
            params={{ concept: node.id }}
            className="inline-flex max-w-full items-center gap-1.5 rounded-(--radius-chip) px-1.5 py-0.5 font-mono text-meta text-muted-foreground ring-1 ring-line ring-inset hover:text-foreground max-sm:min-h-[44px]"
            title="Inspect admitted definition artifact"
          >
            <bdi className="truncate">{node.artifactId}</bdi>
            <ExternalLink aria-hidden className="size-3 shrink-0" />
          </Link>
          <h2 className="mt-1.5 truncate text-heading font-medium text-foreground" lang={node.titleLang}>{node.title}</h2>
          <p className="text-meta text-muted-foreground">Island: {island.title}</p>
        </div>
        <Button type="button" variant="ghost" size="icon-sm" aria-label="Close provenance sheet" onClick={onClose} className="max-sm:size-[44px]">
          <X aria-hidden className="size-4" />
        </Button>
      </div>

      {node.summary && <p className="text-body text-muted-foreground">{node.summary}</p>}

      <div className="flex flex-wrap gap-2">
        <Button asChild size="sm" variant="default" className="max-sm:min-h-[44px]">
          <Link to="/objects/$type" params={{ type: node.id }}>
            Open records<ExternalLink aria-hidden className="size-3.5" />
          </Link>
        </Button>
        <Button asChild size="sm" variant="outline" className="max-sm:min-h-[44px]">
          <Link to="/sources/$concept" params={{ concept: node.id }}>
            Definition
          </Link>
        </Button>
      </div>

      {node.ruleHoldings > 0 && (
        <p className="text-body">
          <StatusDot tone="bad" className="text-body whitespace-normal">
            {atLeast(node.ruleHoldings, node.ruleHoldingsApproximate)} rule {node.ruleHoldings === 1 && !node.ruleHoldingsApproximate ? "holding" : "holdings"} reported in inbox.
          </StatusDot>
        </p>
      )}

      <section className="space-y-1.5">
        <SectionLabel count={sourceFeeds.length}>Source feeds</SectionLabel>
        {sourceFeeds.length === 0 ? (
          <p className="text-meta text-muted-foreground">No external source feed mapped.</p>
        ) : (
          <Well className={well}>
            <RowList>
              {sourceFeeds.map((feed) => (
                <li key={`${feed.mappingId ?? feed.system}:${feed.dataset}`}>
                  <ListRow passive title={`${feed.system} · ${feed.dataset}`}
                    end={<span className="text-meta">{feed.mappedFields} mapped{feed.gapCount > 0 ? ` · ${feed.gapCount} ${feed.gapCount === 1 ? "gap" : "gaps"}` : ""}</span>} />
                </li>
              ))}
            </RowList>
          </Well>
        )}
      </section>

      <section className="space-y-1.5">
        <SectionLabel count={laws.length}>Governing laws</SectionLabel>
        {laws.length === 0 ? (
          <p className="text-meta text-muted-foreground">No admitted laws scoped to this type.</p>
        ) : (
          <Well className={well}>
            <RowList>
              {laws.slice(0, 6).map((law) => (
                <li key={law.id}>
                  <ListRow passive title={<span className="whitespace-normal">{lawHeadline(law)}</span>}
                    description={law.inheritedFrom ? `Inherited from ${law.inheritedFrom}` : undefined}
                    end={<StatusDot tone={law.severity === "error" ? "bad" : "held"}>{law.severity}</StatusDot>} />
                </li>
              ))}
            </RowList>
          </Well>
        )}
      </section>

      {node.markings.length > 0 && (
        <section className="space-y-1.5">
          <SectionLabel>Markings</SectionLabel>
          <div className="flex flex-wrap gap-1.5">
            {node.markings.map((marking) => <SourceTag key={marking}>{marking}</SourceTag>)}
          </div>
        </section>
      )}

      <section className="space-y-1.5">
        <SectionLabel count={incoming.length + outgoing.length}>Connected types</SectionLabel>
        {incoming.length + outgoing.length === 0 ? (
          <p className="text-meta text-muted-foreground">No declared relationships.</p>
        ) : (
          <Well className={well}>
            <RowList>
              {outgoing.map((edge) => (
                <li key={edge.id}>
                  <ListRow onClick={() => onSelectNode(edge.targetConcept)} title={edge.targetTitle}
                    description={`→ ${edge.relationshipId.replaceAll("_", " ")}`} />
                </li>
              ))}
              {incoming.map((edge) => (
                <li key={`in:${edge.id}`}>
                  <ListRow onClick={() => onSelectNode(edge.sourceConcept)} title={edge.sourceTitle}
                    description={`← ${edge.relationshipId.replaceAll("_", " ")}`} />
                </li>
              ))}
            </RowList>
          </Well>
        )}
      </section>
    </aside>
  )
}

/** What opened an island: its own "Expand island" trigger, or one end of a bridge in the bridges list. */
type Opener = { readonly kind: "island"; readonly id: string } | { readonly kind: "bridge"; readonly id: string; readonly end: "from" | "to" }

const openerSelector = (opener: Opener): string => opener.kind === "island"
  ? `[data-slot="semantic-island"][data-island-id="${CSS.escape(opener.id)}"] [data-island-trigger]`
  : `[data-bridge-id="${CSS.escape(opener.id)}"][data-bridge-end="${opener.end}"]`

/** `/map`: Read-only Catalog Ontology Map (Phase 1, #1664) — Monochrome Islands, Bundled Bridges, Lenses & Schema Dossier. */
export function SemanticMapPage() {
  const queries = useQueryClient()
  const { read, catalog } = useCatalog()
  const mode = useMode()
  const inbox = useInbox()
  const [lens, setLens] = useState<SemanticLens>("all")
  const [query, setQuery] = useState("")
  const [openIslandId, setOpenIslandId] = useState<string | null>(null)
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null)
  // What opened the island (its own trigger, or a bridge's end in the bridges list): Back returns
  // focus there on the overview. A jump to another island from inside opens that island in its own
  // right, so Back then returns to that island's trigger.
  const [opener, setOpener] = useState<Opener | null>(null)
  const [returnTo, setReturnTo] = useState<Opener | null>(null)
  const openIsland = (id: string, from: Opener = { kind: "island", id }) => { setOpenIslandId(id); setOpener(from) }
  useEffect(() => {
    if (openIslandId !== null || returnTo === null) return
    document.querySelector<HTMLElement>(openerSelector(returnTo))?.focus()
    setReturnTo(null)
  }, [openIslandId, returnTo])

  const attentionInput: SemanticMapAttentionInput = useMemo(() => {
    if (mode.data?.kind !== "live") return { kind: "recorded" }
    if (inbox.isPending) return { kind: "loading" }
    if (inbox.isError || !inbox.data?.document) return { kind: "unavailable" }
    return { kind: "live", inbox: inbox.data.document }
  }, [mode.data?.kind, inbox.isPending, inbox.isError, inbox.data])

  const projection = useMemo(
    () => (catalog ? buildSemanticMap(catalog, attentionInput) : undefined),
    [catalog, attentionInput],
  )

  const activeIsland = openIslandId ? projection?.islandsById.get(openIslandId) ?? null : null
  const dossier = selectedNodeId && projection ? provenanceForNode(projection, selectedNodeId) : null

  const selectNode = (nodeId: string) => {
    setSelectedNodeId(nodeId)
    const node = projection?.nodesById.get(nodeId)
    if (node && openIslandId && node.islandId !== openIslandId) openIsland(node.islandId)
  }

  return (
    <section className="space-y-6">
      <PageHeader
        title="Semantic map"
        description="Admitted workspace definitions grouped into monochrome structural islands with bundled cross-island bridges and schema provenance."
        actions={
          <Button asChild variant="outline" size="sm" className="max-sm:min-h-[44px]">
            <Link to="/sources"><Database aria-hidden className="size-3.5" />Sources & types</Link>
          </Button>
        }
      />

      <LoadState
        status={read.status}
        data={projection}
        error={read.error}
        isFetching={read.isFetching}
        // Route pending/refusal Retry through retryWorkspace so a wedged mode probe or unavailable
        // cell recovers in one click without refetching against a dead cell (#1774).
        onRetry={() => void retryWorkspace(queries).then((ok) => { if (ok) void read.refetch({ cancelRefetch: false }) })}
        bone="semantic-map"
        skeleton="cards"
        loadingLabel="Building semantic map"
        isEmpty={(map) => map.islands.length === 0}
        empty={{
          icon: MapIcon,
          title: "No object types to map yet",
          description: "Connect a source or install a model to explore its islands, bridges, and provenance.",
          next: <Button asChild size="sm" variant="outline"><Link to="/sources">Open sources</Link></Button>,
        }}
      >
        {(map) => {
          const alerts = map.totals.alerts
          const alertTotal = alerts ? alerts.ruleHoldings + alerts.sourceErrors : null
          const alertApprox = alerts ? (alerts.ruleHoldingsApproximate || alerts.sourceErrorsApproximate) : false
          const alertValue = alertTotal === null ? "—" : `${alertTotal}${alertApprox ? "+" : ""}`
          const alertSub = map.attentionStatus === "recorded"
            ? "live alerts require a connected cell"
            : map.attentionStatus === "loading"
              ? "loading live inbox alerts…"
              : map.attentionStatus === "unavailable"
                ? "live inbox alerts unavailable"
                : alertTotal && alertTotal > 0
                  ? `${alerts!.ruleHoldings}${alerts!.ruleHoldingsApproximate ? "+" : ""} rule holdings · ${alerts!.sourceErrors}${alerts!.sourceErrorsApproximate ? "+" : ""} sync`
                  : alerts?.bounded
                    ? "none reported within inbox scan bounds"
                    : "no open alerts reported"

          return (
            <>
              <StatStrip>
                <KpiTile label="Islands" value={map.totals.islands} sub={`${map.totals.bridges} cross-island bridges`} />
                <KpiTile label="Object types" value={map.totals.objectTypes} sub={`${map.totals.links} declared links`} />
                <KpiTile label="Admitted laws" value={map.totals.laws} sub="scoped across object types" />
                <KpiTile label="Inbox alerts" value={alertValue} sub={alertSub} />
              </StatStrip>

              <div className="flex flex-wrap items-center gap-3">
                <ToggleGroup
                  type="single"
                  value={lens}
                  onValueChange={(value) => { if (value) setLens(value as SemanticLens) }}
                  aria-label="Semantic lens"
                >
                  {SEMANTIC_LENSES.map((item) => (
                    <ToggleGroupItem key={item.id} value={item.id} title={item.detail}>
                      {item.label}
                    </ToggleGroupItem>
                  ))}
                </ToggleGroup>

                <label className="relative ms-auto block w-full max-w-80">
                  <span className="sr-only">Filter object types, feeds, or markings</span>
                  <Search aria-hidden className="pointer-events-none absolute start-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    type="search"
                    placeholder="Find types, feeds, markings…"
                    value={query}
                    onChange={(event) => setQuery(event.target.value)}
                    className="h-8 ps-8 max-sm:min-h-[44px]"
                  />
                </label>
              </div>

              <div className={cn("grid gap-5", dossier && "xl:grid-cols-[minmax(0,1fr)_22rem] xl:items-start")}>
                <div className="space-y-6">
                  {activeIsland ? (
                    <IslandExpansion
                      island={activeIsland}
                      bridges={map.bridges}
                      islandsById={map.islandsById}
                      lens={lens}
                      query={query}
                      selectedNodeId={selectedNodeId}
                      onBack={() => { setReturnTo(opener ?? { kind: "island", id: activeIsland.id }); setOpenIslandId(null) }}
                      onSelectNode={selectNode}
                      onJumpIsland={(id) => { openIsland(id); setSelectedNodeId(null) }}
                    />
                  ) : (
                    <>
                      <div className="grid gap-4 md:grid-cols-2">
                        {map.islands.map((island) => {
                          const bridgeCount = map.bridges.filter((b) => b.fromIsland === island.id || b.toIsland === island.id).length
                          const dimmed = (lens !== "all" || query.trim() !== "") && !islandMatchesLens(island, lens, query)
                          return (
                            <IslandCard
                              key={island.id}
                              island={island}
                              attentionStatus={map.attentionStatus}
                              bridgeCount={bridgeCount}
                              dimmed={dimmed}
                              onOpen={() => openIsland(island.id)}
                              onSelectNode={selectNode}
                            />
                          )
                        })}
                      </div>

                      {map.bridges.length > 0 && (
                        <Panel icon={Waypoints} label="Bundled cross-island bridges" action={`${map.bridges.length} total`}>
                          <Well>
                            <RowList>
                            {map.bridges.map((bridge) => {
                              const from = map.islandsById.get(bridge.fromIsland)
                              const to = map.islandsById.get(bridge.toIsland)
                              // A row shaped like a ListRow, but a plain one: each end of the bridge is its own button.
                              return (
                                <li key={bridge.id} className="flex min-h-12 flex-wrap items-center gap-x-3 gap-y-1 rounded-(--radius-row) px-3 py-1.5 text-body max-sm:min-h-[44px]">
                                  <button
                                    type="button"
                                    data-bridge-id={bridge.id}
                                    data-bridge-end="from"
                                    onClick={() => openIsland(bridge.fromIsland, { kind: "bridge", id: bridge.id, end: "from" })}
                                    className="rounded-sm text-foreground underline-offset-4 outline-none hover:underline focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-ring max-sm:min-h-[44px]"
                                  >
                                    {from?.title ?? bridge.fromIsland}
                                  </button>
                                  <ArrowRight aria-hidden className={cn("size-3.5 text-muted-foreground", FLOW_ARROW)} />
                                  <button
                                    type="button"
                                    data-bridge-id={bridge.id}
                                    data-bridge-end="to"
                                    onClick={() => openIsland(bridge.toIsland, { kind: "bridge", id: bridge.id, end: "to" })}
                                    className="rounded-sm text-foreground underline-offset-4 outline-none hover:underline focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-ring max-sm:min-h-[44px]"
                                  >
                                    {to?.title ?? bridge.toIsland}
                                  </button>
                                  <span className="truncate text-meta text-muted-foreground">
                                    {bridge.edges.slice(0, 3).map((edge) => edge.relationshipId.replaceAll("_", " ")).join(", ")}
                                    {bridge.edges.length > 3 ? ` +${bridge.edges.length - 3}` : ""}
                                  </span>
                                  <span className="ms-auto text-body text-muted-foreground tabular-nums">
                                    {bridge.count} {bridge.count === 1 ? "link" : "links"}
                                  </span>
                                </li>
                              )
                            })}
                            </RowList>
                          </Well>
                        </Panel>
                      )}
                    </>
                  )}
                </div>

                {dossier && (
                  <ProvenanceSideSheet
                    dossier={dossier}
                    onClose={() => setSelectedNodeId(null)}
                    onSelectNode={selectNode}
                  />
                )}
              </div>
            </>
          )
        }}
      </LoadState>
    </section>
  )
}
