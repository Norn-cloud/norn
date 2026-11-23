import { Link, Navigate, useParams, useSearch } from "@tanstack/react-router"
import { ArrowRight, Clock3, GitMerge, Layers, Network, Search, Waypoints } from "lucide-react"
import { useEffect, useState } from "react"
import { Chip } from "@/components/chip"
import { PageHeader, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { useInspector } from "@/lib/inspector"
import { cn } from "@/lib/utils"
import type { MenuEntry } from "../shell.tsx"
import { CLIENT, MARKS, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { PRIMARY_TABS, objectTab, primaryTabOf, TypeDefinition } from "./definitions.tsx"
import { BY_ID, LINKS, RECORDS, isFinanceType, type Fact, type FinanceRecord } from "./records.ts"
import { Mark, TypeTile } from "./workspace.tsx"

/** Workspace sidebar menu: each object type opens its records list; also claims its record detail pages. */
export const financeRecordsMenu = (): readonly MenuEntry[] =>
  (Object.entries(TYPES) as [FinanceType, (typeof TYPES)[FinanceType]][]).map(([type, definition]) => ({
    href: `/objects/${type}`,
    also: `/sources/${type}`,
    label: definition.plural,
    kind: "objectType",
    count: RECORDS.filter((record) => record.type === type).length,
  }))

/** Builder sidebar menu: each object type opens its schema, behavior and governance definition. */
export const financeSourceMenu = (): readonly MenuEntry[] =>
  (Object.entries(TYPES) as [FinanceType, (typeof TYPES)[FinanceType]][]).map(([type, definition]) => ({
    href: `/sources/${type}`,
    label: definition.name,
    group: "Object types",
    kind: "objectType",
    count: RECORDS.filter((record) => record.type === type).length,
  }))

export function RecordLink({ record, children, className }: {
  readonly record: FinanceRecord; readonly children: React.ReactNode; readonly className?: string
}) {
  return <Link to="/sources/$concept/$" params={{ concept: record.type, _splat: record.id }} className={className}>{children}</Link>
}

export function FinanceRecordsListRoute() {
  const { type } = useParams({ strict: false })
  if (!isFinanceType(type)) return <Navigate to="/objects/$type" params={{ type: "client" }} replace />
  return <RecordsListPage key={type} type={type} />
}

export function FinanceRecordRoute() {
  const { concept, _splat, type } = useParams({ strict: false })
  const name = concept ?? type
  if (!isFinanceType(name)) return <Navigate to="/sources" replace />
  const record = typeof _splat === "string" ? BY_ID.get(_splat) : undefined
  if (_splat && record?.type !== name) return <Navigate to="/sources/$concept" params={{ concept: name }} replace />
  return record?.type === name ? <RecordDetail key={record.id} record={record} /> : <TypeList key={name} type={name} />
}

function RecordsTable({ type }: { readonly type: FinanceType }) {
  const [search, setSearch] = useState("")
  const records = RECORDS.filter((record) => record.type === type)
  const term = search.trim().toLowerCase()
  const matches = records.filter(({ id, name, summary }) => `${id} ${name} ${summary}`.toLowerCase().includes(term))

  return (
    <Panel
      label={TYPES[type].plural}
      action={
        <div className="flex items-center gap-3">
          <label className="relative block w-52">
            <span className="sr-only">Find a record</span>
            <Search className="pointer-events-none absolute start-2.5 top-2 size-3.5 text-muted-foreground" />
            <Input className="h-7 ps-8 text-xs" placeholder="Find a record…" value={search} onChange={(event) => setSearch(event.target.value)} />
          </label>
          <span className="tabular-nums">{matches.length}</span>
        </div>
      }
    >
      <TableCard>
        <Table aria-label={TYPES[type].plural}>
          <TableHeader>
            <TableRow>
              <TableHead>Record</TableHead>
              <TableHead>Key</TableHead>
              <TableHead>Summary</TableHead>
              <TableHead className="text-end">Status</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {matches.map((record) => (
              <TableRow key={record.id}>
                <TableCell>
                  <RecordLink record={record} className="inline-flex items-center gap-2 font-medium text-foreground hover:underline underline-offset-4">
                    <TypeTile type={type} />{record.name}
                  </RecordLink>
                </TableCell>
                <TableCell className="whitespace-nowrap font-mono text-xs text-muted-foreground">{record.id}</TableCell>
                <TableCell className="text-foreground">{record.summary}</TableCell>
                <TableCell className="text-end">
                  <Chip tone={["In review", "Draft"].includes(record.status) ? "amber" : "green"} dot>{record.status}</Chip>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        {matches.length === 0 && <p className="p-4 text-meta text-muted-foreground">No sample records match this search.</p>}
      </TableCard>
    </Panel>
  )
}

function RecordsListPage({ type }: { readonly type: FinanceType }) {
  const count = RECORDS.filter((record) => record.type === type).length
  return (
    <section className="space-y-6">
      <PageHeader
        title={TYPES[type].plural}
        meta={`${count} records`}
        description={`Resolved ${TYPES[type].name.toLowerCase()} records across connected systems. Select a record to inspect facts, lineage, and links.`}
        secondary={[{ label: "Open type definition", icon: Layers, to: `/sources/${type}` }]}
      />
      <RecordsTable type={type} />
    </section>
  )
}

function TypeList({ type }: { readonly type: FinanceType }) {
  const routeSearch = useSearch({ strict: false })
  const tab = objectTab("tab" in routeSearch ? routeSearch.tab : undefined)
  const activePrimary = primaryTabOf(tab)
  const records = RECORDS.filter((record) => record.type === type)

  return (
    <section className="space-y-6">
      <PageHeader
        title={TYPES[type].plural}
        meta={`${TYPES[type].name} · ${records.length} sample records`}
        description="Typed model definition, properties, relationships, behavior, and governing rules."
        primary={tab !== "records" ? <Button asChild size="sm"><Link to="/objects/$type" params={{ type }}>Browse records</Link></Button> : undefined}
      />
      <nav aria-label="Object type views" className="flex flex-wrap gap-1 border-b">
        {PRIMARY_TABS.map(({ id, label }) => (
          <Link
            key={id}
            to="/sources/$concept"
            params={{ concept: type }}
            search={{ tab: id === "overview" ? undefined : id }}
            aria-current={activePrimary === id ? "page" : undefined}
            className={cn(
              "border-b-2 border-transparent px-3 py-2 text-meta font-medium text-muted-foreground transition-colors hover:text-foreground",
              activePrimary === id && "border-foreground text-foreground",
            )}
          >
            {label}
          </Link>
        ))}
      </nav>

      {tab !== "records" ? <TypeDefinition type={type} tab={tab} /> : <RecordsTable type={type} />}
    </section>
  )
}

function SourceBadge({ id }: { readonly id: MarkId }) {
  return (
    <span className="inline-flex items-center gap-1.5 whitespace-nowrap rounded-sm border bg-muted/30 px-1.5 py-0.5 text-xs text-muted-foreground">
      <Mark id={id} className="size-3.5 rounded-xs border-0 bg-transparent text-[7px]" />{MARKS[id].name}
    </span>
  )
}

function LineageBody({ fact }: { readonly fact: Fact }) {
  return (
    <div aria-label="Fact lineage" className="divide-y text-meta">
      <div className="flex items-center justify-between gap-2 pb-3">
        <span className="text-xs text-muted-foreground">Origin system</span>
        <SourceBadge id={fact.source} />
      </div>
      <div className="py-3">
        <p className="text-xs text-muted-foreground">Source column</p>
        <p className="mt-1 break-all font-mono text-xs text-foreground">{fact.path}</p>
      </div>
      <div className="py-3">
        <p className="text-xs text-muted-foreground">Mapping rule</p>
        <p className="mt-1 text-foreground">{fact.mapping}</p>
      </div>
      <div className="py-3">
        <p className="text-xs text-muted-foreground">Domain property</p>
        <p className="mt-1 break-all font-mono text-xs text-foreground">{fact.target}</p>
      </div>
      <div className="py-3">
        <p className="text-xs text-muted-foreground">Value</p>
        <p className="mt-1 font-medium tabular-nums text-foreground">{fact.value}</p>
      </div>
      <p className="flex items-center gap-1.5 pt-3 text-xs text-muted-foreground">
        <Clock3 className="size-3.5" />As of {fact.at}
      </p>
    </div>
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
    <Panel icon={Network} label="Linked records" action={`${nodes.length} connected`}>
      <Well>
        <div aria-label="Record neighbourhood" className="relative grid gap-2 bg-canvas-surface p-4 md:block md:h-[26rem] md:bg-[radial-gradient(var(--canvas-dot)_1px,transparent_1px)] md:bg-size-[16px_16px]">
          <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="absolute inset-0 hidden size-full md:block">
            {nodes.map(({ record: adjacent, anchor, position: [a, b] }) => {
              const [x, y] = centreOf(anchor)
              return <path key={adjacent.id} d={`M ${x} ${y} C ${(x + a) / 2} ${y}, ${(x + a) / 2} ${b}, ${a} ${b}`} fill="none" stroke="var(--edge)" strokeWidth="1.25" strokeOpacity={0.55} vectorEffect="non-scaling-stroke" />
            })}
          </svg>
          <div className="z-10 flex items-center gap-2 rounded-md border border-foreground/40 bg-card p-2.5 md:absolute md:w-[20%] md:-translate-1/2" style={{ left: `${centre[0]}%`, top: `${centre[1]}%` }}>
            <TypeTile type={record.type} className="text-foreground" />
            <div className="min-w-0">
              <div className="truncate text-meta font-medium">{record.name}</div>
              <code className="text-[11px] text-muted-foreground">{record.id}</code>
            </div>
          </div>
          {nodes.map(({ record: adjacent, label, position: [x, y] }) => (
            <Link key={adjacent.id} to="/sources/$concept/$" params={{ concept: adjacent.type, _splat: adjacent.id }}
              className="z-10 flex items-center gap-2 rounded-md border bg-card p-2 transition-colors hover:border-foreground/40 md:absolute md:w-[19%] md:-translate-1/2" style={{ left: `${x}%`, top: `${y}%` }}>
              <TypeTile type={adjacent.type} />
              <span className="min-w-0">
                <span className="block truncate text-xs font-medium text-foreground">{TYPES[adjacent.type].name}</span>
                <span className="block truncate text-[11px] text-muted-foreground">{label} · {adjacent.summary}</span>
                <code className="block truncate text-[10px] text-muted-foreground">{adjacent.id}</code>
              </span>
            </Link>
          ))}
          {nodes.length === 0 && <p className="text-meta text-muted-foreground">No linked sample records.</p>}
        </div>
      </Well>
    </Panel>
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
    <Panel label="Payment allocation">
      <Well>
        <div aria-label="Payment allocation" className="flex flex-wrap items-center gap-4 px-4 py-3 text-meta">
          {([[loan, "Applied to"], [borrower, "Paid by"]] as const).map(([target, label]) => (
            <RecordLink key={target.id} record={target} className="inline-flex items-center gap-1.5 text-foreground hover:underline underline-offset-4">
              <span className="text-muted-foreground">{label}</span>
              <ArrowRight className="size-3 text-muted-foreground" />
              <span className="font-medium">{TYPES[target.type].name}</span>
              <code className="text-xs text-muted-foreground">{target.id}</code>
            </RecordLink>
          ))}
        </div>
      </Well>
    </Panel>
  )
}

export function RecordDetail({ record }: { readonly record: FinanceRecord }) {
  const [active, setActive] = useState<Fact>(record.facts[0]!)
  const { inspected, inspect } = useInspector()
  const history = record.history

  useEffect(() => () => inspect(), [inspect])

  const openLineage = (value: Fact) => {
    setActive(value)
    inspect({ title: `Source lineage · ${value.label}`, body: <LineageBody fact={value} /> })
  }

  return (
    <section className="space-y-6">
      <PageHeader
        title={record.name}
        meta={
          <>
            <code className="font-mono">{record.id}</code>
            <Chip tone={["In review", "Draft"].includes(record.status) ? "amber" : "green"} dot>{record.status}</Chip>
            {record.type === "client" && <Chip tone="gray" icon={GitMerge}>Merged from 4 sources</Chip>}
          </>
        }
        description={TYPES[record.type].name}
        actions={
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => inspected ? inspect() : openLineage(active)}
          >
            <Waypoints className="size-3.5" />{inspected ? "Close lineage" : "Inspect lineage"}
          </Button>
        }
        secondary={[{ label: "Open type definition", icon: Layers, to: `/sources/${record.type}` }]}
      />

      <Panel label="Record attributes" action="Select any value to inspect its source lineage">
        <Well>
          <dl aria-label="Facts" className="divide-y px-4">
            {record.facts.map((value) => {
              const selected = inspected !== undefined && active === value
              return (
                <div key={value.target} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_auto] items-center gap-3 py-2.5 text-meta">
                  <dt className="text-muted-foreground">{value.label}</dt>
                  <dd>
                    <button
                      type="button"
                      aria-label={`Show lineage for ${value.label}`}
                      aria-pressed={selected}
                      onClick={() => openLineage(value)}
                      className={cn(
                        "rounded-sm px-1.5 py-0.5 text-start font-medium tabular-nums transition-colors hover:bg-muted",
                        selected && "bg-muted underline underline-offset-4",
                      )}
                    >
                      {value.value}
                    </button>
                  </dd>
                  <dd><SourceBadge id={value.source} /></dd>
                </div>
              )
            })}
          </dl>
        </Well>
      </Panel>

      {record.type === "payment" && <PaymentAllocation record={record} />}

      {history && (
        <Panel label="I-Score history" action="Last 3 bureau pulls">
          <Well>
            <ol aria-label="Score history" className="grid max-w-lg grid-cols-3 divide-x">
              {history.map(({ date, score }, index) => (
                <li key={date} className={cn("px-4 py-3", index === history.length - 1 && "bg-muted/35")}>
                  <div className="text-xs text-muted-foreground">{date}</div>
                  <div className="mt-1 text-lg font-medium tabular-nums text-foreground">{score}</div>
                </li>
              ))}
            </ol>
          </Well>
        </Panel>
      )}

      <Neighbourhood record={record} />
    </section>
  )
}
