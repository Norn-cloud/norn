import { Link, useLocation, useNavigate, useSearch } from "@tanstack/react-router"
import { ArrowRight, BadgeCheck, Box, BriefcaseBusiness, Building2, CalendarClock, ClipboardList, CreditCard, Database, FileCheck, Files, Gauge, GitMerge, Landmark, Layers, LifeBuoy, Network, Receipt, Search, UserRound, type LucideIcon } from "lucide-react"
import { createContext, use, useEffect, useState, type ReactNode } from "react"
import { Chip } from "@/components/chip"
import { Input } from "@/components/ui/input"
import { cn } from "@/lib/utils"
import { AGGREGATES, AUXILIARY_TYPES, COMMANDS, type AuxiliaryType } from "./behavior.ts"
import { isFinanceType, RECORDS } from "./records.ts"
import { CLIENT_FIELDS, MARKS, MODEL_LINKS, ROW_COUNT, SYSTEMS, TYPES, type FinanceType, type MarkId } from "./data.ts"

export type FinanceMode = "domain" | "data"

const MODE_KEY = "norn.studio.finance.mode"
const DATA_ONLY_PATHS = ["/examples/dataset", "/examples/explorer", "/markings"]
const DOMAIN_ONLY_PREFIXES = ["/workflows", "/guarantees", "/metrics", "/optimisations", "/examples/document", "/policies"]

const FinanceModeContext = createContext<{
  readonly mode: FinanceMode
  readonly setMode: (next: FinanceMode) => void
}>({ mode: "domain", setMode: () => {} })

function readStoredMode(): FinanceMode {
  try {
    return globalThis.localStorage?.getItem(MODE_KEY) === "data" ? "data" : "domain"
  } catch {
    return "domain"
  }
}

export function FinanceModeProvider({ children }: { readonly children: ReactNode }) {
  const pathname = useLocation({ select: (location) => location.pathname })
  const [mode, setModeState] = useState<FinanceMode>(() => {
    if (DATA_ONLY_PATHS.some((path) => pathname === path || pathname.startsWith(`${path}/`))) return "data"
    if (DOMAIN_ONLY_PREFIXES.some((path) => pathname === path || pathname.startsWith(`${path}/`))) return "domain"
    return readStoredMode()
  })

  useEffect(() => {
    if (DATA_ONLY_PATHS.some((path) => pathname === path || pathname.startsWith(`${path}/`))) {
      setModeState("data")
    } else if (DOMAIN_ONLY_PREFIXES.some((path) => pathname === path || pathname.startsWith(`${path}/`))) {
      setModeState("domain")
    }
  }, [pathname])

  const setMode = (next: FinanceMode) => {
    setModeState(next)
    try { globalThis.localStorage?.setItem(MODE_KEY, next) } catch { /* ignore storage restrictions */ }
  }

  return <FinanceModeContext value={{ mode, setMode }}>{children}</FinanceModeContext>
}

export const useFinanceMode = () => use(FinanceModeContext)

/** Top-bar Snowflake/Retool persona switcher: separates clean Domain modeling from Data Engineering pipelines. */
export function FinanceModeSwitcher() {
  const { mode, setMode } = useFinanceMode()
  const pathname = useLocation({ select: (location) => location.pathname })
  const navigate = useNavigate()

  const choose = (next: FinanceMode) => {
    if (next === mode) return
    setMode(next)
    if (next === "domain") {
      if (DATA_ONLY_PATHS.some((path) => pathname === path || pathname.startsWith(`${path}/`))) {
        void navigate({ to: "/" })
      }
    } else {
      if (DOMAIN_ONLY_PREFIXES.some((path) => pathname === path || pathname.startsWith(`${path}/`)) || pathname === "/") {
        void navigate({ to: "/sources", search: { view: undefined, model: undefined } })
      }
    }
  }

  return (
    <div role="group" aria-label="Workspace perspective" className="inline-flex h-8 items-center rounded-md border border-border bg-surface-subtle p-0.5 text-xs">
      <button
        type="button"
        aria-pressed={mode === "domain"}
        onClick={() => choose("domain")}
        className={cn(
          "inline-flex h-7 items-center gap-1.5 rounded-sm px-2.5 font-medium transition-colors",
          mode === "domain" ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground",
        )}
      >
        <Layers className="size-3.5" aria-hidden />
        <span>Domain</span>
      </button>
      <button
        type="button"
        aria-pressed={mode === "data"}
        onClick={() => choose("data")}
        className={cn(
          "inline-flex h-7 items-center gap-1.5 rounded-sm px-2.5 font-medium transition-colors",
          mode === "data" ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground",
        )}
      >
        <Database className="size-3.5" aria-hidden />
        <span>Data Engineering</span>
      </button>
    </div>
  )
}

const glyphs: Record<FinanceType, LucideIcon> = {
  client: UserRound, loan: Landmark, installment: CalendarClock, payment: CreditCard,
  "credit-report": Gauge, "kyc-check": FileCheck, "relationship-manager": BriefcaseBusiness,
  "hardship-case": LifeBuoy, "brokerage-account": Building2, order: ClipboardList, "regulatory-return": Receipt,
}

/** Restrained neutral object glyph (Palantir OMA / Snowsight style — color reserved for status and active selection). */
export function TypeTile({ type, className }: { readonly type: FinanceType; readonly className?: string }) {
  const Icon = glyphs[type]
  return (
    <span aria-hidden className={cn("inline-flex size-6 shrink-0 items-center justify-center rounded-md border border-border bg-surface-subtle text-foreground/75 [&_svg]:size-3.5", className)}>
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
      className={cn("relative inline-flex size-6 shrink-0 items-center justify-center rounded-md border border-border bg-background text-[8px] font-semibold", className)}
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
          <span key={id} className="inline-flex items-center gap-1 rounded-md border border-border bg-surface-subtle px-1.5 py-0.5 text-[11px] text-muted-foreground">
            <Mark id={id} className="size-4 rounded-xs border-0 text-[7px]" />
            {MARKS[id].name}
          </span>
        ))}
      </div>
      {!compact && <p className="text-xs text-muted-foreground"><strong className="font-medium text-foreground">Why joined:</strong> {needs}</p>}
    </div>
  )
}

/** Compact enterprise workbench header: flat, crisp, no redundant pills. */
export function FinanceHeading({ title, description, actions }: { readonly title: string; readonly description: string; readonly actions?: ReactNode }) {
  return (
    <header className="flex flex-wrap items-end justify-between gap-3 border-b border-border pb-3">
      <div className="min-w-0">
        <h1 className="text-xl font-semibold tracking-tight text-foreground">{title}</h1>
        <p className="mt-0.5 max-w-2xl text-xs text-muted-foreground">{description}</p>
      </div>
      {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
    </header>
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

function ModelGraph({ showRawFields }: { readonly showRawFields: boolean }) {
  return (
    <section aria-label="Lending model graph" className="relative grid gap-3 rounded-lg border border-border bg-surface-subtle p-4 md:block md:h-[30rem] md:bg-[radial-gradient(var(--canvas-dot)_1px,transparent_1px)] md:bg-size-[16px_16px]">
      <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="absolute inset-0 hidden size-full md:block">
        {EDGES.map(({ from: [x, y], to: [a, b], text }) => (
          <path
            key={text}
            d={x === a ? `M ${x} ${y} V ${b}` : `M ${x} ${y} C ${(x + a) / 2} ${y}, ${(x + a) / 2} ${b}, ${a} ${b}`}
            fill="none"
            stroke="var(--edge)"
            strokeOpacity={0.75}
            strokeWidth="1.25"
            vectorEffect="non-scaling-stroke"
          />
        ))}
      </svg>
      {EDGES.map(({ text, label: [x, y] }) => (
        <span key={text} className="pointer-events-none absolute z-10 hidden -translate-x-1/2 -translate-y-1/2 whitespace-nowrap rounded-sm border border-border bg-background px-1.5 py-0.5 text-[10px] text-muted-foreground md:block" style={{ left: `${x}%`, top: `${y}%` }}>
          {text}
        </span>
      ))}
      <article className="z-20 space-y-2.5 rounded-lg border border-data-accent bg-background p-3 md:absolute md:top-1/2 md:left-1/2 md:w-[25%] md:-translate-1/2">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <TypeTile type="client" className="border-data-accent/40 text-tone-blue" />
            <h3 className="text-sm font-semibold">
              <Link to="/sources/$concept" params={{ concept: "client" }} className="hover:text-tone-blue">Client (Borrower)</Link>
            </h3>
          </div>
          <Chip tone="blue" icon={GitMerge}>Resolved</Chip>
        </div>
        {showRawFields ? (
          <ul className="grid gap-1 border-t border-border pt-2">
            {CLIENT_FIELDS.map(({ mark, field }) => (
              <li key={field} className="flex items-center gap-1.5">
                <Mark id={mark} className="size-4 text-[7px]" />
                <code className="text-[11px] text-muted-foreground">{field}</code>
              </li>
            ))}
          </ul>
        ) : (
          <p className="border-t border-border pt-2 text-xs text-muted-foreground">
            Unified client entity resolved on National ID across lending, bureau, KYC and CRM.
          </p>
        )}
        <div className="border-t border-border pt-1.5 text-[11px] text-muted-foreground">Identifier · <span className="font-medium text-foreground">National ID</span></div>
      </article>
      {NODES.map(({ type, x, y, field }) => (
        <article key={type} className="rounded-lg border border-border bg-background p-2.5 hover:border-data-accent md:absolute md:w-[23%] md:-translate-1/2" style={{ left: `${x}%`, top: `${y}%` }}>
          <div className="flex items-center gap-2">
            <TypeTile type={type} />
            <div className="min-w-0">
              <h3 className="truncate text-xs font-semibold">
                <Link to="/sources/$concept" params={{ concept: type }} className="hover:text-tone-blue">{TYPES[type].name}</Link>
              </h3>
              <p className="truncate text-[10px] text-muted-foreground">
                {showRawFields ? <code>{field}</code> : `${RECORDS.filter((r) => r.type === type).length} sample records`}
              </p>
            </div>
          </div>
        </article>
      ))}
    </section>
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
    <div className="space-y-4">
      <section aria-label={showAggregates ? "Finance aggregate boundaries" : "Full finance model graph"} className="relative grid grid-cols-2 gap-2 rounded-lg border border-border bg-surface-subtle p-4 md:block md:h-[30rem] md:bg-[radial-gradient(var(--canvas-dot)_1px,transparent_1px)] md:bg-size-[16px_16px]">
        {showAggregates && AGGREGATES.map(({ root }) => (
          <div key={root} className="absolute top-[4%] bottom-[42%] hidden w-[29%] -translate-x-1/2 rounded-md border border-dashed border-data-accent/60 bg-data-accent/[0.03] md:block" style={{ left: `${AGGREGATE_POSITIONS[root][0]}%` }}>
            <p className="px-2 pt-1.5 text-center text-[10px] font-semibold tracking-wide uppercase text-tone-blue">{TYPES[root].name} aggregate</p>
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
                strokeOpacity={0.6}
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
                "z-10 flex items-center gap-2 rounded-md border border-border bg-background px-2.5 py-2 text-xs font-medium transition-colors hover:border-data-accent md:absolute md:w-[22%] md:-translate-1/2",
                (showAggregates ? root : type === "client") && "border-s-2 border-s-data-accent border-data-accent/50",
              )}
              style={{ left: `${left}%`, top: `${top}%` }}
            >
              {isFinanceType(type) ? <TypeTile type={type} className={root && showAggregates ? "border-data-accent/40 text-tone-blue" : undefined} /> : <Box className="size-4 text-muted-foreground" />}
              <span className="min-w-0 truncate">
                {core?.name ?? auxiliary?.name}
                {showAggregates && root && <span className="block text-[10px] font-normal text-tone-blue">Aggregate root</span>}
              </span>
            </Link>
          )
        })}
      </section>
      <section aria-label="Model relationships" className="overflow-hidden rounded-lg border border-border bg-background">
        <header className="border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold text-foreground">
          Relationships &amp; cardinalities
        </header>
        <ul className="grid divide-y divide-border md:grid-cols-2 md:divide-y-0">
          {MODEL_LINKS.map(([from, forward, to, , outgoing, incoming]) => (
            <li key={from + to} className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-2 text-xs md:odd:border-e">
              <Link to="/sources/$concept" params={{ concept: from }} className="font-medium hover:text-tone-blue">{TYPES[from].name}</Link>
              <code className="rounded-xs bg-muted px-1 text-[10px] text-muted-foreground">{incoming}</code>
              <span className="text-muted-foreground">{forward}</span>
              <ArrowRight className="size-3 text-muted-foreground" />
              <Link to="/sources/$concept" params={{ concept: to }} className="font-medium hover:text-tone-blue">{TYPES[to].name}</Link>
              <code className="rounded-xs bg-muted px-1 text-[10px] text-muted-foreground">{outgoing}</code>
            </li>
          ))}
        </ul>
      </section>
    </div>
  )
}

function ObjectCatalog() {
  const [query, setQuery] = useState("")
  const entries = (Object.entries(TYPES) as [FinanceType, (typeof TYPES)[FinanceType]][]).filter(([, item]) =>
    item.name.toLowerCase().includes(query.trim().toLowerCase()),
  )
  return (
    <div className="overflow-hidden rounded-lg border border-border bg-background">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-surface-subtle px-4 py-2.5">
        <label className="relative block w-full max-w-xs">
          <span className="sr-only">Filter object types</span>
          <Search className="pointer-events-none absolute start-2.5 top-2 size-3.5 text-muted-foreground" />
          <Input className="h-8 bg-background ps-8 text-xs" placeholder="Filter business objects…" value={query} onChange={(e) => setQuery(e.target.value)} />
        </label>
        <span className="text-xs text-muted-foreground tabular-nums">{entries.length} object types</span>
      </div>
      <div className="overflow-x-auto">
        <table className="w-full text-start text-xs">
          <thead>
            <tr className="border-b border-border bg-surface-subtle/60 text-muted-foreground">
              <th className="px-4 py-2 text-start font-medium">Object type</th>
              <th className="px-4 py-2 text-start font-medium">Aggregate role</th>
              <th className="px-4 py-2 text-start font-medium">Commands</th>
              <th className="px-4 py-2 text-start font-medium">Links</th>
              <th className="px-4 py-2 text-end font-medium">Sample records</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {entries.map(([id, def]) => {
              const agg = AGGREGATES.find((a) => a.root === id || a.members.some((m) => m === id))
              const role = agg?.root === id ? "Aggregate root" : agg ? `${TYPES[agg.root].name} entity` : "Reference entity"
              const cmdCount = Object.values(COMMANDS).filter((c) => c.on === id).length
              const linkCount = MODEL_LINKS.filter(([from, , to]) => from === id || to === id).length
              const recCount = RECORDS.filter((r) => r.type === id).length
              return (
                <tr key={id} className="transition-colors hover:bg-surface-subtle">
                  <td className="px-4 py-2.5">
                    <Link to="/sources/$concept" params={{ concept: id }} className="inline-flex items-center gap-2 font-medium text-foreground hover:text-tone-blue">
                      <TypeTile type={id} />
                      <span>{def.name}</span>
                    </Link>
                  </td>
                  <td className="px-4 py-2.5 text-muted-foreground">
                    {agg?.root === id ? <span className="inline-flex items-center rounded-sm bg-tone-blue/10 px-1.5 py-0.5 text-[11px] font-medium text-tone-blue">{role}</span> : role}
                  </td>
                  <td className="px-4 py-2.5 tabular-nums text-muted-foreground">{cmdCount > 0 ? cmdCount : "—"}</td>
                  <td className="px-4 py-2.5 tabular-nums text-muted-foreground">{linkCount}</td>
                  <td className="px-4 py-2.5 text-end tabular-nums text-muted-foreground">{recCount}</td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
    </div>
  )
}

/** Snowflake-style Connected Systems table + docked Schema Inspector for Data Engineering mode. */
function DataSourcesWorkbench() {
  const [selectedId, setSelectedId] = useState<string>(SYSTEMS[0]!.id)
  const selected = SYSTEMS.find((s) => s.id === selectedId) ?? SYSTEMS[0]!
  const totalDatasets = SYSTEMS.reduce((count, item) => count + item.datasets.length, 0)
  const totalRows = SYSTEMS.reduce((sum, item) => sum + item.datasets.reduce((s, [, r]) => s + r, 0), 0)

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 divide-y divide-border overflow-hidden rounded-lg border border-border bg-background sm:grid-cols-4 sm:divide-x sm:divide-y-0">
        <div className="px-4 py-2.5"><p className="text-[11px] text-muted-foreground">Connected systems</p><p className="mt-0.5 text-lg font-semibold tabular-nums">7</p></div>
        <div className="px-4 py-2.5"><p className="text-[11px] text-muted-foreground">Ingested tables</p><p className="mt-0.5 text-lg font-semibold tabular-nums">{totalDatasets}</p></div>
        <div className="px-4 py-2.5"><p className="text-[11px] text-muted-foreground">Records synced</p><p className="mt-0.5 text-lg font-semibold tabular-nums">{ROW_COUNT.format(totalRows)}</p></div>
        <div className="px-4 py-2.5"><p className="text-[11px] text-muted-foreground">Last sync window</p><p className="mt-0.5 inline-flex items-center gap-1.5 text-sm font-medium"><BadgeCheck className="size-3.5 text-tone-green" />22 Nov 2026 · EET</p></div>
      </div>

      <div className="grid overflow-hidden rounded-lg border border-border bg-background lg:grid-cols-[minmax(0,1fr)_21rem]">
        <div className="min-w-0 overflow-x-auto">
          <table className="w-full text-start text-xs" aria-label="Connected source systems">
            <thead>
              <tr className="border-b border-border bg-surface-subtle text-muted-foreground">
                <th className="px-4 py-2.5 text-start font-medium">System</th>
                <th className="px-4 py-2.5 text-start font-medium">Schema</th>
                <th className="px-4 py-2.5 text-end font-medium">Datasets</th>
                <th className="px-4 py-2.5 text-end font-medium">Rows</th>
                <th className="px-4 py-2.5 text-end font-medium">Last sync</th>
                <th className="px-4 py-2.5 text-end font-medium">Status</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {SYSTEMS.map((system) => {
                const active = system.id === selected.id
                const rows = system.datasets.reduce((sum, [, count]) => sum + count, 0)
                return (
                  <tr
                    key={system.id}
                    onClick={() => setSelectedId(system.id)}
                    className={cn("cursor-pointer transition-colors hover:bg-surface-subtle", active && "bg-tone-blue/[0.06]")}
                  >
                    <td className={cn("px-4 py-2.5", active && "border-s-2 border-s-data-accent")}>
                      <div className="flex items-center gap-2.5">
                        <div className="flex -space-x-1">{system.marks.map((mark) => <Mark key={mark} id={mark} />)}</div>
                        <div>
                          <div className="font-medium text-foreground">{system.name}</div>
                          <div className="text-[11px] text-muted-foreground">{system.marks.map((m) => MARKS[m].name).join(" · ")}</div>
                        </div>
                      </div>
                    </td>
                    <td className="px-4 py-2.5 font-mono text-[11px] text-muted-foreground">{system.schema}</td>
                    <td className="px-4 py-2.5 text-end tabular-nums text-muted-foreground">{system.datasets.length}</td>
                    <td className="px-4 py-2.5 text-end tabular-nums text-foreground">{ROW_COUNT.format(rows)}</td>
                    <td className="px-4 py-2.5 text-end tabular-nums text-muted-foreground">{system.syncedAt}</td>
                    <td className="px-4 py-2.5 text-end"><Chip tone="green" dot>Synced</Chip></td>
                  </tr>
                )
              })}
            </tbody>
          </table>
          <div className="flex items-center justify-between border-t border-border bg-surface-subtle px-4 py-2 text-xs text-muted-foreground">
            <span className="inline-flex items-center gap-2"><Mark id="aws" className="size-5" />Hosted on AWS · Synthetic presentation data</span>
            <Link to="/examples/$scene" params={{ scene: "dataset" }} className="inline-flex items-center gap-1 font-medium text-tone-blue hover:underline">
              Open field mappings <ArrowRight className="size-3.5" />
            </Link>
          </div>
        </div>

        <aside aria-label="Schema inspector" className="flex flex-col border-t border-border bg-surface-subtle/40 lg:border-s lg:border-t-0">
          <div className="border-b border-border bg-surface-subtle px-4 py-2.5">
            <p className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">Schema Inspector</p>
            <div className="mt-1 flex items-center gap-2">
              <div className="flex -space-x-1">{selected.marks.map((m) => <Mark key={m} id={m} className="size-5" />)}</div>
              <h2 className="text-sm font-semibold text-foreground">{selected.name}</h2>
            </div>
          </div>
          <div className="flex-1 space-y-4 p-4 text-xs">
            <div>
              <p className="text-[11px] text-muted-foreground">Schema contract</p>
              <code className="mt-1 block rounded-md border border-border bg-background px-2.5 py-1.5 font-mono text-xs text-foreground">{selected.schema}</code>
            </div>
            <div>
              <div className="flex items-center justify-between text-[11px] text-muted-foreground">
                <span>Datasets ({selected.datasets.length})</span>
                <span>Synced {selected.syncedAt} EET</span>
              </div>
              <dl className="mt-1.5 divide-y divide-border rounded-md border border-border bg-background">
                {selected.datasets.map(([dataset, rows]) => (
                  <div key={dataset} className="flex items-center justify-between gap-2 px-3 py-2">
                    <dt className="flex items-center gap-1.5 font-mono text-[11px] text-foreground"><Files className="size-3.5 text-muted-foreground" />{dataset}</dt>
                    <dd className="tabular-nums text-muted-foreground">{ROW_COUNT.format(rows)}</dd>
                  </div>
                ))}
              </dl>
            </div>
            <div className="space-y-1.5 border-t border-border pt-3">
              <Link to="/examples/$scene" params={{ scene: "dataset" }} className="flex w-full items-center justify-between rounded-md border border-border bg-background px-3 py-2 font-medium text-tone-blue hover:border-data-accent">
                <span>Inspect field mappings</span><ArrowRight className="size-3.5" />
              </Link>
              <Link to="/sources" search={{ view: "graph", model: "aggregates" }} className="flex w-full items-center justify-between rounded-md border border-border bg-background px-3 py-2 font-medium text-foreground hover:border-data-accent">
                <span>View target ontology graph</span><Network className="size-3.5 text-muted-foreground" />
              </Link>
            </div>
          </div>
        </aside>
      </div>
    </div>
  )
}

const underlineTab = "inline-flex items-center gap-1.5 border-b-2 border-transparent px-3 py-2 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground"
const underlineTabActive = "border-data-accent text-foreground"

export function FinanceSources() {
  const { mode } = useFinanceMode()
  const search = useSearch({ strict: false })
  const explicitGraph = "view" in search && search.view === "graph"
  const model = "model" in search ? search.model : undefined
  const [domainView, setDomainView] = useState<"catalog" | "aggregates" | "full" | "borrower">(
    explicitGraph ? (model === "aggregates" ? "aggregates" : model === "full" ? "full" : "borrower") : "aggregates",
  )

  useEffect(() => {
    if (explicitGraph) {
      setDomainView(model === "aggregates" ? "aggregates" : model === "full" ? "full" : "borrower")
    }
  }, [explicitGraph, model])

  if (mode === "data" && !explicitGraph) {
    return (
      <section className="space-y-4">
        <FinanceHeading
          title="Sources & Sync"
          description="Seven operational systems ingested and normalized into the shared financial ontology."
          actions={
            <Link to="/sources" search={{ view: "graph", model: "aggregates" }} className="inline-flex h-8 items-center gap-1.5 rounded-md border border-border bg-background px-3 text-xs font-medium hover:border-data-accent">
              <Network className="size-3.5" />Model graph
            </Link>
          }
        />
        <DataSourcesWorkbench />
      </section>
    )
  }

  return (
    <section className="space-y-4">
      <FinanceHeading
        title={mode === "data" ? "Sources & Model Graph" : "Business Objects"}
        description={mode === "data"
          ? "How the seven source systems map into typed objects and aggregate boundaries."
          : "Typed financial objects, aggregate boundaries and relationships — clean data assumed."}
        actions={
          mode === "data" ? (
            <Link to="/sources" search={{ view: undefined, model: undefined }} className="inline-flex h-8 items-center gap-1.5 rounded-md border border-border bg-background px-3 text-xs font-medium hover:border-data-accent">
              <Database className="size-3.5" />Connected systems
            </Link>
          ) : undefined
        }
      />

      <nav aria-label="Object model views" className="flex flex-wrap gap-1 border-b border-border">
        <button type="button" onClick={() => setDomainView("aggregates")} className={cn(underlineTab, domainView === "aggregates" && underlineTabActive)}>
          <Box className="size-3.5" />Aggregate boundaries
        </button>
        <button type="button" onClick={() => setDomainView("catalog")} className={cn(underlineTab, domainView === "catalog" && underlineTabActive)}>
          <Layers className="size-3.5" />Object catalog ({Object.keys(TYPES).length})
        </button>
        <button type="button" onClick={() => setDomainView("full")} className={cn(underlineTab, domainView === "full" && underlineTabActive)}>
          <Network className="size-3.5" />Full graph
        </button>
        <button type="button" onClick={() => setDomainView("borrower")} className={cn(underlineTab, domainView === "borrower" && underlineTabActive)}>
          <GitMerge className="size-3.5" />Borrower resolution
        </button>
      </nav>

      {domainView === "catalog" ? (
        <ObjectCatalog />
      ) : domainView === "borrower" ? (
        <ModelGraph showRawFields={mode === "data"} />
      ) : (
        <FullModel showAggregates={domainView === "aggregates"} />
      )}
    </section>
  )
}
