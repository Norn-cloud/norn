import { financeDemo } from "./lib/finance-demo.ts"
import { useQuery } from "@tanstack/react-query"
import { MarkingChips, useMarkingCatalog } from "@/components/marking-chip"
import { Link, useParams } from "@tanstack/react-router"
import { ArrowRight, Database, Search, Shapes } from "lucide-react"
import { useMemo, useState, type ReactNode } from "react"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { Chip } from "@/components/chip"
import { ObjectTile, objectAppearance } from "@/components/object-glyph"
import { SourceChip, SourceTile } from "@/components/source-chip"
import { EmptyState, PageHeader, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { useView, type View } from "@/lib/view"
import { catalogOf, cellOwned, conceptTitle, type Catalog, type Concept, type Source } from "./lib/catalog.ts"
import { transport } from "./lib/transport.ts"
import type { Total } from "./lib/records-query.ts"
import { LawsPanel } from "./laws.tsx"
import { SourceConnections } from "./source-connections.tsx"
import { ModelGraph } from "./model-graph.tsx"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { RecordsGrid } from "./grid/records-grid.tsx"
import type { MenuEntry } from "./shell.tsx"
import { Iso } from "@/components/iso"
import { directionOf, useLocale } from "@/lib/locale"

/** The "dense, crisp" tier for data tables: theme radius, 13px tabular figures, hairlines, grey header, no zebra. */
export function DenseTable({ head, children }: { readonly head: ReactNode; readonly children: ReactNode }) {
  return (
    <div className="overflow-hidden rounded-lg border">
      <Table className="text-[0.8125rem] tabular-nums">
        <TableHeader className="bg-muted">{head}</TableHeader>
        <TableBody>{children}</TableBody>
      </Table>
    </div>
  )
}

export const RECORDED_HINT = financeDemo ? "Synthetic finance examples from recorded data." : "Documents the engine produced, replayed exactly. Not a live connection."

const EXAMPLE_STACK = [
  "lms", "bank-settlement", "aws", "python", "postgresql", "fastapi", "langgraph", "grafana", "kafka", "iceberg", "mlflow",
  "spark", "airflow", "scikit-learn", "gitlab", "kubernetes", "openshift", "ollama", "pgvector",
  "power-bi", "tableau", "salesforce", "i-score", "instapay", "fawry", "egx", "mcdr", "fra",
]

function Systems({ catalog, recorded }: { readonly catalog: Catalog; readonly recorded: boolean }) {
  const concepts = catalog.concepts.filter(({ definition }) => definition === "object type")
  const relationships = concepts.reduce((total, { relationships }) => total + relationships.length, 0)
  const outside = catalog.systems.filter((system) => !cellOwned(system))
  const own = catalog.systems.filter(cellOwned)
  const systemsLabel = recorded ? "Mapped sample systems" : "Mapped systems"
  return (
    <div className="space-y-3">
      <StatStrip>
        <KpiTile icon={Database} label={systemsLabel} value={outside.length} sub={outside.length ? <span className="flex flex-wrap gap-1">{outside.map(({ id, name }) => <SourceChip key={id} system={name} />)}</span> : "none mapped yet"} />
        <KpiTile icon={Shapes} label="One business model" value={concepts.length} sub={`object types, built on ${catalog.concepts.length - concepts.length} shared interfaces`} />
        <KpiTile icon={ArrowRight} label="Links between them" value={relationships} sub="declared once, read the same from every system" />
      </StatStrip>
      <Panel icon={Database} label={systemsLabel}>
        <Well className="divide-y divide-border/70">
          {outside.length === 0 && <EmptyState icon={Database} title="No mapped system">No admitted mapping feeds this cell from a source system yet.</EmptyState>}
          {outside.map((system) => (
            <div key={system.id} className="flex items-center gap-3 px-4 py-2.5">
              <SourceTile system={system.name} />
              <span className="min-w-0 flex-1">
                <span className="block font-medium">{system.name}</span>
                {system.schema && <span className="block truncate text-xs text-muted-foreground">
                  {[system.schema.instance && `instance ${system.schema.instance}`, system.schema.version && `schema ${system.schema.version}`, system.schema.observedAt && `observed ${system.schema.observedAt}`].filter(Boolean).join(" · ")}
                </span>}
              </span>
              <span className="flex shrink-0 flex-wrap justify-end gap-1.5 tabular-nums" title={`${system.datasets.length} kinds of record · feeds ${system.feeds.length} object types`}>
                <Chip tone="gray">{system.datasets.length} kinds of record</Chip>
                <Chip tone="violet" icon={ArrowRight}>{system.feeds.length} object types</Chip>
              </span>
            </div>
          ))}
        </Well>
        {own.length > 0 && (
          <p className="border-t border-border/70 px-4 py-2 text-meta text-muted-foreground">
            Kept by Norn: {own.map((system) => `${system.datasets.length} kinds of record about its own syncs (${system.feeds.length} object types)`).join("; ")}. Not a connected system.
          </p>
        )}
      </Panel>
    </div>
  )
}

/** Business shows a type's plain one-line summary; Technical shows the registry's own description and the type's id. */
const about = (concept: Concept, view: View): { readonly text: string; readonly lang: string } | undefined => {
  // The registry's own description is English; a summary carries the language it was written in.
  if (view === "technical" && concept.description !== undefined) return { text: concept.description, lang: "en" }
  return concept.summary === undefined ? undefined : { text: concept.summary, lang: concept.summaryLang }
}

function ConceptCard({ concept }: { readonly concept: Concept }) {
  const systems = [...new Set(concept.feeds.map(({ system }) => system))]
  const { view } = useView()
  return (
    <Link to="/sources/$concept" params={{ concept: concept.name }} className="h-full rounded-lg text-start outline-none focus-visible:ring-3 focus-visible:ring-ring/30">
      <Card size="sm" className="h-full ring-foreground/10 transition-colors hover:ring-foreground/25">
        <CardHeader>
          <CardTitle lang={concept.titleLang} className="flex items-center gap-2.5"><ObjectTile name={concept.name} shared={concept.definition === "interface"} />{concept.title}</CardTitle>
          <CardDescription lang={about(concept, view)?.lang} className="line-clamp-2 min-h-[2lh]">{about(concept, view)?.text}</CardDescription>
        </CardHeader>
        <CardContent className="mt-auto space-y-3 text-sm">
          <p className="text-muted-foreground tabular-nums">{concept.fields.length} {concept.fields.length === 1 ? "fact" : "facts"} · {concept.relationships.length} {concept.relationships.length === 1 ? "link" : "links"}</p>
          {systems.length > 0 && <div className="flex flex-wrap gap-1">{systems.map((system) => <SourceChip key={system} system={system} />)}</div>}
        </CardContent>
      </Card>
    </Link>
  )
}

const GAP = { not_in_source: "not in this system", defaulted: "defaulted", deferred: "not mapped yet", derived_elsewhere: "derived elsewhere" } as const

function SourceCell({ source }: { readonly source: Source | undefined }) {
  switch (source?.kind) {
    case undefined: return <span className="text-muted-foreground">not sent</span>
    case "field": return <code className="text-xs">{source.field}</code>
    case "fixed": return <span>always <code className="text-xs">{source.value}</code></span>
    case "empty": return <span className="text-muted-foreground">left empty</span>
    case "computed": return <code className="text-xs">{source.expression}</code>
    case "nested": return <span title={`${source.collection} rows where ${source.where}`}><SourceCell source={source.value} /> <span className="text-muted-foreground">in each {source.dataset} row</span></span>
    case "transition": return <span><span className="text-muted-foreground">from transition </span><code className="text-xs">{source.expression}</code></span>
    case "gap": return <Badge variant="outline" title={source.note}>{GAP[source.reason]}</Badge>
  }
}

/** Each fact against each system's record that feeds it, as the admitted mappings declare. */
function SourceMatrix({ concept }: { readonly concept: Concept }) {
  const gaps = new Map<string, { readonly system: string; readonly reason: string; readonly note: string; readonly facts: string[] }>()
  for (const { system, facts } of concept.feeds) for (const [fact, [source]] of facts) {
    if (source?.kind !== "gap" || source.note === undefined) continue
    const key = `${system}\u0000${source.reason}\u0000${source.note}`
    const gap = gaps.get(key) ?? { system, reason: GAP[source.reason], note: source.note, facts: [] }
    gap.facts.push(fact.replaceAll("_", " "))
    gaps.set(key, gap)
  }
  return (
    <div className="space-y-3">
      <DenseTable head={
        <TableRow>
          <TableHead>Fact</TableHead>
          {concept.feeds.map(({ system, dataset }) => <TableHead key={`${system}/${dataset}`}><SourceChip system={system} /><div className="pt-0.5 font-normal text-muted-foreground">{dataset}</div></TableHead>)}
        </TableRow>
      }>
        {[...concept.fields.map(({ id }) => ({ id, label: id.replaceAll("_", " ") })),
          ...concept.relationships.map(({ id, target }) => ({ id, label: `${id.replaceAll("_", " ")} → ${conceptTitle(target)}` }))].map(({ id, label }) => (
          <TableRow key={id}>
            <TableCell className="font-medium">{label}</TableCell>
            {concept.feeds.map(({ system, dataset, facts }) => <TableCell key={`${system}/${dataset}`}>{(facts.get(id) ?? [undefined]).map((source, index) => <div key={index}><SourceCell source={source} /></div>)}</TableCell>)}
          </TableRow>
        ))}
      </DenseTable>
      {[...gaps].map(([key, { system, reason, note, facts }]) => <p key={key} className="text-sm text-muted-foreground"><b className="font-medium text-foreground">{system}, {facts.join(" and ")}: {reason}.</b> {note}</p>)}
      {concept.feeds.some(({ ignored }) => ignored.length > 0) && (
        <div className="space-y-1">
          <h3 className="text-meta font-medium text-muted-foreground">Left out of the mapping, on purpose</h3>
          {concept.feeds.flatMap(({ system, dataset, ignored }) => ignored.map(({ field, reason }) => (
            <p key={`${system}/${dataset}/${field}`} className="text-sm text-muted-foreground"><b className="font-medium text-foreground">{system} {dataset}, <code className="font-mono text-xs">{field}</code>:</b> {reason}</p>
          )))}
        </div>
      )}
    </div>
  )
}

/** A type's facts in the reader's language; Technical adds each fact's id, isolated. */
export function FactsTable({ concept, technical }: { readonly concept: Concept; readonly technical: boolean }) {
  const { locale } = useLocale()
  const comma = locale === "ar" ? "، " : ", "
  const markings = useMarkingCatalog()
  return (
    <DenseTable head={<TableRow><TableHead>Fact</TableHead><TableHead>Kind of value</TableHead></TableRow>}>
      {concept.fields.map((field) => (
        <TableRow key={field.id}>
          <TableCell className="font-medium">
            <span lang={field.labelLang ?? "en"}>{field.label ?? field.id.replaceAll("_", " ")}</span>
            {field.optional && <span className="text-muted-foreground"> (optional)</span>}
            {technical && <> <Iso className="font-mono text-xs text-muted-foreground">{field.id}</Iso></>}
            <MarkingChips markings={field.markings} catalog={markings} className="ms-2 inline-flex flex-wrap items-center gap-1 align-middle" />
          </TableCell>
          <TableCell className="whitespace-normal text-muted-foreground">
            {field.scalar.kind === "enum" && field.valueLabels !== undefined && field.valueLabels.size > 0
              // Every declared value, in order: its label, or the stored value itself (marked English) when it has none.
              ? <span dir={directionOf(locale)} data-values={field.scalar.values.length}>{field.scalar.values.map((value, index) => {
                const label = field.valueLabels?.get(value)
                return (
                  // An explicit direction: a value shown only as its id has no strong character of the page's
                  // script, and would otherwise order its own separator left to right.
                  <span key={value} dir={directionOf(locale)}>
                    {index > 0 && comma}
                    {label !== undefined
                      ? <>{label}{technical && <> <Iso className="font-mono text-xs">{value}</Iso></>}</>
                      : technical ? <Iso lang="en" className="font-mono text-xs">{value}</Iso> : <bdi lang="en">{value.replaceAll("_", " ")}</bdi>}
                  </span>
                )
              })}</span>
              : field.scalar.kind === "enum" && field.scalar.values.length > 12
                ? <details><summary className="cursor-pointer">One of {field.scalar.values.length} values</summary><div className="pt-1">{field.type}</div></details>
                : <span>{field.type}</span>}
          </TableCell>
        </TableRow>
      ))}
    </DenseTable>
  )
}

/** One object type as a record page: what it is, its fields, links, rules and where its records come from. */
function ObjectTypeDetail({ catalog, concept, recorded }: { readonly catalog: Catalog; readonly concept: Concept; readonly recorded: boolean }) {
  const link = (target: string) => catalog.byName.has(target)
    ? <Link to="/sources/$concept" params={{ concept: target }} className="font-medium underline-offset-4 hover:underline">{conceptTitle(target)}</Link>
    : <span>{conceptTitle(target)}</span>
  const systems = [...new Set(concept.feeds.map(({ system }) => system))]
  const { view } = useView()
  const text = about(concept, view)
  return (
    <article className="space-y-6">
      <header className="space-y-2">
        <h1 lang={concept.titleLang} className="text-title font-semibold tracking-tight">{concept.title}</h1>
        <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
          <Badge variant="secondary">{concept.definition === "interface" ? "Shared interface" : systems.length > 0 ? `Fed by ${systems.join(", ")}` : "No connected system feeds it"}</Badge>
          {recorded && <Badge variant="outline" title={RECORDED_HINT}>Recorded</Badge>}
          {view === "technical" && <Iso className="font-mono">{concept.name}</Iso>}
        </div>
        {text !== undefined && <p lang={text.lang} className="max-w-2xl text-muted-foreground">{text.text}</p>}
        {concept.parents.length > 0 && <p className="text-sm">A kind of {concept.parents.map((parent, index) => <span key={parent}>{index > 0 && ", "}{link(parent)}</span>)}</p>}
      </header>
      <Panel icon={Shapes} label="Facts recorded">
        <div className="p-1"><FactsTable concept={concept} technical={view === "technical"} /></div>
      </Panel>
      {concept.relationships.length > 0 && (
        <Panel icon={Shapes} label="Links">
          <Well className="divide-y">
            {concept.relationships.map((relationship) => (
              <div key={relationship.id} className="px-4 py-2.5 text-sm">
                <span className="text-muted-foreground">{relationship.id.replaceAll("_", " ")} → </span>{relationship.count} {link(relationship.target)}
                {relationship.description && <p className="text-meta text-muted-foreground">{relationship.description}</p>}
              </div>
            ))}
          </Well>
        </Panel>
      )}
      <LawsPanel concept={concept} />
      <Panel icon={Database} label="Where its records come from">
        <div className="p-1">{concept.feeds.length > 0 ? <SourceMatrix concept={concept} /> : <p className="px-3 py-2 text-sm text-muted-foreground">No connected system maps records into it directly.</p>}</div>
      </Panel>
    </article>
  )
}

/** One object type's page: its definition, then its records; the submenu lists the others. */
export function ObjectTypePage() {
  const { concept: type = "" } = useParams({ strict: false })
  const { read, catalog } = useCatalog()
  const concept = catalog?.byName.get(type)
  return (
    <div className="space-y-6">
      {read.isPending && <p className="text-muted-foreground">Reading definitions…</p>}
      {read.isError && <p className="text-destructive">{read.error.message}</p>}
      {catalog && (concept ? <ObjectTypeDetail catalog={catalog} concept={concept} recorded={read.data?.origin === "recorded"} /> : <EmptyState icon={Shapes}>No object type named {type} is installed.</EmptyState>)}
      {concept?.definition === "object type" && <RecordsGrid key={concept.name} concept={concept} recorded={read.data?.origin === "recorded"} />}
    </div>
  )
}

/** The overview's groups: object types by whether a system feeds them, then shared interfaces. */
const OVERVIEW_GROUPS: readonly { readonly label: string; readonly has: (concept: Concept) => boolean }[] = [
  { label: "Object types the systems feed", has: (concept) => concept.definition === "object type" && concept.kind === "concept" },
  { label: "Other object types", has: (concept) => concept.definition === "object type" && concept.kind !== "concept" },
  { label: "Shared interfaces", has: (concept) => concept.definition === "interface" },
]

/** The object types and shared interfaces, for the submenu island. */
export function useSourcesMenu(): readonly MenuEntry[] | undefined {
  const read = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const { locale } = useLocale()
  const catalog = useMemo(() => read.data && catalogOf(read.data.document, locale), [read.data, locale])
  // The cell counts each type for this caller; a type it may not list has no count, and the recording has none.
  const counts = useQuery({ queryKey: ["catalog", "counts"], queryFn: transport.objectCounts }).data
  return catalog && (["object type", "interface"] as const).flatMap((definition) => catalog.concepts.filter((concept) => concept.definition === definition)
    .map((concept): MenuEntry => {
      const { icon, tone } = objectAppearance(concept.name, definition === "interface")
      return { href: `/sources/${concept.name}`, label: concept.title, group: definition === "object type" ? "Object types" : "Shared interfaces", icon, iconTone: tone, kind: "objectType", count: menuCount(counts?.[concept.name]) }
    }))
}

/** A menu entry's count: exact, or the server's lower bound as text ("500+"); none when the cell sent none. */
export const menuCount = (total: Total | undefined): number | string | undefined =>
  total === undefined ? undefined : total.approximate ? `${total.count.toLocaleString("en-US")}+` : total.count

/** The admitted catalog, indexed once per read. */
export function useCatalog() {
  const read = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const { locale } = useLocale()
  const catalog = useMemo(() => read.data && catalogOf(read.data.document, locale), [read.data, locale])
  return { read, catalog }
}

export function SourcesPage() {
  const { read, catalog } = useCatalog()
  const recorded = read.data?.origin === "recorded"
  const [search, setSearch] = useState("")
  const { view } = useView()
  const matches = (concept: Concept) => `${concept.title} ${about(concept, view)?.text ?? ""}`.toLowerCase().includes(search.trim().toLowerCase())
  return (
    <section className="space-y-5">
      <PageHeader title="Sources" meta={recorded && <Badge variant="outline" title={RECORDED_HINT}>Recorded demo</Badge>}
        description={recorded
          ? (financeDemo ? "Synthetic core lending and bank settlement records map into the same model for clients, loans, and repayments." : "This snapshot uses synthetic ERPNext and Shopify records. Both systems map into the same business model for holdings, orders, and deliveries.")
          : "Each source system keeps its own records. Norn maps them into the same business model for holdings, orders, and deliveries."} />
      {read.isPending && <p className="text-muted-foreground">Reading definitions…</p>}
      {read.isError && <p className="text-destructive">{read.error.message}</p>}
      {catalog && <>
        <Systems catalog={catalog} recorded={recorded} />
        {read.data?.origin === "live" && <SourceConnections />}
        <Panel icon={Shapes} label="The business model">
          <Tabs defaultValue="cards" className="p-1">
          <TabsList className="mx-3 mt-2"><TabsTrigger value="cards">Types</TabsTrigger><TabsTrigger value="graph">Graph</TabsTrigger></TabsList>
          <TabsContent value="graph" className="p-3"><ModelGraph catalog={catalog} /></TabsContent>
          <TabsContent value="cards">
          <Well className="space-y-6 p-4">
            <label className="relative block w-full max-w-sm">
              <span className="sr-only">Find an object type</span>
              <Search aria-hidden className="pointer-events-none absolute start-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input type="search" placeholder="Find an object type" value={search} onChange={(event) => setSearch(event.target.value)}
                className="border-border bg-background ps-9 dark:bg-background" />
            </label>
            {OVERVIEW_GROUPS.filter(({ has }) => search.trim() !== "" || catalog.concepts.some(has)).map(({ label, has }) => (
              <div key={label} className="space-y-3">
                <h3 className="text-meta font-medium text-muted-foreground">{label}</h3>
                {catalog.concepts.some((concept) => has(concept) && matches(concept))
                  ? <div className="grid auto-rows-fr gap-3 sm:grid-cols-2 lg:grid-cols-3">
                      {catalog.concepts.filter((concept) => has(concept) && matches(concept)).map((concept) => (
                        <ConceptCard key={concept.name} concept={concept} />
                      ))}
                    </div>
                  : <EmptyState icon={Search}>Nothing here matches “{search.trim()}”.</EmptyState>}
              </div>
            ))}
          </Well>
          </TabsContent>
          </Tabs>
        </Panel>
        {recorded && <Panel icon={Database} label="Works with">
          <div className="space-y-2 px-4 pb-3">
            <p className="text-meta text-muted-foreground">Example business feeds and data tools</p>
            <div className="flex flex-wrap gap-1.5">{EXAMPLE_STACK.map((system) => <SourceChip key={system} system={system} title="Example system" />)}</div>
          </div>
        </Panel>}
      </>}
    </section>
  )
}
