import { Link } from "@tanstack/react-router"
import { ArrowLeft, ArrowRight, Database, ExternalLink, Map as MapIcon, Search, ShieldAlert, Waypoints, X } from "lucide-react"
import { useEffect, useMemo, useRef, useState } from "react"
import { LoadState } from "@/components/load-state"
import { PageHeader, Panel, Well } from "@/components/panel"
import { StatStrip, KpiTile } from "@/components/kpi-tile"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { cn } from "@/lib/utils"
import { lawHeadline } from "./lib/catalog.ts"
import { refreshMode, useMode } from "./lib/live-transport.ts"
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
} from "./lib/semantic-map.ts"
import { useInbox } from "./inbox.tsx"

/** Colour strictly reserved for verified live attention status dots — unknown/recorded stays monochrome. */
function AttentionDot({ tone, label }: { readonly tone: MapAttentionTone; readonly label: string }) {
  const dotClass = tone === "critical"
    ? "bg-destructive"
    : tone === "warning"
      ? "bg-warning"
      : tone === "none"
        ? "bg-success"
        : "bg-muted-foreground/45"
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
      <span aria-hidden className={cn("size-2 shrink-0 rounded-full", dotClass)} />
      <span>{label}</span>
    </span>
  )
}

/** A count as the inbox reported it: `20+` when the inbox capped or bounded what it returned. */
const atLeast = (count: number, approximate: boolean): string => `${count}${approximate ? "+" : ""}`

/** A node's or island's attention in words, for when no count badge says it. */
function attentionWords(tone: MapAttentionTone): string {
  return tone === "none" ? "No open alerts" : tone === "unknown" ? "Alerts not known" : "Has alerts"
}

/** The cards outside the active lens or search: only their frame fades, never their text, ids or focus rings. */
const OUTSIDE_LENS = "border-dashed bg-transparent shadow-none"

/** Arrows that point along the reading direction flip in right-to-left text. */
const FLOW_ARROW = "rtl:-scale-x-100"

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

/** Zoom 0 — Monochrome structural Island card with honest attention badges and bridge count. */
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
        "flex flex-col rounded-xl border border-panel-border bg-card transition-colors",
        dimmed && OUTSIDE_LENS,
      )}
    >
      <div className="flex items-start justify-between gap-3 border-b border-border/80 px-4 py-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <h3 className="truncate text-panel font-medium text-foreground">{island.title}</h3>
            {dimmed && <span className="sr-only">(outside the current lens)</span>}
            <span className="rounded-md bg-muted px-1.5 py-0.5 text-[0.6875rem] text-muted-foreground tabular-nums">
              {island.nodes.length}
            </span>
          </div>
          <p className="mt-0.5 truncate text-xs text-muted-foreground">{island.subtitle}</p>
        </div>
        <AttentionDot tone={island.attention} label={statusText} />
      </div>

      <div className="flex-1 divide-y divide-border/60 px-2 py-1">
        {preview.map((node) => (
          <button
            key={node.id}
            type="button"
            onClick={() => onSelectNode(node.id)}
            className="flex w-full items-center gap-2.5 rounded-lg px-2 py-2 text-start text-sm transition-colors outline-none hover:bg-muted/70 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring"
          >
            <span
              aria-hidden
              className={cn(
                "size-1.5 shrink-0 rounded-full",
                node.attention === "critical" ? "bg-destructive" : "bg-foreground/35",
              )}
            />
            <span className="min-w-0 flex-1 truncate font-medium text-foreground" lang={node.titleLang}>
              {node.title}
            </span>
            <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
              {node.fieldCount} facts
            </span>
            {node.lawCount > 0 && (
              <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
                · {node.lawCount} {node.lawCount === 1 ? "law" : "laws"}
              </span>
            )}
            {node.ruleHoldings > 0 && (
              <Badge variant="destructive" className="shrink-0 px-1.5 py-0 text-[0.6875rem] tabular-nums">
                {atLeast(node.ruleHoldings, node.ruleHoldingsApproximate)}
              </Badge>
            )}
          </button>
        ))}
        {overflow > 0 && (
          <button
            type="button"
            onClick={onOpen}
            className="flex w-full items-center justify-between rounded-lg px-2 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
          >
            <span>+{overflow} more object {overflow === 1 ? "type" : "types"}</span>
            <ArrowRight aria-hidden className={cn("size-3.5", FLOW_ARROW)} />
          </button>
        )}
      </div>

      <footer className="flex items-center justify-between gap-2 border-t border-border/80 px-4 py-2.5 text-xs text-muted-foreground">
        <span className="tabular-nums">
          {island.lawCount} {island.lawCount === 1 ? "law" : "laws"} · {bridgeCount} {bridgeCount === 1 ? "bridge" : "bridges"}
        </span>
        <Button type="button" variant="ghost" size="xs" onClick={onOpen} data-island-trigger="">
          Expand island<ArrowRight aria-hidden className={cn("size-3.5", FLOW_ARROW)} />
        </Button>
      </footer>
    </article>
  )
}

/** Zoom 1 — Expanded Island view with object cards and compact ghost stubs for cross-island neighbors. */
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
        <Button type="button" variant="outline" size="sm" onClick={onBack}>
          <ArrowLeft aria-hidden className={cn("size-3.5", FLOW_ARROW)} />All islands
        </Button>
        <div className="min-w-0">
          <h2 ref={heading} tabIndex={-1} className="text-section font-medium text-foreground outline-none">{island.title}</h2>
          <p className="text-xs text-muted-foreground">
            {island.subtitle} · {island.nodes.length} object {island.nodes.length === 1 ? "type" : "types"} · {island.lawCount} {island.lawCount === 1 ? "law" : "laws"}
          </p>
        </div>
      </div>

      {island.sourceErrors > 0 && (
        <div role="status" className="flex items-center gap-2 rounded-xl border border-destructive/30 bg-destructive/10 px-3.5 py-2.5 text-xs text-destructive-text">
          <ShieldAlert aria-hidden className="size-4 shrink-0" />
          <span>{atLeast(island.sourceErrors, island.sourceErrorsApproximate)} source sync {island.sourceErrors === 1 && !island.sourceErrorsApproximate ? "error reported" : "errors reported"} for this island.</span>
        </div>
      )}

      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {island.nodes.map((node) => {
          const active = selectedNodeId === node.id
          const matched = nodeMatchesLens(node, lens) && nodeMatchesQuery(node, query)
          return (
            <button
              key={node.id}
              type="button"
              data-slot="semantic-node"
              data-node-id={node.id}
              aria-pressed={active}
              data-outside-lens={!matched || undefined}
              onClick={() => onSelectNode(node.id)}
              className={cn(
                "flex flex-col gap-2 rounded-xl border bg-card p-4 text-start transition-colors outline-none hover:bg-well-hover focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring",
                active ? "border-action ring-1 ring-action/25" : "border-border",
                !matched && OUTSIDE_LENS,
              )}
            >
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium text-foreground" lang={node.titleLang}>{node.title}</div>
                  <div className="truncate font-mono text-[0.6875rem] text-muted-foreground">{node.id}</div>
                  {!matched && <span className="sr-only">Outside the current lens.</span>}
                </div>
                {node.ruleHoldings > 0 || node.sourceErrors > 0 ? (
                  <div className="flex shrink-0 items-center gap-1">
                    {node.ruleHoldings > 0 && (
                      <Badge variant="destructive" className="tabular-nums">
                        {node.ruleHoldings}{node.ruleHoldingsApproximate ? "+" : ""} {node.ruleHoldings === 1 ? "holding" : "holdings"}
                      </Badge>
                    )}
                    {node.sourceErrors > 0 && (
                      <Badge variant="destructive" className="tabular-nums">
                        {atLeast(node.sourceErrors, node.sourceErrorsApproximate)} sync
                      </Badge>
                    )}
                  </div>
                ) : (
                  <span data-slot="node-status" className="mt-0.5 shrink-0">
                    <AttentionDot tone={node.attention} label={attentionWords(node.attention)} />
                  </span>
                )}
              </div>
              {node.summary && (
                <p className="line-clamp-2 text-xs text-muted-foreground">{node.summary}</p>
              )}
              <div className="mt-auto flex flex-wrap items-center gap-x-3 gap-y-1 pt-1 text-xs text-muted-foreground tabular-nums">
                <span>{node.fieldCount} facts</span>
                <span>{node.relationshipCount} links</span>
                {node.lawCount > 0 && <span>{node.lawCount} {node.lawCount === 1 ? "law" : "laws"}</span>}
                {node.markingCount > 0 && <span>{node.markingCount} {node.markingCount === 1 ? "marking" : "markings"}</span>}
              </div>
            </button>
          )
        })}
      </div>

      {ghostNeighbors.size > 0 && (
        <div className="space-y-2">
          <h3 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">Cross-island neighbors</h3>
          <div className="grid gap-2.5 sm:grid-cols-2 lg:grid-cols-3">
            {[...ghostNeighbors.values()].map(({ island: other, linkCount, labels }) => (
              <button
                key={other.id}
                type="button"
                onClick={() => onJumpIsland(other.id)}
                className="flex items-center justify-between gap-3 rounded-xl border border-dashed border-border bg-surface-subtle/60 px-3.5 py-2.5 text-start transition-colors hover:border-foreground/30 hover:bg-muted/60"
              >
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium text-foreground">{other.title}</div>
                  <div className="truncate text-xs text-muted-foreground">
                    {linkCount} {linkCount === 1 ? "link" : "links"} · {labels.slice(0, 2).join(", ")}
                  </div>
                </div>
                <ArrowRight aria-hidden className={cn("size-4 shrink-0 text-muted-foreground", FLOW_ARROW)} />
              </button>
            ))}
          </div>
        </div>
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
  return (
    <aside
      data-slot="provenance-sheet"
      aria-label={`Provenance for ${node.title}`}
      className="flex flex-col gap-4 rounded-xl border border-panel-border bg-card p-4"
    >
      <div className="flex items-start justify-between gap-2 border-b border-border/80 pb-3">
        <div className="min-w-0">
          <Link
            to="/sources/$concept"
            params={{ concept: node.id }}
            className="inline-flex max-w-full items-center gap-1.5 rounded bg-muted px-1.5 py-0.5 font-mono text-[0.6875rem] text-muted-foreground hover:text-foreground"
            title="Inspect admitted definition artifact"
          >
            <span className="truncate">{node.artifactId}</span>
            <ExternalLink aria-hidden className="size-3 shrink-0" />
          </Link>
          <h2 className="mt-1.5 truncate text-section font-medium text-foreground" lang={node.titleLang}>{node.title}</h2>
          <p className="text-xs text-muted-foreground">Island: {island.title}</p>
        </div>
        <Button type="button" variant="ghost" size="icon-sm" aria-label="Close provenance sheet" onClick={onClose}>
          <X aria-hidden className="size-4" />
        </Button>
      </div>

      {node.summary && <p className="text-sm text-muted-foreground">{node.summary}</p>}

      <div className="flex flex-wrap gap-2">
        <Button asChild size="sm" variant="default">
          <Link to="/objects/$type" params={{ type: node.id }}>
            Open records<ExternalLink aria-hidden className="size-3.5" />
          </Link>
        </Button>
        <Button asChild size="sm" variant="outline">
          <Link to="/sources/$concept" params={{ concept: node.id }}>
            Definition
          </Link>
        </Button>
      </div>

      {node.ruleHoldings > 0 && (
        <div className="flex items-center gap-2 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive-text">
          <ShieldAlert aria-hidden className="size-4 shrink-0" />
          <span>
            {atLeast(node.ruleHoldings, node.ruleHoldingsApproximate)} rule {node.ruleHoldings === 1 && !node.ruleHoldingsApproximate ? "holding" : "holdings"} reported in inbox.
          </span>
        </div>
      )}

      <section className="space-y-1.5">
        <h3 className="text-xs font-medium text-muted-foreground uppercase">Source feeds ({sourceFeeds.length})</h3>
        {sourceFeeds.length === 0 ? (
          <p className="text-xs text-muted-foreground">No external source feed mapped.</p>
        ) : (
          <ul className="space-y-1">
            {sourceFeeds.map((feed) => (
              <li key={`${feed.mappingId ?? feed.system}:${feed.dataset}`} className="flex items-center justify-between gap-2 rounded-lg bg-well px-2.5 py-1.5 text-xs">
                <span className="truncate font-medium text-foreground">{feed.system} · {feed.dataset}</span>
                <span className="shrink-0 text-muted-foreground tabular-nums">
                  {feed.mappedFields} mapped{feed.gapCount > 0 ? ` · ${feed.gapCount} ${feed.gapCount === 1 ? "gap" : "gaps"}` : ""}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-1.5">
        <h3 className="text-xs font-medium text-muted-foreground uppercase">Governing laws ({laws.length})</h3>
        {laws.length === 0 ? (
          <p className="text-xs text-muted-foreground">No admitted laws scoped to this type.</p>
        ) : (
          <ul className="space-y-1.5">
            {laws.slice(0, 6).map((law) => (
              <li key={law.id} className="rounded-lg border border-border/80 bg-well px-2.5 py-2 text-xs">
                <div className="flex items-start justify-between gap-2">
                  <span className="font-medium text-foreground">{lawHeadline(law)}</span>
                  <Badge variant={law.severity === "error" ? "destructive" : "outline"} className="shrink-0 text-[0.625rem]">
                    {law.severity}
                  </Badge>
                </div>
                {law.inheritedFrom && (
                  <p className="mt-0.5 text-[0.6875rem] text-muted-foreground">Inherited from {law.inheritedFrom}</p>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>

      {node.markings.length > 0 && (
        <section className="space-y-1.5">
          <h3 className="text-xs font-medium text-muted-foreground uppercase">Markings</h3>
          <div className="flex flex-wrap gap-1.5">
            {node.markings.map((marking) => (
              <Badge key={marking} variant="outline" className="font-mono text-[0.6875rem]">{marking}</Badge>
            ))}
          </div>
        </section>
      )}

      <section className="space-y-1.5">
        <h3 className="text-xs font-medium text-muted-foreground uppercase">Connected types ({incoming.length + outgoing.length})</h3>
        {incoming.length + outgoing.length === 0 ? (
          <p className="text-xs text-muted-foreground">No declared relationships.</p>
        ) : (
          <ul className="space-y-1">
            {outgoing.map((edge) => (
              <li key={edge.id}>
                <button
                  type="button"
                  onClick={() => onSelectNode(edge.targetConcept)}
                  className="flex w-full items-center justify-between gap-2 rounded-lg px-2 py-1.5 text-start text-xs transition-colors hover:bg-muted"
                >
                  <span className="truncate text-muted-foreground">→ {edge.relationshipId.replaceAll("_", " ")}</span>
                  <span className="truncate font-medium text-foreground">{edge.targetTitle}</span>
                </button>
              </li>
            ))}
            {incoming.map((edge) => (
              <li key={`in:${edge.id}`}>
                <button
                  type="button"
                  onClick={() => onSelectNode(edge.sourceConcept)}
                  className="flex w-full items-center justify-between gap-2 rounded-lg px-2 py-1.5 text-start text-xs transition-colors hover:bg-muted"
                >
                  <span className="truncate font-medium text-foreground">{edge.sourceTitle}</span>
                  <span className="truncate text-muted-foreground">← {edge.relationshipId.replaceAll("_", " ")}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>
    </aside>
  )
}

/** `/map`: Read-only Catalog Ontology Map (Phase 1, #1664) — Monochrome Islands, Bundled Bridges, Lenses & Schema Dossier. */
/** What opened an island: its own "Expand island" trigger, or one end of a bridge in the bridges list. */
type Opener = { readonly kind: "island"; readonly id: string } | { readonly kind: "bridge"; readonly id: string; readonly end: "from" | "to" }

const openerSelector = (opener: Opener): string => opener.kind === "island"
  ? `[data-slot="semantic-island"][data-island-id="${CSS.escape(opener.id)}"] [data-island-trigger]`
  : `[data-bridge-id="${CSS.escape(opener.id)}"][data-bridge-end="${opener.end}"]`

export function SemanticMapPage() {
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
          <Button asChild variant="outline" size="sm">
            <Link to="/sources"><Database aria-hidden className="size-3.5" />Sources & types</Link>
          </Button>
        }
      />

      <LoadState
        status={read.status}
        data={projection}
        error={read.error}
        isFetching={read.isFetching}
        onRetry={() => { refreshMode(); void read.refetch() }}
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

                <label className="relative ms-auto block w-full max-w-72">
                  <span className="sr-only">Filter object types, feeds, or markings</span>
                  <Search aria-hidden className="pointer-events-none absolute start-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    type="search"
                    placeholder="Find object types, feeds, markings…"
                    value={query}
                    onChange={(event) => setQuery(event.target.value)}
                    className="h-8 ps-8"
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
                          <Well className="divide-y divide-border/70">
                            {map.bridges.map((bridge) => {
                              const from = map.islandsById.get(bridge.fromIsland)
                              const to = map.islandsById.get(bridge.toIsland)
                              return (
                                <div key={bridge.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2.5 text-sm">
                                  <button
                                    type="button"
                                    data-bridge-id={bridge.id}
                                    data-bridge-end="from"
                                    onClick={() => openIsland(bridge.fromIsland, { kind: "bridge", id: bridge.id, end: "from" })}
                                    className="font-medium text-foreground underline-offset-4 hover:underline"
                                  >
                                    {from?.title ?? bridge.fromIsland}
                                  </button>
                                  <ArrowRight aria-hidden className={cn("size-3.5 text-muted-foreground", FLOW_ARROW)} />
                                  <button
                                    type="button"
                                    data-bridge-id={bridge.id}
                                    data-bridge-end="to"
                                    onClick={() => openIsland(bridge.toIsland, { kind: "bridge", id: bridge.id, end: "to" })}
                                    className="font-medium text-foreground underline-offset-4 hover:underline"
                                  >
                                    {to?.title ?? bridge.toIsland}
                                  </button>
                                  <span className="truncate text-xs text-muted-foreground">
                                    {bridge.edges.slice(0, 3).map((edge) => edge.relationshipId.replaceAll("_", " ")).join(", ")}
                                    {bridge.edges.length > 3 ? ` +${bridge.edges.length - 3}` : ""}
                                  </span>
                                  <Badge variant="outline" className="ms-auto tabular-nums">
                                    {bridge.count} {bridge.count === 1 ? "link" : "links"}
                                  </Badge>
                                </div>
                              )
                            })}
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
