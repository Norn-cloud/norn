import { Link, Navigate, useParams, useSearch } from "@tanstack/react-router"
import { ArrowRight, ChevronRight, Clock3, Database, GitMerge, Network, Search, Waypoints } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { Input } from "@/components/ui/input"
import type { MenuEntry } from "../shell.tsx"
import { cn } from "@/lib/utils"
import { CLIENT, MARKS, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { BY_ID, LINKS, RECORDS, isFinanceType, type Fact, type FinanceRecord } from "./records.ts"
import { financePolicyMenu } from "./policies.tsx"
import { PRIMARY_TABS, objectTab, primaryTabOf, TypeDefinition } from "./definitions.tsx"
import { FinanceHeading, Mark, TypeTile, useFinanceMode } from "./workspace.tsx"

export const financeSourceMenu = (): readonly MenuEntry[] => [
  ...Object.entries(TYPES).map(([type, definition]): MenuEntry => ({
    href: `/sources/${type}`, label: definition.name, group: "Object types", kind: "objectType",
    count: RECORDS.filter((record) => record.type === type).length,
  })),
  ...financePolicyMenu(),
]

export function RecordLink({ record, children, className }: {
  readonly record: FinanceRecord; readonly children: React.ReactNode; readonly className?: string
}) {
  return <Link to="/sources/$concept/$" params={{ concept: record.type, _splat: record.id }} className={className}>{children}</Link>
}

export function FinanceRecordRoute() {
  const { concept, _splat, type } = useParams({ strict: false })
  const name = concept ?? type
  if (!isFinanceType(name)) return <Navigate to="/sources" replace />
  const record = typeof _splat === "string" ? BY_ID.get(_splat) : undefined
  if (_splat && record?.type !== name) return <Navigate to="/sources/$concept" params={{ concept: name }} replace />
  return record?.type === name ? <RecordDetail key={record.id} record={record} /> : <TypeList key={name} type={name} />
}

function TypeList({ type }: { readonly type: FinanceType }) {
  const [search, setSearch] = useState("")
  const routeSearch = useSearch({ strict: false })
  const tab = objectTab("tab" in routeSearch ? routeSearch.tab : undefined)
  const activePrimary = primaryTabOf(tab)
  const records = RECORDS.filter((record) => record.type === type)
  const term = search.trim().toLowerCase()
  const matches = records.filter(({ id, name, summary }) => `${id} ${name} ${summary}`.toLowerCase().includes(term))

  return (
    <section className="space-y-4">
      <Link to="/sources" search={{ view: "graph", model: "aggregates" }} className="inline-flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">
        Objects <ChevronRight className="size-3" /> Business model
      </Link>
      <FinanceHeading title={TYPES[type].plural} description={`Typed ontology definition and ${records.length} sample records.`} />
      <nav aria-label="Object type views" className="flex flex-wrap gap-1 border-b border-border">
        {PRIMARY_TABS.map(({ id, label }) => (
          <Link
            key={id}
            to="/sources/$concept"
            params={{ concept: type }}
            search={{ tab: id === "overview" ? undefined : id }}
            aria-current={activePrimary === id ? "page" : undefined}
            className={cn(
              "border-b-2 border-transparent px-3 py-2 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground",
              activePrimary === id && "border-data-accent text-foreground",
            )}
          >
            {label}
          </Link>
        ))}
      </nav>

      {tab !== "records" ? (
        <TypeDefinition type={type} tab={tab} />
      ) : (
        <div className="overflow-hidden rounded-lg border border-border bg-background">
          <div className="flex items-center justify-between gap-2 border-b border-border bg-surface-subtle px-4 py-2.5">
            <label className="relative block w-full max-w-xs">
              <span className="sr-only">Find a record</span>
              <Search className="pointer-events-none absolute start-2.5 top-2 size-3.5 text-muted-foreground" />
              <Input className="h-7 bg-background ps-8 text-xs" placeholder="Find a record…" value={search} onChange={(event) => setSearch(event.target.value)} />
            </label>
            <span className="text-xs text-muted-foreground tabular-nums">{matches.length} records</span>
          </div>
          <div className="overflow-x-auto">
            <table className="w-full text-start text-xs" aria-label={TYPES[type].plural}>
              <thead>
                <tr className="border-b border-border bg-surface-subtle/50 text-muted-foreground">
                  <th className="px-4 py-2 text-start font-medium">Record</th>
                  <th className="px-4 py-2 text-start font-medium">Key</th>
                  <th className="px-4 py-2 text-start font-medium">Summary</th>
                  <th className="px-4 py-2 text-end font-medium">Status</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {matches.map((record) => (
                  <tr key={record.id} className="transition-colors hover:bg-surface-subtle">
                    <td className="px-4 py-2.5">
                      <RecordLink record={record} className="inline-flex items-center gap-2 font-medium text-foreground hover:text-tone-blue">
                        <TypeTile type={type} />{record.name}
                      </RecordLink>
                    </td>
                    <td className="whitespace-nowrap px-4 py-2.5 font-mono text-[11px] text-muted-foreground">{record.id}</td>
                    <td className="px-4 py-2.5 tabular-nums text-foreground">{record.summary}</td>
                    <td className="px-4 py-2.5 text-end">
                      <Chip tone={["In review", "Draft"].includes(record.status) ? "amber" : "green"} dot>{record.status}</Chip>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {matches.length === 0 && <p className="p-4 text-xs text-muted-foreground">No sample records match this search.</p>}
          </div>
        </div>
      )}
    </section>
  )
}

function SourceBadge({ id }: { readonly id: MarkId }) {
  return (
    <span className="inline-flex items-center gap-1 whitespace-nowrap rounded-sm border border-border bg-surface-subtle px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
      <Mark id={id} className="size-3.5 rounded-xs border-0 text-[7px]" />{MARKS[id].name}
    </span>
  )
}

function Lineage({ fact }: { readonly fact: Fact }) {
  return (
    <aside aria-label="Fact lineage" className="flex flex-col border-t border-border bg-surface-subtle/35 md:border-s md:border-t-0">
      <header className="flex items-center gap-1.5 border-b border-border bg-surface-subtle px-4 py-2.5 text-xs font-semibold">
        <Waypoints className="size-3.5 text-tone-blue" />Source lineage
      </header>
      <div className="space-y-3 p-4 text-xs">
        <div className="flex items-center justify-between">
          <span className="text-[11px] text-muted-foreground">Origin system</span>
          <SourceBadge id={fact.source} />
        </div>
        <div>
          <p className="text-[11px] text-muted-foreground">Source column</p>
          <p className="mt-1 break-all rounded-md border border-border bg-background px-2.5 py-1.5 font-mono text-[11px]">{fact.path}</p>
        </div>
        <div className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
          <ArrowRight className="size-3 text-tone-blue" />{fact.mapping}
        </div>
        <div>
          <p className="text-[11px] text-muted-foreground">Domain property</p>
          <p className="mt-1 break-all rounded-md border border-data-accent/35 bg-tone-blue/[0.06] px-2.5 py-1.5 font-mono text-[11px] text-foreground">{fact.target}</p>
        </div>
        <p className="flex items-center gap-1.5 border-t border-border pt-2.5 text-[11px] text-muted-foreground">
          <Clock3 className="size-3.5" />As of {fact.at}
        </p>
      </div>
    </aside>
  )
}

const borrowerPositions = [[33, 21], [33, 75], [83, 10], [83, 31], [83, 52], [83, 73], [54, 87]] as const
const centredPositions = [[18, 25], [18, 73], [82, 21], [82, 48], [82, 78], [50, 88], [50, 12]] as const
const clientLinks = ["LN-1042-A", "LN-1042-B", "RM-1042", "CR-1042", "KYC-1042", "HC-1042", "BA-1042"]

function neighbours(record: FinanceRecord) {
  const direct = LINKS.flatMap(({ from, to, label }) => {
    const adjacent = from === record.id ? BY_ID.get(to) : to === record.id ? BY_ID.get(from) : undefined
    return adjacent ? [{ record: adjacent, label, anchor: record.id }] : []
  })
  if (record.id !== CLIENT.id) return direct
  const branches = direct.sort((a, b) => clientLinks.indexOf(a.record.id) - clientLinks.indexOf(b.record.id))
  return [
    ...branches,
    ...branches.filter(({ record: target }) => target.type === "loan").flatMap(({ record: loan }) =>
      LINKS.flatMap(({ from, to, label }) => {
        const installment = from === loan.id ? BY_ID.get(to) : undefined
        return installment?.type === "installment" ? [{ record: installment, label, anchor: loan.id }] : []
      }),
    ),
  ]
}

export function Neighbourhood({ record }: { readonly record: FinanceRecord }) {
  const related = neighbours(record)
  const wide = record.id === CLIENT.id
  const centre = wide ? [54, 48] as const : [50, 48] as const
  const layout = wide ? [...borrowerPositions, [10, 21] as const, [10, 75] as const] : centredPositions
  const nodes = related.slice(0, layout.length).map((entry, index) => ({ ...entry, position: layout[index] ?? centre }))
  const centreOf = (id: string) => nodes.find(({ record: adjacent }) => adjacent.id === id)?.position ?? centre
  return (
    <section className="overflow-hidden rounded-lg border border-border bg-background" aria-label="Record neighbourhood">
      <header className="flex items-center gap-2 border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">
        <Network className="size-3.5 text-tone-blue" />Linked records neighbourhood
      </header>
      <div className="relative grid gap-2 bg-surface-subtle/50 p-4 md:block md:h-[26rem] md:bg-[radial-gradient(var(--canvas-dot)_1px,transparent_1px)] md:bg-size-[16px_16px]">
        <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="absolute inset-0 hidden size-full md:block">
          {nodes.map(({ record: adjacent, anchor, position: [a, b] }) => {
            const [x, y] = centreOf(anchor)
            return <path key={adjacent.id} d={`M ${x} ${y} C ${(x + a) / 2} ${y}, ${(x + a) / 2} ${b}, ${a} ${b}`} fill="none" stroke="var(--edge)" strokeWidth="1.25" strokeOpacity={0.65} vectorEffect="non-scaling-stroke" />
          })}
        </svg>
        <div className="z-10 flex items-center gap-2 rounded-md border border-data-accent bg-background p-2.5 md:absolute md:w-[20%] md:-translate-1/2" style={{ left: `${centre[0]}%`, top: `${centre[1]}%` }}>
          <TypeTile type={record.type} className="border-data-accent/40 text-tone-blue" />
          <div className="min-w-0">
            <div className="truncate text-xs font-semibold">{record.name}</div>
            <code className="text-[10px] text-muted-foreground">{record.id}</code>
          </div>
        </div>
        {nodes.map(({ record: adjacent, label, position: [x, y] }) => (
          <Link key={adjacent.id} to="/sources/$concept/$" params={{ concept: adjacent.type, _splat: adjacent.id }}
            className="z-10 flex items-center gap-2 rounded-md border border-border bg-background p-2 transition-colors hover:border-data-accent md:absolute md:w-[19%] md:-translate-1/2" style={{ left: `${x}%`, top: `${y}%` }}>
            <TypeTile type={adjacent.type} />
            <span className="min-w-0">
              <span className="block truncate text-xs font-medium text-foreground">{TYPES[adjacent.type].name}</span>
              <span className="block truncate text-[10px] text-muted-foreground">{label} · {adjacent.summary}</span>
              <code className="block truncate text-[9px] text-muted-foreground">{adjacent.id}</code>
            </span>
          </Link>
        ))}
        {nodes.length === 0 && <p className="text-xs text-muted-foreground">No linked sample records.</p>}
      </div>
    </section>
  )
}

function PaymentAllocation({ record }: { readonly record: FinanceRecord }) {
  const installment = LINKS.find((link) => link.to === record.id && link.label === "is paid by")?.from
  const loanId = LINKS.find((link) => link.to === installment && link.label === "has")?.from
  const borrowerId = LINKS.find((link) => link.to === loanId && link.label === "holds")?.from
  const loan = loanId ? BY_ID.get(loanId) : undefined
  const borrower = borrowerId ? BY_ID.get(borrowerId) : undefined
  if (!loan || !borrower) return null
  return (
    <section aria-label="Payment allocation" className="flex flex-wrap items-center gap-2 rounded-lg border border-border bg-background px-4 py-2.5 text-xs">
      <span className="font-medium text-muted-foreground">Allocation:</span>
      {([[loan, "Applied to"], [borrower, "Paid by"]] as const).map(([target, label]) => (
        <RecordLink key={target.id} record={target} className="inline-flex items-center gap-1.5 rounded-md border border-border bg-surface-subtle px-2.5 py-1 hover:border-data-accent">
          <span className="text-muted-foreground">{label}</span>
          <ArrowRight className="size-3 text-tone-blue" />
          <strong className="font-medium text-foreground">{TYPES[target.type].name}</strong>
          <code className="text-muted-foreground">{target.id}</code>
        </RecordLink>
      ))}
    </section>
  )
}

export function RecordDetail({ record }: { readonly record: FinanceRecord }) {
  const { mode } = useFinanceMode()
  const [active, setActive] = useState<Fact>(record.facts[0]!)
  const [showLineage, setShowLineage] = useState(mode === "data")
  const history = record.history

  return (
    <section className="space-y-4">
      <nav aria-label="Record breadcrumbs" className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
        <Link to="/sources" className="hover:text-foreground">Objects</Link><ChevronRight className="size-3" />
        <Link to="/sources/$concept" params={{ concept: record.type }} search={{ tab: "records" }} className="hover:text-foreground">{TYPES[record.type].plural}</Link><ChevronRight className="size-3" />
        <span className="font-mono text-foreground" aria-current="page">{record.id}</span>
      </nav>
      <FinanceHeading
        title={record.name}
        description={`${TYPES[record.type].name} · ${record.id}`}
        actions={
          <div className="flex items-center gap-2">
            <Chip tone="green" dot>{record.status}</Chip>
            {record.type === "client" && <Chip tone="blue" icon={GitMerge}>Merged from 4 sources</Chip>}
            <button
              type="button"
              onClick={() => setShowLineage(!showLineage)}
              className="inline-flex h-7 items-center gap-1.5 rounded-md border border-border bg-background px-2.5 text-xs font-medium text-muted-foreground hover:border-data-accent hover:text-foreground"
            >
              <Database className="size-3" />{showLineage ? "Hide lineage" : "Inspect lineage"}
            </button>
          </div>
        }
      />

      <div className={cn("grid overflow-hidden rounded-lg border border-border bg-background", showLineage && "md:grid-cols-[minmax(0,1fr)_20rem]")}>
        <section aria-label="Facts" className="min-w-0">
          <header className="border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">Record attributes</header>
          <dl className="divide-y divide-border px-4">
            {record.facts.map((value) => (
              <div key={value.target} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_auto] items-center gap-3 py-2.5 text-xs">
                <dt className="text-muted-foreground">{value.label}</dt>
                <dd>
                  <button
                    type="button"
                    aria-label={`Show lineage for ${value.label}`}
                    aria-pressed={active === value}
                    onClick={() => { setActive(value); setShowLineage(true) }}
                    className={cn("rounded-sm px-1.5 py-0.5 text-left font-medium tabular-nums hover:bg-surface-subtle", active === value && showLineage && "bg-tone-blue/10 text-tone-blue")}
                  >
                    {value.value}
                  </button>
                </dd>
                <dd>{showLineage && <SourceBadge id={value.source} />}</dd>
              </div>
            ))}
          </dl>
        </section>
        {showLineage && <Lineage fact={active} />}
      </div>

      {record.type === "payment" && <PaymentAllocation record={record} />}

      {history && (
        <section aria-label="Score history" className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">I-Score history</header>
          <ol className="grid max-w-lg grid-cols-3 divide-x divide-border">
            {history.map(({ date, score }, index) => (
              <li key={date} className={cn("p-3.5", index === history.length - 1 && "bg-tone-blue/[0.04]")}>
                <div className="text-[11px] text-muted-foreground">{date}</div>
                <div className="mt-0.5 text-lg font-semibold tabular-nums text-foreground">{score}</div>
              </li>
            ))}
          </ol>
        </section>
      )}

      <Neighbourhood record={record} />
    </section>
  )
}
