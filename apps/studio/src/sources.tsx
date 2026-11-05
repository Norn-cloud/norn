import { useQuery } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { ArrowRight, Database, Search, Shapes } from "lucide-react"
import { useMemo, useState, type ReactNode } from "react"
import { KpiTile } from "@/components/kpi-tile"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { catalogOf, conceptTitle, type Catalog, type Concept, type Source } from "./lib/catalog.ts"
import { recordedTransport } from "./lib/transport.ts"
import type { MenuEntry } from "./shell.tsx"

/** The "dense, crisp" tier for data tables: theme radius, 13px tabular figures, hairlines, grey header, no zebra. */
export function DenseTable({ head, children }: { readonly head: ReactNode; readonly children: ReactNode }) {
  return (
    <div className="overflow-hidden rounded-lg border">
      <Table className="text-[13px] tabular-nums">
        <TableHeader className="bg-muted">{head}</TableHeader>
        <TableBody>{children}</TableBody>
      </Table>
    </div>
  )
}

export const RECORDED_HINT = "Documents the engine produced, replayed exactly. Not a live connection."

function Systems({ catalog }: { readonly catalog: Catalog }) {
  const concepts = catalog.concepts.filter(({ kind }) => kind === "concept")
  const relationships = concepts.reduce((total, { relationships }) => total + relationships.length, 0)
  return (
    <div className="grid items-stretch gap-3 md:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)]">
      <Panel icon={Database} label="Connected systems">
        <Well className="divide-y">
          {catalog.systems.map((system) => (
            <div key={system.id} className="flex items-baseline justify-between gap-3 px-4 py-3">
              <span className="font-medium">{system.name}</span>
              <span className="text-meta text-muted-foreground tabular-nums">{system.datasets.length} kinds of record · feeds {system.feeds.length} object types</span>
            </div>
          ))}
        </Well>
      </Panel>
      <ArrowRight aria-hidden className="mx-auto size-5 self-center text-muted-foreground max-md:rotate-90" />
      <KpiTile icon={Shapes} label="One business model" value={concepts.length}
        sub={`object types, ${relationships} links between them, built on ${catalog.concepts.length - concepts.length} shared interfaces`} />
    </div>
  )
}

function ConceptCard({ concept, onOpen }: { readonly concept: Concept; readonly onOpen: () => void }) {
  const systems = [...new Set(concept.feeds.map(({ system }) => system))]
  return (
    <button type="button" id={`concept-${concept.name}`} className="h-full scroll-mt-4 rounded-lg text-left outline-none focus-visible:ring-3 focus-visible:ring-ring/30" onClick={onOpen}>
      <Card size="sm" className="h-full ring-foreground/10 transition-colors hover:ring-foreground/25">
        <CardHeader>
          <CardTitle>{concept.title}</CardTitle>
          <CardDescription className="line-clamp-2 min-h-[2lh]">{concept.description ?? "No description declared."}</CardDescription>
        </CardHeader>
        <CardContent className="mt-auto space-y-3 text-sm">
          <p className="text-muted-foreground tabular-nums">{concept.fields.length} facts · {concept.relationships.length} links</p>
          {systems.length > 0 && <div className="flex flex-wrap gap-1">{systems.map((system) => <Badge key={system} variant="secondary">{system}</Badge>)}</div>}
        </CardContent>
      </Card>
    </button>
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
          {concept.feeds.map(({ system, dataset }) => <TableHead key={`${system}/${dataset}`}><div>{system}</div><div className="font-normal text-muted-foreground">{dataset}</div></TableHead>)}
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
    </div>
  )
}

function ConceptSheet({ catalog, name, onOpen }: { readonly catalog: Catalog; readonly name: string | undefined; readonly onOpen: (name: string | undefined) => void }) {
  const concept = name === undefined ? undefined : catalog.byName.get(name)
  const link = (target: string) => catalog.byName.has(target)
    ? <button type="button" className="font-medium underline-offset-4 hover:underline" onClick={() => onOpen(target)}>{conceptTitle(target)}</button>
    : <span>{conceptTitle(target)}</span>
  return (
    <Sheet open={concept !== undefined} onOpenChange={(open) => { if (!open) onOpen(undefined) }}>
      <SheetContent className="overflow-y-auto data-[side=right]:w-full data-[side=right]:sm:max-w-2xl">
        {concept && <>
          <SheetHeader>
            <SheetTitle className="text-xl">{concept.title}</SheetTitle>
            <SheetDescription>{concept.description ?? concept.name}</SheetDescription>
            {concept.kind === "concept" && <Link to="/sources/$concept" params={{ concept: concept.name }} className="text-sm font-medium underline-offset-4 hover:underline">See the {concept.title.toLowerCase()} records →</Link>}
            {concept.parents.length > 0 && <p className="text-sm">A kind of {concept.parents.map((parent, index) => <span key={parent}>{index > 0 && ", "}{link(parent)}</span>)}</p>}
          </SheetHeader>
          <div className="space-y-6 px-4 pb-6">
            <section>
              <h3 className="pb-2 font-medium">Facts recorded</h3>
              <DenseTable head={<TableRow><TableHead>Fact</TableHead><TableHead>Kind of value</TableHead></TableRow>}>
                {concept.fields.map((field) => (
                  <TableRow key={field.id}>
                    <TableCell className="font-medium">{field.id.replaceAll("_", " ")}{field.optional && <span className="text-muted-foreground"> (optional)</span>}</TableCell>
                    <TableCell className="whitespace-normal text-muted-foreground">{field.type}</TableCell>
                  </TableRow>
                ))}
              </DenseTable>
            </section>
            {concept.relationships.length > 0 && (
              <section>
                <h3 className="pb-2 font-medium">Links</h3>
                <ul className="space-y-2 text-sm">
                  {concept.relationships.map((relationship) => (
                    <li key={relationship.id}>
                      <span className="text-muted-foreground">{relationship.id.replaceAll("_", " ")} → </span>{relationship.count} {link(relationship.target)}
                      {relationship.description && <p className="text-muted-foreground">{relationship.description}</p>}
                    </li>
                  ))}
                </ul>
              </section>
            )}
            <section>
              <h3 className="pb-2 font-medium">Rules it must keep</h3>
              {concept.rules.length > 0
                ? <ul className="list-disc space-y-1 pl-5 text-sm">{concept.rules.map((rule) => <li key={rule}>{rule}</li>)}</ul>
                : <p className="text-sm text-muted-foreground">No rule is declared on this object type itself.</p>}
            </section>
            <section>
              <h3 className="pb-2 font-medium">Where its records come from</h3>
              {concept.feeds.length > 0 ? <SourceMatrix concept={concept} /> : <p className="text-sm text-muted-foreground">No connected system maps records into it directly.</p>}
            </section>
          </div>
        </>}
      </SheetContent>
    </Sheet>
  )
}

/** The object types the systems feed, for the submenu island. */
export function useSourcesMenu(): readonly MenuEntry[] | undefined {
  const read = useQuery({ queryKey: ["catalog"], queryFn: recordedTransport.catalog })
  const catalog = useMemo(() => read.data && catalogOf(read.data.document), [read.data])
  return catalog?.concepts.filter(({ kind }) => kind === "concept").map((concept) => ({ href: `#concept-${concept.name}`, label: concept.title, group: "Object types" }))
}

/** The admitted catalog, indexed once per read. */
export function useCatalog() {
  const read = useQuery({ queryKey: ["catalog"], queryFn: recordedTransport.catalog })
  const catalog = useMemo(() => read.data && catalogOf(read.data.document), [read.data])
  return { read, catalog }
}

export function SourcesPage() {
  const { read, catalog } = useCatalog()
  const [open, setOpen] = useState<string>()
  const [search, setSearch] = useState("")
  const matches = (concept: Concept) => `${concept.title} ${concept.description ?? ""}`.toLowerCase().includes(search.trim().toLowerCase())
  return (
    <section className="space-y-8">
      <div className="space-y-2">
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-title font-medium">Sources</h1>
          {read.data?.origin === "recorded" && <Badge variant="outline" title={RECORDED_HINT}>Recorded</Badge>}
        </div>
        <p className="max-w-2xl text-muted-foreground">Each connected system keeps its own records. Each one is mapped into the same business model, so a holding, an order or a delivery means one thing, wherever it was entered.</p>
      </div>
      {read.isPending && <p className="text-muted-foreground">Reading definitions…</p>}
      {read.isError && <p className="text-destructive">{read.error.message}</p>}
      {catalog && <>
        <Systems catalog={catalog} />
        <Panel icon={Shapes} label="The business model">
          <Well className="space-y-6 p-4">
            <label className="relative block w-full max-w-sm">
              <span className="sr-only">Find an object type</span>
              <Search aria-hidden className="pointer-events-none absolute start-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input type="search" placeholder="Find an object type" value={search} onChange={(event) => setSearch(event.target.value)}
                className="border-border bg-background ps-9 dark:bg-background" />
            </label>
            {(["concept", "core idea"] as const).map((kind) => (
              <div key={kind} className="space-y-3">
                <h3 className="text-meta font-medium text-muted-foreground">{kind === "concept" ? "Object types the systems feed" : "Shared interfaces"}</h3>
                {catalog.concepts.some((concept) => concept.kind === kind && matches(concept))
                  ? <div className="grid auto-rows-fr gap-3 sm:grid-cols-2 lg:grid-cols-3">
                      {catalog.concepts.filter((concept) => concept.kind === kind && matches(concept)).map((concept) => (
                        <ConceptCard key={concept.name} concept={concept} onOpen={() => setOpen(concept.name)} />
                      ))}
                    </div>
                  : <EmptyState icon={Search}>Nothing here matches “{search.trim()}”.</EmptyState>}
              </div>
            ))}
          </Well>
        </Panel>
        <ConceptSheet catalog={catalog} name={open} onOpen={setOpen} />
      </>}
    </section>
  )
}
