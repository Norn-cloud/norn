import { Link, Navigate, useParams } from "@tanstack/react-router"
import { ArrowRight, ChevronRight, Clock3, GitMerge, Network, Search, Waypoints } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { Input } from "@/components/ui/input"
import type { MenuEntry } from "../shell.tsx"
import { cn } from "@/lib/utils"
import { CLIENT, MARKS, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { BY_ID, LINKS, RECORDS, isFinanceType, type Fact, type FinanceRecord } from "./records.ts"
import { FinanceHeading, Mark, TypeTile } from "./workspace.tsx"

export const financeSourceMenu = (): readonly MenuEntry[] =>
  Object.entries(TYPES).map(([type, definition]) => ({
    href: `/sources/${type}`, label: definition.name, group: "Object types", kind: "objectType",
    count: RECORDS.filter((record) => record.type === type).length,
  }))

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
  const records = RECORDS.filter((record) => record.type === type)
  const term = search.trim().toLowerCase()
  const matches = records.filter(({ id, name, summary }) => `${id} ${name} ${summary}`.toLowerCase().includes(term))
  return <section className="space-y-4">
    <Link to="/sources" search={{ view: "graph" }} className="inline-flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">Sources <ChevronRight className="size-3" /> Business model</Link>
    <FinanceHeading title={TYPES[type].plural} description={`${records.length} sample records in the finance workspace.`} />
    <div className="rounded-2xl border bg-card">
      <div className="border-b p-3">
        <label className="relative block max-w-sm"><span className="sr-only">Find a record</span><Search className="absolute start-2.5 top-2.5 size-4 text-muted-foreground" /><Input className="ps-8" placeholder="Find a record" value={search} onChange={(event) => setSearch(event.target.value)} /></label>
      </div>
      <div className="overflow-x-auto">
        <table className="w-full text-start text-sm" aria-label={TYPES[type].plural}>
          <thead><tr className="border-b bg-surface-subtle text-left text-xs font-medium text-muted-foreground"><th className="px-4 py-2">Record</th><th className="px-4 py-2">Key</th><th className="px-4 py-2">Summary</th><th className="px-4 py-2">Status</th></tr></thead>
          <tbody className="divide-y">
            {matches.map((record) => <tr key={record.id} className="hover:bg-muted/40">
              <td className="px-4 py-2.5"><RecordLink record={record} className="inline-flex items-center gap-2 font-medium hover:text-tone-blue"><TypeTile type={type} />{record.name}</RecordLink></td>
              <td className="whitespace-nowrap px-4 py-2.5 font-mono text-xs text-muted-foreground">{record.id}</td>
              <td className="px-4 py-2.5 tabular-nums">{record.summary}</td>
              <td className="px-4 py-2.5"><Chip tone={["In review", "Draft"].includes(record.status) ? "amber" : "green"} className="rounded-full">{record.status}</Chip></td>
            </tr>)}
          </tbody>
        </table>
        {matches.length === 0 && <p className="p-4 text-sm text-muted-foreground">No sample records match this search.</p>}
      </div>
    </div>
  </section>
}

function SourceBadge({ id }: { readonly id: MarkId }) {
  return <span className="inline-flex items-center gap-1 whitespace-nowrap rounded-full border bg-card px-1.5 py-0.5 text-[10px] font-medium">
    <Mark id={id} className="size-4 rounded-sm border-0 text-[7px]" />{MARKS[id].name}
  </span>
}

function Lineage({ fact }: { readonly fact: Fact }) {
  return <section aria-label="Fact lineage" className="min-w-0 rounded-2xl border bg-card p-4">
    <div className="mb-3 flex items-center gap-2"><Waypoints className="size-4 text-tone-blue" /><h3 className="text-sm font-semibold">Fact lineage</h3></div>
    <div className="space-y-2 text-xs">
      <SourceBadge id={fact.source} />
      <p className="break-all rounded-md border bg-surface-subtle p-2 font-mono">{fact.path}</p>
      <div className="flex items-center gap-2 text-muted-foreground"><ArrowRight className="size-3.5 text-tone-blue" />{fact.mapping}</div>
      <p className="break-all rounded-md border border-data-accent/30 bg-data-accent/10 p-2 font-mono">{fact.target}</p>
      <p className="flex items-center gap-2 pt-1 text-muted-foreground"><Clock3 className="size-3.5" />As of {fact.at}</p>
    </div>
  </section>
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
  return [...branches, ...branches.filter(({ record: target }) => target.type === "loan").flatMap(({ record: loan }) =>
    LINKS.flatMap(({ from, to, label }) => {
      const installment = from === loan.id ? BY_ID.get(to) : undefined
      return installment?.type === "installment" ? [{ record: installment, label, anchor: loan.id }] : []
    }))]
}

export function Neighbourhood({ record }: { readonly record: FinanceRecord }) {
  const related = neighbours(record)
  const wide = record.id === CLIENT.id
  const centre = wide ? [54, 48] as const : [50, 48] as const
  const layout = wide ? [...borrowerPositions, [10, 21] as const, [10, 75] as const] : centredPositions
  const nodes = related.slice(0, layout.length).map((entry, index) => ({ ...entry, position: layout[index] ?? centre }))
  const centreOf = (id: string) => nodes.find(({ record: adjacent }) => adjacent.id === id)?.position ?? centre
  return <section className="space-y-3" aria-label="Record neighbourhood">
    <div className="flex items-center gap-2"><Network className="size-4 text-tone-blue" /><h2 className="text-section font-semibold">Neighbourhood</h2></div>
    <div className="relative grid gap-2 rounded-2xl border bg-surface-subtle p-3 md:block md:h-[28rem] md:bg-[radial-gradient(var(--border)_1px,transparent_1px)] md:bg-size-[16px_16px]">
      <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="absolute inset-0 hidden size-full md:block">
        {nodes.map(({ record: adjacent, anchor, position: [a,b] }) => {
          const [x,y]=centreOf(anchor)
          return <path key={adjacent.id} d={`M ${x} ${y} C ${(x+a)/2} ${y}, ${(x+a)/2} ${b}, ${a} ${b}`} fill="none" stroke="var(--data-accent)" strokeWidth="1.4" strokeOpacity={0.7} vectorEffect="non-scaling-stroke" />
        })}
      </svg>
      <div className="z-10 flex items-center gap-2 rounded-xl border border-data-accent bg-card p-2.5 ring-4 ring-data-accent/15 md:absolute md:w-[20%] md:-translate-1/2" style={{left:`${centre[0]}%`,top:`${centre[1]}%`}}>
        <TypeTile type={record.type} /><div className="min-w-0"><div className="truncate text-xs font-semibold">{record.name}</div><code className="text-[10px] text-muted-foreground">{record.id}</code></div>
      </div>
      {nodes.map(({ record: adjacent, label, position:[x,y] }) => <Link key={adjacent.id} to="/sources/$concept/$" params={{ concept: adjacent.type, _splat: adjacent.id }}
        className="z-10 flex items-center gap-2 rounded-xl border bg-card p-2 shadow-sm hover:border-data-accent hover:shadow-md md:absolute md:w-[19%] md:-translate-1/2" style={{left:`${x}%`,top:`${y}%`}}>
        <TypeTile type={adjacent.type} /><span className="min-w-0"><span className="block truncate text-xs font-semibold">{TYPES[adjacent.type].name}</span><span className="block truncate text-[10px] text-muted-foreground">{label} · {adjacent.summary}</span><code className="block truncate text-[9px] text-muted-foreground">{adjacent.id}</code></span>
      </Link>)}
      {nodes.length === 0 && <p className="text-sm text-muted-foreground">No linked sample records.</p>}
    </div>
  </section>
}

export function RecordDetail({ record }: { readonly record: FinanceRecord }) {
  const [active, setActive] = useState<Fact>(record.facts[0]!)
  const history = record.history
  return <section className="space-y-4">
    <nav aria-label="Record breadcrumbs" className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
      <Link to="/sources" className="hover:text-foreground">Sources</Link><ChevronRight className="size-3" />
      <Link to="/sources/$concept" params={{concept:record.type}} className="hover:text-foreground">{TYPES[record.type].plural}</Link><ChevronRight className="size-3" />
      <span className="text-foreground" aria-current="page">{record.id}</span>
    </nav>
    <FinanceHeading title={record.name} description={`${TYPES[record.type].name} · ${record.id}`} />
    <div className="flex flex-wrap items-center gap-2">
      <TypeTile type={record.type} /><Chip tone="green" className="rounded-full">{record.status}</Chip>
      {record.type === "client" && <Chip tone="blue" icon={GitMerge} className="rounded-full">Merged from 4 sources on National ID</Chip>}
    </div>
    <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_minmax(13rem,0.5fr)]">
      <section aria-label="Facts" className="min-w-0 rounded-2xl border bg-card p-4">
        <h2 className="mb-2 text-sm font-semibold">Facts</h2>
        <dl className="divide-y">
          {record.facts.map((value) => <div key={value.target} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] items-center gap-2 py-2 text-xs">
            <dt className="text-muted-foreground">{value.label}</dt>
            <dd><button type="button" aria-label={`Show lineage for ${value.label}`} aria-pressed={active === value} onClick={() => setActive(value)} className={cn("rounded px-1 text-left font-medium tabular-nums hover:bg-muted", active === value && "bg-data-accent/10 text-tone-blue")}>{value.value}</button></dd>
            <dd><SourceBadge id={value.source} /></dd>
          </div>)}
        </dl>
      </section>
      <Lineage fact={active} />
    </div>
    {history && <section aria-label="Score history" className="rounded-2xl border bg-card p-4">
      <h2 className="mb-3 text-sm font-semibold">I-Score history</h2>
      <ol className="grid max-w-lg grid-cols-3 gap-2">{history.map(({ date, score }, index) => <li key={date} className={cn("rounded-lg border bg-surface-subtle p-3", index === history.length - 1 && "border-data-accent bg-data-accent/10")}>
        <div className="text-[11px] text-muted-foreground">{date}</div><div className="text-lg font-semibold tabular-nums">{score}</div>
      </li>)}</ol>
    </section>}
    <Neighbourhood record={record} />
  </section>
}
