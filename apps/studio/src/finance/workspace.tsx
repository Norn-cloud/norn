import { Link, useSearch } from "@tanstack/react-router"
import { ArrowRight, BadgeCheck, BriefcaseBusiness, Building2, CalendarClock, ClipboardList, CreditCard, Database, FileCheck, Files, Gauge, GitMerge, Landmark, LifeBuoy, Network, Receipt, UserRound, type LucideIcon } from "lucide-react"
import { useState } from "react"
import { Chip, IconTile } from "@/components/chip"
import { cn } from "@/lib/utils"
import { CLIENT_FIELDS, MARKS, ROW_COUNT, SYSTEMS, TYPES, type FinanceType, type MarkId } from "./data.ts"

const glyphs: Record<FinanceType, LucideIcon> = {
  client: UserRound, loan: Landmark, installment: CalendarClock, payment: CreditCard,
  "credit-report": Gauge, "kyc-check": FileCheck, "relationship-manager": BriefcaseBusiness,
  "hardship-case": LifeBuoy, "brokerage-account": Building2, order: ClipboardList, "regulatory-return": Receipt,
}

export function TypeTile({ type }: { readonly type: FinanceType }) {
  const Icon = glyphs[type]
  return <IconTile tone={TYPES[type].tone}><Icon /></IconTile>
}

export function Mark({ id, className }: { readonly id: MarkId; readonly className?: string }) {
  const { name, tone, file, letters } = MARKS[id]
  const [loaded, setLoaded] = useState(false)
  return <span title={name} aria-label={name} role="img" className={cn("relative inline-flex size-8 shrink-0 items-center justify-center rounded-lg border bg-card text-[9px] font-semibold", className)} style={{ color: `var(--tone-${tone})`, background: `color-mix(in oklab,var(--tone-${tone}) 13%,var(--card))` }}>
    <span aria-hidden className={loaded ? "invisible" : undefined}>{letters ?? name.slice(0, 2).toUpperCase()}</span>
    {file && <img src={`/finance-marks/${file}`} alt="" className={cn("absolute inset-0 size-full object-contain p-[12%]", !loaded && "opacity-0")} onLoad={() => setLoaded(true)} />}
  </span>
}

export function FinanceHeading({ title, description }: { readonly title: string; readonly description: string }) {
  return <header className="flex flex-wrap items-start justify-between gap-3">
    <div><h1 className="text-title font-semibold tracking-tight">{title}</h1><p className="mt-1 max-w-2xl text-sm text-muted-foreground">{description}</p></div>
    <Chip tone="gray" className="rounded-full">Synthetic example</Chip>
  </header>
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

function ModelGraph() {
  return <section aria-label="Lending model graph" className="relative grid gap-3 rounded-2xl border bg-surface-subtle p-3 md:block md:h-[32rem] md:bg-[radial-gradient(var(--border)_1px,transparent_1px)] md:bg-size-[16px_16px]">
    <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="absolute inset-0 hidden size-full md:block">
      {EDGES.map(({ from: [x, y], to: [a, b], text }) => <path key={text} d={x === a ? `M ${x} ${y} V ${b}` : `M ${x} ${y} C ${(x + a) / 2} ${y}, ${(x + a) / 2} ${b}, ${a} ${b}`}
        fill="none" stroke="var(--data-accent)" strokeOpacity={0.8} strokeWidth="1.5" vectorEffect="non-scaling-stroke" />)}
    </svg>
    {EDGES.map(({ text, label: [x, y] }) => <span key={text} className="pointer-events-none absolute z-10 hidden -translate-x-1/2 -translate-y-1/2 whitespace-nowrap rounded-full border bg-card px-1.5 py-0.5 text-[9px] text-muted-foreground md:block" style={{ left: `${x}%`, top: `${y}%` }}>{text}</span>)}
    <article className="z-20 space-y-3 rounded-2xl border border-data-accent bg-card p-3 shadow-sm ring-4 ring-data-accent/15 md:absolute md:top-1/2 md:left-1/2 md:w-[26%] md:-translate-1/2">
      <div className="flex flex-wrap items-center justify-between gap-1.5">
        <div className="flex items-center gap-2"><TypeTile type="client" /><h3 className="text-sm font-semibold"><Link to="/sources/$concept" params={{ concept: "client" }} className="hover:text-tone-blue">Borrower</Link></h3></div>
        <div aria-label="Four source systems" className="flex -space-x-1">{CLIENT_FIELDS.map(({ mark }) => <Mark key={mark} id={mark} className="size-7 text-[8px] ring-1 ring-card" />)}</div>
      </div>
      <Chip tone="blue" icon={GitMerge} className="rounded-full">Merged</Chip>
      <ul className="grid gap-1.5">
        {CLIENT_FIELDS.map(({ mark, field }) => <li key={field} className="flex items-center gap-1.5"><Mark id={mark} className="size-6 text-[8px]" /><code className="text-xs">{field}</code></li>)}
      </ul>
      <div className="border-t pt-2 text-[10px] text-muted-foreground">Matching key · National ID</div>
    </article>
    {NODES.map(({ type, x, y, field }) => <article key={type} className="rounded-xl border bg-card p-3 shadow-sm md:absolute md:w-[26%] md:-translate-1/2" style={{ left: `${x}%`, top: `${y}%` }}>
      <div className="flex items-center gap-2"><TypeTile type={type} /><h3 className="truncate text-xs font-semibold md:text-sm"><Link to="/sources/$concept" params={{ concept: type }} className="hover:text-tone-blue">{TYPES[type].name}</Link></h3></div>
      <div className="mt-1.5 flex items-center gap-1.5"><Mark id={TYPES[type].mark} className="size-6 rounded-md text-[8px]" /><code className="truncate text-[10px] text-muted-foreground">{field}</code></div>
    </article>)}
  </section>
}

const activeTab = "border-data-accent bg-data-accent/10 text-foreground"
const tabClass = "inline-flex items-center gap-1.5 rounded-lg border border-transparent px-3 py-1.5 text-sm font-medium text-muted-foreground hover:bg-muted"

export function FinanceSources() {
  const search = useSearch({ strict: false })
  const graph = "view" in search && search.view === "graph"
  return <section className="space-y-4">
    <FinanceHeading title="Sources" description="Seven systems mapped to one client, lending and payments model." />
    <nav aria-label="Sources views" className="flex flex-wrap gap-1 rounded-xl border bg-card p-1">
      <Link to="/sources" search={{ view: undefined }} className={cn(tabClass, !graph && activeTab)} aria-current={!graph ? "page" : undefined}><Database className="size-4" />Mapped systems</Link>
      <Link to="/sources" search={{ view: "graph" }} className={cn(tabClass, graph && activeTab)} aria-current={graph ? "page" : undefined}><Network className="size-4" />Graph</Link>
    </nav>
    {graph ? <>
      <div className="space-y-1"><h2 className="text-section font-semibold">How your lending data fits together</h2><p className="text-xs text-muted-foreground">Four feeds resolve to one borrower on National ID.</p></div>
      <ModelGraph />
      <div className="flex flex-wrap gap-1.5">{Object.entries(TYPES).map(([type, item]) => <Link key={type} to="/sources/$concept" params={{ concept: type }}><Chip tone={item.tone} className="rounded-full hover:ring-1 hover:ring-border">{item.name}</Chip></Link>)}</div>
    </> : <>
      <div className="flex flex-wrap gap-x-6 gap-y-2 rounded-xl border bg-surface-subtle px-3.5 py-2.5 text-xs text-muted-foreground">
        <span><strong className="font-semibold text-foreground">7</strong> mapped systems</span>
        <span><strong className="font-semibold text-foreground">{SYSTEMS.reduce((count, item) => count + item.datasets.length, 0)}</strong> datasets</span>
        <span><strong className="font-semibold text-foreground">{Object.keys(TYPES).length}</strong> object types</span>
        <span className="ms-auto inline-flex items-center gap-1.5"><BadgeCheck className="size-3.5 text-tone-green" />22 Nov 2026 · Cairo time</span>
      </div>
      <div className="grid gap-3 lg:grid-cols-2">
        {SYSTEMS.map(({ id, name, marks, schema, datasets, syncedAt }) => <article key={id} aria-label={name} className="space-y-2.5 rounded-2xl border bg-card p-3.5">
          <div className="flex items-start justify-between gap-3">
            <div className="flex items-center gap-2.5">
              <div className="flex -space-x-1">{marks.map((mark) => <Mark key={mark} id={mark} />)}</div>
              <div><h2 className="text-sm font-semibold">{name}</h2><p className="text-[10px] text-muted-foreground">{marks.map((mark) => MARKS[mark].name).join(" · ")}</p></div>
            </div>
            <span className="flex shrink-0 items-center gap-1 whitespace-nowrap text-[10px] text-muted-foreground"><CalendarClock className="size-3" />Last sync {syncedAt}</span>
          </div>
          <div className="flex items-center gap-2 text-[11px]"><span className="text-muted-foreground">Schema</span><code className="rounded bg-muted px-1.5 py-0.5">{schema}</code></div>
          <dl className="divide-y rounded-lg border bg-surface-subtle/60 px-2.5">
            {datasets.map(([dataset, rows]) => <div key={dataset} className="flex items-center justify-between gap-2 py-1 text-[11px]"><dt className="flex items-center gap-1.5"><Files className="size-3 text-muted-foreground" />{dataset}</dt><dd className="tabular-nums text-muted-foreground">{ROW_COUNT.format(rows)} records</dd></div>)}
          </dl>
        </article>)}
        <div className="flex flex-col items-start justify-between gap-4 rounded-2xl border bg-surface-subtle p-4">
          <div className="space-y-1"><h2 className="flex items-center gap-2 text-sm font-semibold"><Mark id="aws" className="size-6" />Cloud infrastructure</h2><p className="text-xs text-muted-foreground">AWS hosts the example data platform.</p></div>
          <Link to="/sources" search={{ view: "graph" }} className="inline-flex items-center gap-1.5 text-sm font-medium text-tone-blue hover:underline">Explore the business model <ArrowRight className="size-4" /></Link>
        </div>
      </div>
    </>}
  </section>
}
