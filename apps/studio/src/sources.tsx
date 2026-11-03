import { useQuery } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { ArrowRight, Database } from "lucide-react"
import { useMemo, useState, type ReactNode } from "react"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { catalogOf, conceptTitle, type Catalog, type Concept } from "./lib/catalog.ts"
import { recordedTransport } from "./lib/transport.ts"

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
    <div className="grid items-center gap-4 md:grid-cols-[1fr_auto_1fr]">
      <div className="grid gap-3">
        {catalog.systems.map((system) => (
          <Card key={system.id} className="py-4">
            <CardHeader className="px-4">
              <CardTitle className="flex items-center gap-2"><Database className="size-4" />{system.name}</CardTitle>
              <CardDescription>{system.datasets.length} kinds of record · feeds {system.feeds.length} concepts</CardDescription>
            </CardHeader>
          </Card>
        ))}
      </div>
      <ArrowRight className="mx-auto size-8 rotate-90 text-muted-foreground md:rotate-0" />
      <Card>
        <CardHeader>
          <CardDescription>One business model</CardDescription>
          <CardTitle className="text-5xl tabular-nums">{concepts.length}</CardTitle>
          <CardDescription>concepts, {relationships} relationships between them, built on {catalog.concepts.length - concepts.length} core ideas</CardDescription>
        </CardHeader>
      </Card>
    </div>
  )
}

function ConceptCard({ concept, onOpen }: { readonly concept: Concept; readonly onOpen: () => void }) {
  const systems = [...new Set(concept.feeds.map(({ system }) => system))]
  return (
    <button type="button" className="text-left" onClick={onOpen}>
      <Card className="h-full transition-colors hover:border-primary">
        <CardHeader>
          <CardTitle>{concept.title}</CardTitle>
          {concept.description && <CardDescription className="line-clamp-2">{concept.description}</CardDescription>}
        </CardHeader>
        <CardContent className="space-y-3 text-sm">
          <p className="text-muted-foreground tabular-nums">{concept.fields.length} facts · {concept.relationships.length} relationships</p>
          {systems.length > 0 && <div className="flex flex-wrap gap-1">{systems.map((system) => <Badge key={system} variant="secondary">{system}</Badge>)}</div>}
        </CardContent>
      </Card>
    </button>
  )
}

function ConceptSheet({ catalog, name, onOpen }: { readonly catalog: Catalog; readonly name: string | undefined; readonly onOpen: (name: string | undefined) => void }) {
  const concept = name === undefined ? undefined : catalog.byName.get(name)
  const link = (target: string) => catalog.byName.has(target)
    ? <button type="button" className="font-medium underline-offset-4 hover:underline" onClick={() => onOpen(target)}>{conceptTitle(target)}</button>
    : <span>{conceptTitle(target)}</span>
  return (
    <Sheet open={concept !== undefined} onOpenChange={(open) => { if (!open) onOpen(undefined) }}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-xl">
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
                <h3 className="pb-2 font-medium">Relationships</h3>
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
                : <p className="text-sm text-muted-foreground">No rule is declared on this concept itself.</p>}
            </section>
            <section>
              <h3 className="pb-2 font-medium">Where its records come from</h3>
              {concept.feeds.length > 0
                ? <ul className="space-y-1 text-sm">{concept.feeds.map(({ system, dataset }) => <li key={`${system}/${dataset}`}><Badge variant="secondary">{system}</Badge> <span className="text-muted-foreground">{dataset}</span></li>)}</ul>
                : <p className="text-sm text-muted-foreground">No connected system maps records into it directly.</p>}
            </section>
          </div>
        </>}
      </SheetContent>
    </Sheet>
  )
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
          <h1 className="text-3xl font-semibold tracking-tight">Sources</h1>
          {read.data?.origin === "recorded" && <Badge variant="outline" title={RECORDED_HINT}>Recorded</Badge>}
        </div>
        <p className="max-w-2xl text-muted-foreground">Three systems keep their own records. Each one is mapped into the same business model, so a holding, an order or a delivery means one thing, wherever it was entered.</p>
      </div>
      {read.isPending && <p className="text-muted-foreground">Reading definitions…</p>}
      {read.isError && <p className="text-destructive">{read.error.message}</p>}
      {catalog && <>
        <Systems catalog={catalog} />
        <div className="flex flex-wrap items-end justify-between gap-4">
          <h2 className="text-xl font-semibold">The business model</h2>
          <label className="grid w-full max-w-xs gap-1 text-sm text-muted-foreground">
            Find a concept
            <Input type="search" value={search} onChange={(event) => setSearch(event.target.value)} />
          </label>
        </div>
        {(["concept", "core idea"] as const).map((kind) => (
          <div key={kind} className="space-y-3">
            <h3 className="text-sm font-medium text-muted-foreground">{kind === "concept" ? "Concepts the systems feed" : "Core ideas they build on"}</h3>
            <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
              {catalog.concepts.filter((concept) => concept.kind === kind && matches(concept)).map((concept) => (
                <ConceptCard key={concept.name} concept={concept} onOpen={() => setOpen(concept.name)} />
              ))}
            </div>
          </div>
        ))}
        <ConceptSheet catalog={catalog} name={open} onOpen={setOpen} />
      </>}
    </section>
  )
}
