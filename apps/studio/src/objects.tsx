import { useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { Database, GitBranch, Link2, Rows3, Shapes } from "lucide-react"
import { useMemo } from "react"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { cn } from "@/lib/utils"
import { catalogOf, conceptTitle, type Lifecycle } from "./lib/catalog.ts"
import { member, type Read } from "./lib/exact.ts"
import { day } from "./lib/format.ts"
import { lifecycleOf, recordTitle } from "./lib/objects.ts"
import { recordedTransport, type ObjectRecord, type ObjectRead } from "./lib/transport.ts"
import { ObjectTypePage } from "./sources.tsx"

const RECORDED_HINT = "An object the cell stored, replayed exactly. Not a live connection."

/** One stored value as the server wrote it: decimals keep their exact text. */
function valueText(read: Read<ObjectRecord>, property: string): string {
  const value = read.document.values[property]
  const exact = read.numbers.get(`/values/${member(property)}`)
  if (exact !== undefined) return exact
  if (value === null || value === undefined) return "empty"
  return typeof value === "string" ? value : JSON.stringify(value)
}

const recordLink = (objectType: string, id: string) => ({ to: "/sources/$type/$", params: { type: objectType, _splat: id } }) as const

/** An object type's recorded objects, for its record page. */
export function RecordsPanel({ objectType }: { readonly objectType: string }) {
  const read = useQuery({ queryKey: ["objects", objectType], queryFn: async () => (await recordedTransport.objects(objectType)) ?? null })
  const page = read.data?.document
  const held = page?.quarantineCount ?? 0
  return (
    <Panel icon={Rows3} label="Records" action={held > 0 ? `${held} held back by open findings` : undefined}>
      {read.isError && <p className="px-3 py-2 text-sm text-destructive">{read.error.message}</p>}
      {read.isSuccess && (page === undefined || page.records.length === 0
        ? <Well><EmptyState icon={Rows3}>No records of this object type were recorded.</EmptyState></Well>
        : (
          <TableCard><Table>
            <TableHeader><TableRow><TableHead>Record</TableHead><TableHead className="max-sm:hidden">Identity</TableHead><TableHead className="text-right">Version</TableHead></TableRow></TableHeader>
            <TableBody>
              {page.records.map((record) => (
                <TableRow key={record.id}>
                  <TableCell><Link {...recordLink(record.objectType, record.id)} className="font-medium underline-offset-4 hover:underline">{recordTitle(record)}</Link></TableCell>
                  <TableCell className="font-mono text-meta text-muted-foreground max-sm:hidden">{record.id}</TableCell>
                  <TableCell className="text-right tabular-nums">{record.version}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table></TableCard>
        ))}
    </Panel>
  )
}

/** Where the record sits in a declared lifecycle, and the steps the definition declares from there. */
function LifecycleStepper({ lifecycle, record }: { readonly lifecycle: Lifecycle; readonly record: ObjectRecord }) {
  const { states, state, terminal, next } = lifecycleOf(lifecycle, record)
  const at = state === undefined ? -1 : states.indexOf(state)
  return (
    <div className="space-y-3">
      <ol aria-label={`${conceptTitle(record.objectType)} ${lifecycle.property.replaceAll("_", " ")}`} className="flex flex-wrap items-center gap-2">
        {states.map((step, index) => (
          <li key={step} aria-current={step === state ? "step" : undefined} className="flex items-center gap-2">
            {index > 0 && <span aria-hidden className="h-px w-6 bg-border" />}
            <span className={cn("rounded-full border px-3 py-1 text-sm", step === state ? "border-primary bg-primary text-primary-foreground" : index < at ? "bg-accent" : "text-muted-foreground")}>
              {step.replaceAll("_", " ")}
            </span>
          </li>
        ))}
      </ol>
      <p className="text-sm text-muted-foreground">
        {state === undefined
          ? `The record has no ${lifecycle.property.replaceAll("_", " ")} yet.`
          : terminal || next.length === 0
            ? "The definition declares no step from here."
            : <>Next, the definition allows {next.map(({ id, to }, index) => <span key={id}>{index > 0 && ", "}<b className="font-medium text-foreground">{id.replaceAll("_", " ")}</b> (to {to.replaceAll("_", " ")})</span>)}. The server checks each step's rules when it is asked.</>}
      </p>
    </div>
  )
}

/** One recorded object: its facts and where each came from, its links and its lifecycle. */
function ObjectRecordView({ read, lifecycles }: { readonly read: ObjectRead; readonly lifecycles: readonly Lifecycle[] }) {
  const record = read.record.document
  const provenance = read.source?.document
  const links = read.links?.document
  return (
    <article className="space-y-6">
      <header className="space-y-2">
        <h1 className="text-title font-medium">{recordTitle(record)}</h1>
        <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
          <Badge variant="secondary" asChild><Link to="/sources/$type" params={{ type: record.objectType }}>{conceptTitle(record.objectType)}</Link></Badge>
          {read.record.origin === "recorded" && <Badge variant="outline" title={RECORDED_HINT}>Recorded</Badge>}
          <span className="font-mono">{record.id}</span>
          <span>· version {record.version}</span>
        </div>
      </header>
      {lifecycles.map((lifecycle) => (
        <Panel key={lifecycle.property} icon={GitBranch} label={`Lifecycle · ${lifecycle.property.replaceAll("_", " ")}`}>
          <Well className="px-4 py-3"><LifecycleStepper lifecycle={lifecycle} record={record} /></Well>
        </Panel>
      ))}
      <Panel icon={Shapes} label="Facts" action={provenance ? `Mapped by ${provenance.mappingPin.artifactId} ${provenance.mappingPin.version}` : undefined}>
        <TableCard><Table>
          <TableHeader><TableRow><TableHead>Fact</TableHead><TableHead>Value</TableHead><TableHead className="max-md:hidden">From</TableHead><TableHead className="max-lg:hidden">Recorded</TableHead></TableRow></TableHeader>
          <TableBody>
            {Object.keys(record.values).map((property) => {
              const from = provenance?.fieldProvenance[property]
              return (
                <TableRow key={property}>
                  <TableCell className="font-medium">{property.replaceAll("_", " ")}</TableCell>
                  <TableCell className="whitespace-normal tabular-nums">{valueText(read.record, property)}</TableCell>
                  <TableCell className="whitespace-normal text-muted-foreground max-md:hidden">
                    {from ? <>{from.source.system} · {(from.sourceFields ?? []).join(", ") || from.transform}</> : "No lineage recorded"}
                  </TableCell>
                  <TableCell className="text-muted-foreground max-lg:hidden">{from ? day(from.recordedAt) : ""}</TableCell>
                </TableRow>
              )
            })}
          </TableBody>
        </Table></TableCard>
      </Panel>
      <Panel icon={Link2} label="Links">
        <Well>
          {links === undefined && <EmptyState icon={Link2}>The links touching this record were not recorded.</EmptyState>}
          {links !== undefined && links.items.length === 0 && <EmptyState icon={Link2}>No live link touches this record.</EmptyState>}
          {links !== undefined && links.items.length > 0 && (
            <ul className="divide-y">
              {links.items.map((edge) => {
                const outgoing = edge.source === record.id
                const [type, id] = outgoing ? [edge.targetType, edge.target] : [edge.sourceType, edge.source]
                return (
                  <li key={`${edge.link}:${edge.source}:${edge.target}`} className="px-4 py-2.5 text-sm">
                    <span className="text-muted-foreground">{edge.link.replaceAll("_", " ")} {outgoing ? "→" : "←"} </span>
                    <Link {...recordLink(type, id)} className="font-medium underline-offset-4 hover:underline">{conceptTitle(type)} {id}</Link>
                  </li>
                )
              })}
            </ul>
          )}
          {links?.hasMore && <p className="px-4 pb-3 text-meta text-muted-foreground">More links exist than this page holds.</p>}
        </Well>
      </Panel>
      <Panel icon={Database} label="Where it came from">
        <Well className="px-4 py-3 text-sm">
          {provenance === undefined ? <p className="text-muted-foreground">Its source lineage was not recorded.</p> : (
            <dl className="grid gap-x-6 gap-y-2 sm:grid-cols-[auto_minmax(0,1fr)]">
              <dt className="text-muted-foreground">Source record</dt><dd>{provenance.sourceRecordRef.system} · <span className="font-mono">{provenance.sourceRecordRef.record}</span></dd>
              <dt className="text-muted-foreground">Source document</dt><dd>{provenance.documentRef.system} · <span className="font-mono">{provenance.documentRef.record}</span></dd>
              <dt className="text-muted-foreground">Mapping</dt><dd>{provenance.mappingPin.artifactId} {provenance.mappingPin.version} · <span className="font-mono" title={provenance.mappingPin.sha256}>{provenance.mappingPin.sha256.slice(0, 12)}</span></dd>
            </dl>
          )}
        </Well>
      </Panel>
    </article>
  )
}

/** A recorded object's page under its object type; with no identity, the object type's own page. */
export function ObjectRecordPage() {
  const { type, _splat: id = "" } = useParams({ from: "/sources/$type/$" })
  return id === "" ? <ObjectTypePage /> : <RecordRead type={type} id={id} />
}

function RecordRead({ type, id }: { readonly type: string; readonly id: string }) {
  const read = useQuery({ queryKey: ["object", type, id], queryFn: async () => (await recordedTransport.object(type, id)) ?? null })
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: recordedTransport.catalog })
  const lifecycles = useMemo(() => (catalog.data && catalogOf(catalog.data.document).byName.get(type)?.lifecycles) ?? [], [catalog.data, type])
  return (
    <>
      {read.isPending && <p className="text-muted-foreground">Reading the record…</p>}
      {read.isError && <p className="text-destructive">{read.error.message}</p>}
      {read.isSuccess && (read.data ? <ObjectRecordView read={read.data} lifecycles={lifecycles} /> : <EmptyState icon={Rows3}>No {conceptTitle(type)} record {id} was recorded.</EmptyState>)}
    </>
  )
}
