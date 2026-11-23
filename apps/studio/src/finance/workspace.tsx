import { Link, useSearch } from "@tanstack/react-router"
import { ArrowRight, BadgeCheck, Box, BriefcaseBusiness, Building2, CalendarClock, ClipboardList, CreditCard, Database, FileCheck, Files, Gauge, GitMerge, Landmark, Layers, LifeBuoy, Network, Receipt, Search, UserRound, type LucideIcon } from "lucide-react"
import { useEffect, useState } from "react"
import { Chip } from "@/components/chip"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { PageHeader, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { cn } from "@/lib/utils"
import { AGGREGATES, AUXILIARY_TYPES, COMMANDS, type AuxiliaryType } from "./behavior.ts"
import { CLIENT_FIELDS, MARKS, MODEL_LINKS, ROW_COUNT, SYSTEMS, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { isFinanceType, RECORDS } from "./records.ts"

const glyphs: Record<FinanceType, LucideIcon> = {
  client: UserRound, loan: Landmark, installment: CalendarClock, payment: CreditCard,
  "credit-report": Gauge, "kyc-check": FileCheck, "relationship-manager": BriefcaseBusiness,
  "hardship-case": LifeBuoy, "brokerage-account": Building2, order: ClipboardList, "regulatory-return": Receipt,
}

/** Restrained neutral object glyph: colour stays reserved for semantic status. */
export function TypeTile({ type, className }: { readonly type: FinanceType; readonly className?: string }) {
  const Icon = glyphs[type]
  return (
    <span aria-hidden className={cn("inline-flex size-6 shrink-0 items-center justify-center rounded-md border bg-muted/40 text-muted-foreground [&_svg]:size-3.5", className)}>
      <Icon />
    </span>
  )
}

export function Mark({ id, className }: { readonly id: MarkId; readonly className?: string }) {
  const { name, tone, file, letters } = MARKS[id]
  const [loaded, setLoaded] = useState(false)
  return (
    <span
      title={name}
      aria-label={name}
      role="img"
      className={cn("relative inline-flex size-6 shrink-0 items-center justify-center rounded-md border bg-card text-[8px] font-semibold", className)}
      style={{ color: `var(--tone-${tone})` }}
    >
      <span aria-hidden className={loaded ? "invisible" : undefined}>{letters ?? name.slice(0, 2).toUpperCase()}</span>
      {file && <img src={`/finance-marks/${file}`} alt="" className={cn("absolute inset-0 size-full object-contain p-[14%]", !loaded && "opacity-0")} onLoad={() => setLoaded(true)} />}
    </span>
  )
}

export function SystemDependencies({ systems, needs, compact = false }: { readonly systems: readonly MarkId[]; readonly needs: string; readonly compact?: boolean }) {
  return (
    <div className="space-y-1.5">
      <div className="flex flex-wrap items-center gap-1.5" aria-label="Required systems">
        {systems.map((id) => (
          <span key={id} className="inline-flex items-center gap-1.5 rounded-md border bg-muted/30 px-1.5 py-0.5 text-xs text-muted-foreground">
            <Mark id={id} className="size-4 rounded-xs border-0 bg-transparent text-[7px]" />
            {MARKS[id].name}
          </span>
        ))}
      </div>
      {!compact && <p className="text-meta text-muted-foreground"><strong className="font-medium text-foreground">Why joined:</strong> {needs}</p>}
    </div>
  )
}

const NODES = [
  { type: "loan", x: 16, y: 21, field: "loans" },
  { type: "payment", x: 16, y: 77, field: "collections" },
  { type: "relationship-manager", x: 84, y: 12, field: "relationship_managers" },
  { type: "credit-report", x: 84, y: 37, field: "scores" },
  { type: "kyc-check", x: 84, y: 63, field: "identity_checks" },
  { type: "hardship-case", x: 84, y: 88, field: "cases" },
] as const

const EDGES = [
  { from: [37, 43], to: [29, 21], text: "holds", label: [33, 31] },
  { from: [16, 31], to: [16, 67], text: "is paid by", label: [16, 49] },
  { from: [71, 12], to: [63, 39], text: "manages", label: [67, 22] },
  { from: [63, 46], to: [71, 37], text: "reported by", label: [67, 41] },
  { from: [63, 57], to: [71, 63], text: "verified by", label: [67, 61] },
  { from: [63, 63], to: [71, 88], text: "has", label: [67, 76] },
] as const

function BorrowerGraph({ showRawFields, onToggleRaw }: { readonly showRawFields: boolean; readonly onToggleRaw: () => void }) {
  return (
    <Panel
      icon={GitMerge}
      label="Borrower resolution"
      action={
        <Button type="button" size="sm" variant="ghost" className="h-7 text-xs" onClick={onToggleRaw}>
          {showRawFields ? "Hide source columns" : "Show source columns"}
        </Button>
      }
    >
      <Well>
        <div aria-label="Lending model graph" className="relative grid gap-3 bg-canvas-surface p-4 md:block md:h-[30rem] md:bg-[radial-gradient(var(--canvas-dot)_1px,transparent_1px)] md:bg-size-[16px_16px]">
          <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="absolute inset-0 hidden size-full md:block">
            {EDGES.map(({ from: [x, y], to: [a, b], text }) => (
              <path
                key={text}
                d={x === a ? `M ${x} ${y} V ${b}` : `M ${x} ${y} C ${(x + a) / 2} ${y}, ${(x + a) / 2} ${b}, ${a} ${b}`}
                fill="none"
                stroke="var(--edge)"
                strokeOpacity={0.65}
                strokeWidth="1.25"
                vectorEffect="non-scaling-stroke"
              />
            ))}
          </svg>
          {EDGES.map(({ text, label: [x, y] }) => (
            <span key={text} className="pointer-events-none absolute z-10 hidden -translate-x-1/2 -translate-y-1/2 whitespace-nowrap rounded-sm border bg-card px-1.5 py-0.5 text-[11px] text-muted-foreground md:block" style={{ left: `${x}%`, top: `${y}%` }}>
              {text}
            </span>
          ))}
          <article className="z-20 space-y-2.5 rounded-lg border border-foreground/30 bg-card p-3 md:absolute md:top-1/2 md:left-1/2 md:w-[25%] md:-translate-1/2">
            <div className="flex items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <TypeTile type="client" className="text-foreground" />
                <h3 className="text-sm font-medium">
                  <Link to="/sources/$concept" params={{ concept: "client" }} className="hover:underline underline-offset-4">Client (Borrower)</Link>
                </h3>
              </div>
              <Chip tone="green" dot>Resolved</Chip>
            </div>
            {showRawFields ? (
              <ul className="grid gap-1 border-t pt-2">
                {CLIENT_FIELDS.map(({ mark, field }) => (
                  <li key={field} className="flex items-center gap-1.5">
                    <Mark id={mark} className="size-4 text-[7px]" />
                    <code className="text-xs text-muted-foreground">{field}</code>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="border-t pt-2 text-xs text-muted-foreground">
                Unified client entity resolved on National ID across lending, bureau, KYC and CRM.
              </p>
            )}
            <div className="border-t pt-1.5 text-xs text-muted-foreground">Matching key · <span className="font-medium text-foreground">National ID</span></div>
          </article>
          {NODES.map(({ type, x, y, field }) => (
            <article key={type} className="rounded-lg border bg-card p-2.5 transition-colors hover:border-foreground/35 md:absolute md:w-[23%] md:-translate-1/2" style={{ left: `${x}%`, top: `${y}%` }}>
              <div className="flex items-center gap-2">
                <TypeTile type={type} />
                <div className="min-w-0">
                  <h3 className="truncate text-meta font-medium">
                    <Link to="/sources/$concept" params={{ concept: type }} className="hover:underline underline-offset-4">{TYPES[type].name}</Link>
                  </h3>
                  <p className="truncate text-xs text-muted-foreground">
                    {showRawFields ? <code>{field}</code> : `${RECORDS.filter((r) => r.type === type).length} sample records`}
                  </p>
                </div>
              </div>
            </article>
          ))}
        </div>
      </Well>
    </Panel>
  )
}

const MODEL_POSITIONS: Record<FinanceType, readonly [number, number]> = {
  client: [49, 42], loan: [19, 40], installment: [19, 65], payment: [19, 86],
  "regulatory-return": [19, 16], "relationship-manager": [79, 10], "credit-report": [79, 28],
  "kyc-check": [79, 46], "hardship-case": [79, 64], "brokerage-account": [79, 84], order: [49, 84],
}

const AGGREGATE_POSITIONS: Record<FinanceType | AuxiliaryType, readonly [number, number]> = {
  client: [17, 18], "kyc-check": [17, 35], "relationship-assignment": [17, 49],
  loan: [50, 18], installment: [50, 35],
  "brokerage-account": [83, 18], order: [83, 35], holdings: [83, 49],
  "credit-report": [17, 71], "relationship-manager": [17, 88], "hardship-case": [50, 71],
  payment: [50, 88], "regulatory-return": [83, 71],
}
type ModelNode = keyof typeof AGGREGATE_POSITIONS

function FullModel({ showAggregates }: { readonly showAggregates: boolean }) {
  const edges: readonly (readonly [ModelNode, ModelNode])[] = [
    ...MODEL_LINKS.map(([from, , to]) => [from, to] as const),
    ...(showAggregates ? AGGREGATES.flatMap((group) => group.related.map((member) => [group.root, member] as const)) : []),
  ]
  const modelPosition = (id: ModelNode) => showAggregates || !isFinanceType(id) ? AGGREGATE_POSITIONS[id] : MODEL_POSITIONS[id]
  const positions = showAggregates ? AGGREGATE_POSITIONS : MODEL_POSITIONS
  return (
    <div className="space-y-6">
      <Panel icon={showAggregates ? Box : Network} label={showAggregates ? "Aggregate boundaries" : "Full model graph"} action="Select any type to inspect its definition">
        <Well>
          <div aria-label={showAggregates ? "Finance aggregate boundaries" : "Full finance model graph"} className="relative grid grid-cols-2 gap-2 bg-canvas-surface p-4 md:block md:h-[30rem] md:bg-[radial-gradient(var(--canvas-dot)_1px,transparent_1px)] md:bg-size-[16px_16px]">
            {showAggregates && AGGREGATES.map(({ root }) => (
              <div key={root} className="absolute top-[4%] bottom-[42%] hidden w-[29%] -translate-x-1/2 rounded-lg border border-dashed border-muted-foreground/40 bg-muted/25 md:block" style={{ left: `${AGGREGATE_POSITIONS[root][0]}%` }}>
                <p className="px-2 pt-1.5 text-center text-[11px] font-medium tracking-wide uppercase text-muted-foreground">{TYPES[root].name} aggregate</p>
              </div>
            ))}
            <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="absolute inset-0 hidden size-full md:block">
              {edges.map(([from, to]) => {
                const [x, y] = modelPosition(from)
                const [a, b] = modelPosition(to)
                const mid = (x + a) / 2
                return (
                  <path
                    key={from + to}
                    d={x === a ? `M${x} ${y} V${b}` : `M${x} ${y} C${mid} ${y} ${mid} ${b} ${a} ${b}`}
                    fill="none"
                    stroke="var(--edge)"
                    strokeOpacity={0.55}
                    strokeWidth="1.25"
                    vectorEffect="non-scaling-stroke"
                  />
                )
              })}
            </svg>
            {Object.entries(positions).map(([type, [left, top]]) => {
              const auxiliary = type === "relationship-assignment" || type === "holdings" ? AUXILIARY_TYPES[type] : undefined
              const core = isFinanceType(type) ? TYPES[type] : undefined
              if (!auxiliary && !core) return null
              const root = AGGREGATES.some((group) => group.root === type)
              return (
                <Link
                  key={type}
                  to={auxiliary ? `/policies/${type}` : `/sources/${type}`}
                  className={cn(
                    "z-10 flex items-center gap-2 rounded-md border bg-card px-2.5 py-2 text-meta font-medium transition-colors hover:border-foreground/40 md:absolute md:w-[22%] md:-translate-1/2",
                    (showAggregates ? root : type === "client") && "border-s-2 border-s-foreground",
                  )}
                  style={{ left: `${left}%`, top: `${top}%` }}
                >
                  {isFinanceType(type) ? <TypeTile type={type} className={root && showAggregates ? "text-foreground" : undefined} /> : <Box className="size-4 text-muted-foreground" />}
                  <span className="min-w-0 truncate">
                    {core?.name ?? auxiliary?.name}
                    {showAggregates && root && <span className="block text-[11px] font-normal text-muted-foreground">Aggregate root</span>}
                  </span>
                </Link>
              )
            })}
          </div>
        </Well>
      </Panel>

      <Panel icon={Network} label="Relationships and cardinalities" action={`${MODEL_LINKS.length} declared links`}>
        <Well>
          <ul aria-label="Model relationships" className="grid divide-y md:grid-cols-2 md:divide-y-0">
            {MODEL_LINKS.map(([from, forward, to, , outgoing, incoming]) => (
              <li key={from + to} className="flex flex-wrap items-center gap-2 border-b px-4 py-2.5 text-meta md:odd:border-e">
                <Link to="/sources/$concept" params={{ concept: from }} className="font-medium hover:underline underline-offset-4">{TYPES[from].name}</Link>
                <code className="rounded-sm bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">{incoming}</code>
                <span className="text-muted-foreground">{forward}</span>
                <ArrowRight className="size-3 text-muted-foreground" />
                <Link to="/sources/$concept" params={{ concept: to }} className="font-medium hover:underline underline-offset-4">{TYPES[to].name}</Link>
                <code className="rounded-sm bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">{outgoing}</code>
              </li>
            ))}
          </ul>
        </Well>
      </Panel>
    </div>
  )
}

function ObjectCatalog() {
  const [query, setQuery] = useState("")
  const entries = (Object.entries(TYPES) as [FinanceType, (typeof TYPES)[FinanceType]][]).filter(([, item]) =>
    item.name.toLowerCase().includes(query.trim().toLowerCase()),
  )
  return (
    <Panel
      icon={Layers}
      label="Object types"
      action={
        <div className="flex items-center gap-3">
          <label className="relative block w-52">
            <span className="sr-only">Filter object types</span>
            <Search className="pointer-events-none absolute start-2.5 top-2 size-3.5 text-muted-foreground" />
            <Input className="h-7 ps-8 text-xs" placeholder="Filter object types…" value={query} onChange={(e) => setQuery(e.target.value)} />
          </label>
          <span className="tabular-nums">{entries.length}</span>
        </div>
      }
    >
      <TableCard>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Object type</TableHead>
              <TableHead>Aggregate role</TableHead>
              <TableHead>Commands</TableHead>
              <TableHead>Links</TableHead>
              <TableHead className="text-end">Sample records</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {entries.map(([id, def]) => {
              const agg = AGGREGATES.find((a) => a.root === id || a.members.some((m) => m === id))
              const role = agg?.root === id ? "Aggregate root" : agg ? `${TYPES[agg.root].name} entity` : "Reference entity"
              const cmdCount = Object.values(COMMANDS).filter((c) => c.on === id).length
              const linkCount = MODEL_LINKS.filter(([from, , to]) => from === id || to === id).length
              const recCount = RECORDS.filter((r) => r.type === id).length
              return (
                <TableRow key={id}>
                  <TableCell>
                    <Link to="/sources/$concept" params={{ concept: id }} className="inline-flex items-center gap-2 font-medium text-foreground hover:underline underline-offset-4">
                      <TypeTile type={id} />
                      <span>{def.name}</span>
                    </Link>
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {agg?.root === id ? <Badge variant="secondary" className="font-normal">{role}</Badge> : role}
                  </TableCell>
                  <TableCell className="text-muted-foreground">{cmdCount > 0 ? cmdCount : "—"}</TableCell>
                  <TableCell className="text-muted-foreground">{linkCount}</TableCell>
                  <TableCell className="text-end text-muted-foreground">{recCount}</TableCell>
                </TableRow>
              )
            })}
          </TableBody>
        </Table>
      </TableCard>
    </Panel>
  )
}

/** Flat Snowsight-style connected systems table + hairline Schema Inspector side panel. */
function DataSourcesWorkbench() {
  const [selectedId, setSelectedId] = useState<string>(SYSTEMS[0]!.id)
  const selected = SYSTEMS.find((s) => s.id === selectedId) ?? SYSTEMS[0]!
  const totalDatasets = SYSTEMS.reduce((count, item) => count + item.datasets.length, 0)
  const totalRows = SYSTEMS.reduce((sum, item) => sum + item.datasets.reduce((s, [, r]) => s + r, 0), 0)

  return (
    <div className="space-y-6">
      <StatStrip>
        <KpiTile label="Connected systems" value={SYSTEMS.length} sub="Hosted on AWS · Synthetic preview" />
        <KpiTile label="Ingested tables" value={totalDatasets} sub="Normalized into 11 object types" />
        <KpiTile label="Records synced" value={ROW_COUNT.format(totalRows)} sub="11,872 clients resolved" />
        <KpiTile label="Last sync window" value="22 Nov" sub={<span className="inline-flex items-center gap-1 text-tone-green"><BadgeCheck className="size-3.5" />All systems synced · EET</span>} />
      </StatStrip>

      <Panel icon={Database} label="Connected systems" action="Select a system to inspect its schema">
        <Well className="grid lg:grid-cols-[minmax(0,1fr)_20rem]">
          <div className="min-w-0 overflow-x-auto">
            <Table aria-label="Connected source systems" className="text-meta tabular-nums">
              <TableHeader>
                <TableRow>
                  <TableHead>System</TableHead>
                  <TableHead>Schema</TableHead>
                  <TableHead className="text-end">Datasets</TableHead>
                  <TableHead className="text-end">Rows</TableHead>
                  <TableHead className="text-end">Last sync</TableHead>
                  <TableHead className="text-end">Status</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {SYSTEMS.map((system) => {
                  const active = system.id === selected.id
                  const rows = system.datasets.reduce((sum, [, count]) => sum + count, 0)
                  return (
                    <TableRow
                      key={system.id}
                      onClick={() => setSelectedId(system.id)}
                      className={cn("cursor-pointer", active && "bg-muted/60")}
                    >
                      <TableCell className={cn(active && "border-s-2 border-s-foreground")}>
                        <div className="flex items-center gap-2.5">
                          <div className="flex -space-x-1">{system.marks.map((mark) => <Mark key={mark} id={mark} />)}</div>
                          <div>
                            <div className="font-medium text-foreground">{system.name}</div>
                            <div className="text-xs text-muted-foreground">{system.marks.map((m) => MARKS[m].name).join(" · ")}</div>
                          </div>
                        </div>
                      </TableCell>
                      <TableCell className="font-mono text-xs text-muted-foreground">{system.schema}</TableCell>
                      <TableCell className="text-end text-muted-foreground">{system.datasets.length}</TableCell>
                      <TableCell className="text-end text-foreground">{ROW_COUNT.format(rows)}</TableCell>
                      <TableCell className="text-end text-muted-foreground">{system.syncedAt}</TableCell>
                      <TableCell className="text-end"><Chip tone="green" dot>Synced</Chip></TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
            <div className="flex flex-wrap items-center justify-between gap-2 border-t px-4 py-2.5 text-meta text-muted-foreground">
              <span className="inline-flex items-center gap-2"><Mark id="aws" className="size-5" />Hosted on AWS · Synthetic presentation data</span>
              <Link to="/examples/$scene" params={{ scene: "dataset" }} className="inline-flex items-center gap-1 font-medium text-foreground hover:underline underline-offset-4">
                Open field mappings <ArrowRight className="size-3.5 text-muted-foreground" />
              </Link>
            </div>
          </div>

          <aside aria-label="Schema inspector" className="flex flex-col border-t bg-surface-subtle/40 lg:border-s lg:border-t-0">
            <div className="border-b px-4 py-3">
              <p className="text-xs text-muted-foreground">Schema inspector</p>
              <div className="mt-1 flex items-center gap-2">
                <div className="flex -space-x-1">{selected.marks.map((m) => <Mark key={m} id={m} className="size-5" />)}</div>
                <h3 className="text-sm font-medium text-foreground">{selected.name}</h3>
              </div>
            </div>
            <div className="flex-1 divide-y px-4 text-meta">
              <div className="py-3">
                <p className="text-xs text-muted-foreground">Schema contract</p>
                <code className="mt-1 block font-mono text-xs text-foreground">{selected.schema}</code>
              </div>
              <div className="py-3">
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <span>Datasets ({selected.datasets.length})</span>
                  <span>Synced {selected.syncedAt} EET</span>
                </div>
                <dl className="mt-2 divide-y">
                  {selected.datasets.map(([dataset, rows]) => (
                    <div key={dataset} className="flex items-center justify-between gap-2 py-1.5">
                      <dt className="flex items-center gap-1.5 font-mono text-xs text-foreground"><Files className="size-3.5 text-muted-foreground" />{dataset}</dt>
                      <dd className="tabular-nums text-muted-foreground">{ROW_COUNT.format(rows)}</dd>
                    </div>
                  ))}
                </dl>
              </div>
              <div className="space-y-2 py-3 text-xs">
                <Link to="/examples/$scene" params={{ scene: "dataset" }} className="flex items-center justify-between py-1 font-medium text-foreground hover:underline underline-offset-4">
                  <span>Inspect field mappings</span><ArrowRight className="size-3.5 text-muted-foreground" />
                </Link>
                <Link to="/model" className="flex items-center justify-between py-1 font-medium text-foreground hover:underline underline-offset-4">
                  <span>View target model graph</span><ArrowRight className="size-3.5 text-muted-foreground" />
                </Link>
              </div>
            </div>
          </aside>
        </Well>
      </Panel>
    </div>
  )
}

const tabClass = "inline-flex items-center gap-1.5 border-b-2 border-transparent px-3 py-2 text-meta font-medium text-muted-foreground transition-colors hover:text-foreground"
const tabActive = "border-foreground text-foreground"

export function FinanceModel() {
  const search = useSearch({ strict: false })
  const initialView = "view" in search && (search.view === "catalog" || search.view === "full" || search.view === "borrower" || search.view === "aggregates")
    ? search.view
    : "model" in search && search.model === "full"
      ? "full"
      : "model" in search && search.model === "aggregates"
        ? "aggregates"
        : "aggregates"
  const [domainView, setDomainView] = useState<"catalog" | "aggregates" | "full" | "borrower">(initialView)
  const [showRawFields, setShowRawFields] = useState(false)

  useEffect(() => {
    setDomainView(initialView)
  }, [initialView])

  return (
    <section className="space-y-6">
      <PageHeader
        title="Model"
        meta={`${Object.keys(TYPES).length} object types · ${AGGREGATES.length} aggregates`}
        description="Typed financial objects, aggregate boundaries, and cross-system relationships."
        primary={<Button asChild size="sm"><Link to="/sources">Connected systems</Link></Button>}
        secondary={[{ label: "Dataset mapping", icon: GitMerge, to: "/examples/dataset" }]}
      />

      <nav aria-label="Object model views" className="flex flex-wrap gap-1 border-b">
        <button type="button" onClick={() => setDomainView("aggregates")} className={cn(tabClass, domainView === "aggregates" && tabActive)}>
          <Box className="size-3.5" />Aggregate boundaries
        </button>
        <button type="button" onClick={() => setDomainView("catalog")} className={cn(tabClass, domainView === "catalog" && tabActive)}>
          <Layers className="size-3.5" />Object catalog ({Object.keys(TYPES).length})
        </button>
        <button type="button" onClick={() => setDomainView("full")} className={cn(tabClass, domainView === "full" && tabActive)}>
          <Network className="size-3.5" />Full graph
        </button>
        <button type="button" onClick={() => setDomainView("borrower")} className={cn(tabClass, domainView === "borrower" && tabActive)}>
          <GitMerge className="size-3.5" />Borrower resolution
        </button>
      </nav>

      {domainView === "catalog" ? (
        <ObjectCatalog />
      ) : domainView === "borrower" ? (
        <BorrowerGraph showRawFields={showRawFields} onToggleRaw={() => setShowRawFields((v) => !v)} />
      ) : (
        <FullModel showAggregates={domainView === "aggregates"} />
      )}
    </section>
  )
}

export function FinanceSources() {
  const search = useSearch({ strict: false })
  if ("view" in search && search.view === "graph") return <FinanceModel />

  return (
    <section className="space-y-6">
      <PageHeader
        title="Sources"
        meta={`${SYSTEMS.length} systems`}
        description="Seven operational systems ingested and normalized into the shared financial model."
        primary={<Button asChild size="sm"><Link to="/examples/$scene" params={{ scene: "dataset" }}>Dataset mapping</Link></Button>}
        secondary={[{ label: "Model & aggregate graph", icon: Network, to: "/model" }]}
      />
      <DataSourcesWorkbench />
    </section>
  )
}
