import { useQuery } from "@tanstack/react-query"
import { NO_DATA_CELL } from "@/lib/definitions"
import { Link } from "@tanstack/react-router"
import { useState } from "react"
import { ChartLine, Clock, Ellipsis, Rows3, Split } from "lucide-react"
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from "recharts"
import { Badge } from "@/components/ui/badge"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableFooter, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { buckets, contribution, declarations, fraction, shown, type Bucket } from "./lib/answer.ts"
import { chartEmpty, chartFrame } from "./lib/chart.ts"
import { FRESHNESS, headline, period, scope, stamp } from "./lib/format.ts"
import { amountTexts, liveTrace, type LiveRead, type Refusal, type Trace } from "./lib/live.ts"
import type { Read } from "./lib/exact.ts"
import type { Question, QuestionAnswer } from "./lib/transport.ts"
import { readDocument } from "./lib/exact.ts"
import { cellRead } from "./lib/live-transport.ts"
import { evaluateQuestion, traceQuestion } from "./lib/evaluate.ts"
import { breakOuts, headlineOf, type CatalogProperty, type Drill } from "./lib/answer-anatomy.ts"
import { CopyId } from "@/components/copy-id"
import { AnswerProvenance } from "@/components/provenance"
import { systemName } from "./lib/catalog.ts"
import { bucketProvenance, metricLabels } from "./lib/provenance.ts"

/**
 * The answer view: how one answer reads, whether the question is published or
 * built and run in Studio. No transport here, so anything may render it.
 */
function Exact({ value, totals }: { readonly value: string; readonly totals: readonly string[] }) {
  return totals.length > 1
    ? <span className="tabular-nums"><span className="text-muted-foreground">{fraction(totals)} = </span><b>{value}</b></span>
    : <b className="tabular-nums">{value}</b>
}

/** One record's contribution as the server sent it, or "Withheld" when it sent none for this reader. */
function amounts(read: Read<Trace>, index: number) {
  const texts = amountTexts(read, index)
  return texts === null ? <span className="text-muted-foreground" title="The server withheld this record's contribution from your role.">Withheld</span> : fraction(texts)
}

/** A bucket's records from the live trace: who contributed, under which receipt, and what the server withheld. */
/** How a bucket's records are read: the published question's trace by name, or a built question's own (POST :trace). */
export type FetchTrace = (lineageRef: string, bucket: string) => Promise<LiveRead<Trace>>

/**
 * Which engine computed the answer, as the cell reports it: DuckDB when the cell could lower the
 * plan to it, the cell's maintained totals, or its own exact engine. The figures are exact either way.
 */
const ENGINES: Readonly<Record<string, string>> = { duckdb: "Computed by DuckDB", cell: "Computed by the cell's exact engine", ivm: "Read from the cell's maintained totals" }
export const engineOf = (answer: unknown): string | undefined => {
  const engine = (answer as { engine?: unknown } | null)?.engine
  return typeof engine === "string" ? ENGINES[engine] ?? `Computed by ${engine}` : undefined
}

/** Refusals that mean the cell committed since this answer: its trace handle no longer names this number. */
const MOVED = new Set(["core.projection.answer_moved", "core.projection.lineage_incomplete"])

export function LiveRecords({ question, lineageRef, bucket, fetchTrace, onStale }: { readonly question: string; readonly lineageRef: string; readonly bucket: Bucket; readonly fetchTrace?: FetchTrace; readonly onStale?: () => void }) {
  const trace = useQuery({ queryKey: ["live-trace", question, lineageRef, bucket.key], queryFn: () => fetchTrace ? fetchTrace(lineageRef, bucket.key) : liveTrace(question, lineageRef, bucket.key), retry: false })
  const data = trace.data
  if (trace.isPending) return <p className="text-sm text-muted-foreground">Reading the records…</p>
  if (trace.isError || data === undefined || data.kind === "unavailable") return <p className="text-sm text-muted-foreground">{data?.kind === "unavailable" && data.reason === "no-cell" ? NO_DATA_CELL : "The live records could not be read."}</p>
  if (data.kind === "refused" && MOVED.has(data.refusal.code)) return (
    <div role="status" className="space-y-2 rounded-md border p-3 text-sm">
      <p><span className="font-medium">The data changed since this answer.</span> The cell committed new records, so this number may no longer be the current one and its records can't be traced as they were.</p>
      {onStale ? <button type="button" className="font-medium underline underline-offset-4" onClick={onStale}>Ask again for the current number</button> : <p className="text-muted-foreground">Run the question again to trace the current number.</p>}
    </div>
  )
  if (data.kind === "refused") return <RefusedNote refusal={data.refusal} />
  const read = data.read
  return (
    <TableCard><Table>
      <TableHeader><TableRow><TableHead>Record</TableHead><TableHead className="text-right">Contributes</TableHead><TableHead>Source record</TableHead><TableHead>Receipt</TableHead></TableRow></TableHeader>
      <TableBody>
        {read.document.contributions.map((contribution, index) => (
          <TableRow key={`${contribution.fact.key}@${contribution.fact.version}`}>
            <TableCell className="font-mono">
              <Link to="/sources/$concept/$" params={{ concept: contribution.fact.objectType, _splat: contribution.fact.key }} hash="lineage" className="underline-offset-4 hover:underline" title="Open this record and where it came from">{contribution.fact.key}</Link>
              {contribution.restricted && <Badge variant="outline" className="ms-2 font-sans" title="The server withheld this record's values from your role.">Values withheld</Badge>}
            </TableCell>
            <TableCell className="text-right tabular-nums">{amounts(read, index)}</TableCell>
            <TableCell>{contribution.source.status === "resolved"
              ? contribution.source.provenance?.sourceRecordRef
                ? <><div className="font-medium">{systemName(contribution.source.provenance.sourceRecordRef.system)}</div><div className="font-mono text-xs text-muted-foreground">{contribution.source.provenance.sourceRecordRef.record}</div></>
                : <span className="text-muted-foreground">Resolved</span>
              : <span className="text-muted-foreground" title={contribution.source.status === "restricted" ? "Your role may not see where this record came from." : "The cell retains no source record for it."}>{contribution.source.status === "restricted" ? "Withheld" : "None retained"}</span>}</TableCell>
            <TableCell>{contribution.receiptHash === null ? <span className="text-muted-foreground" title="The server withheld this receipt hash with the bucket's values: it commits to them.">Receipt withheld</span> : <CopyId value={contribution.receiptHash} short={10} />}<div className="text-xs text-muted-foreground">commit {contribution.commitSeq}</div></TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table></TableCard>
  )
}

/** The server's refusal, as an answer: its reason in its own words, and its code. */
export function RefusedNote({ refusal }: { readonly refusal: Refusal }) {
  return (
    <div role="status" className="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm">
      <div><span className="font-medium">Refused:</span> {refusal.message}</div>
      <div className="font-mono text-xs text-muted-foreground">{refusal.code}{refusal.explanation ? ` · ${refusal.explanation}` : ""}</div>
    </div>
  )
}

function DrillDown({ bucket, onClose, live }: { readonly bucket: Bucket | undefined; readonly onClose: () => void; readonly live?: { readonly question: string; readonly lineageRef: string; readonly fetchTrace?: FetchTrace; readonly onStale?: () => void } }) {
  return (
    <Sheet open={bucket !== undefined} onOpenChange={(open) => { if (!open) onClose() }}>
      <SheetContent className="data-[side=right]:w-full data-[side=right]:sm:max-w-lg">
        {bucket && <>
          <SheetHeader>
            <SheetTitle>{bucket.label}</SheetTitle>
            <SheetDescription className="text-base"><Exact value={bucket.value} totals={bucket.totals} /></SheetDescription>
          </SheetHeader>
          <div className="px-4">
            <p className="pb-3 text-sm text-muted-foreground">The {bucket.members.length} records the server added up, exactly as it counted them.</p>
            {live ? <LiveRecords question={live.question} lineageRef={live.lineageRef} bucket={bucket} fetchTrace={live.fetchTrace} onStale={live.onStale} /> : <>
            <TableCard><Table>
              <TableHeader><TableRow><TableHead>Record</TableHead><TableHead className="text-right">Contributes</TableHead></TableRow></TableHeader>
              <TableBody>
                {bucket.members.map(({ id, amounts }) => (
                  <TableRow key={id}><TableCell className="font-mono">{id}</TableCell><TableCell className={amounts === null ? "text-right text-muted-foreground" : "text-right tabular-nums"}>{contribution(amounts)}</TableCell></TableRow>
                ))}
              </TableBody>
              <TableFooter><TableRow><TableCell>Total</TableCell><TableCell className="text-right"><Exact value={bucket.value} totals={bucket.totals} /></TableCell></TableRow></TableFooter>
            </Table></TableCard>
            <p className="pt-4 text-xs text-muted-foreground">The cell traces a number only when it answers it live; open a record for where it came from.</p>
            </>}
          </div>
        </>}
      </SheetContent>
    </Sheet>
  )
}

const changes = (count: number) => `${count} committed ${count === 1 ? "change" : "changes"}`

/**
 * One answer as the server wrote it: headline, freshness and origin, the
 * latest bucket, the chart, every bucket, and each bucket's records. Used for
 * published questions and for questions built and run in Studio alike.
 */
export function AnswerView({ read, title, heading = "h2", link = false, refusal, fetchTrace, onStale, openBucket }: {
  readonly read: QuestionAnswer
  /** What the answer measures, in words, when the page that asked knows it better than the question's metric names. */
  readonly title?: string
  readonly heading?: "h1" | "h2" | "h3"
  /** Link the headline to the published question's page. */
  readonly link?: boolean
  /** The server's refusal of the live read, shown over the recording. */
  readonly refusal?: Refusal
  /** Ask the question again, when a drill-down finds the cell has committed since this answer. */
  readonly onStale?: () => void
  /** How a bucket's records are read for a question that is not published. */
  readonly fetchTrace?: FetchTrace
  /** A bucket whose records open at once: a tile's number asked for its trace. */
  readonly openBucket?: string
}) {
  const question = read.question.document
  const answer = read.answer.document
  const declared = declarations(read)
  const rows = buckets(declared, { answer, numbers: read.answer.numbers, at: "" })
  const [open, setOpen] = useState<Bucket | undefined>(() => rows.find(({ key }) => key === openBucket))
  const named = title ?? headline(question)
  const provenanceOf = (bucket: Bucket) => bucketProvenance(answer, bucket, title === undefined ? metricLabels(declared) : [title], engineOf(answer) ?? null)
  const lead = headlineOf(named, question, rows)
  const freshness = FRESHNESS[answer.freshness]
  const frame = chartFrame(rows)
  const sources = Object.entries(answer.watermarks).flatMap(([source, partitions]) =>
    Object.values(partitions).map(({ complete }) => `${systemName(source)} ${complete ? "complete" : "incomplete"}`))
  const [drill, setDrill] = useState<Drill>()
  const live = read.answer.origin === "live"
  const grain = question.groupBy?.[0]?.grain
  return (
    <section className="space-y-4">
      <div className="space-y-1 px-1">
        <div className="flex flex-wrap items-center gap-2">
          {heading === "h1" ? <h1 className="text-title font-semibold tracking-tight">{named}</h1>
            : heading === "h3" ? <h3 className="font-medium">{named}</h3>
            : <h2 className="text-section font-medium">{link ? <Link to="/questions/$name" params={{ name: question.name }} className="underline-offset-4 hover:underline">{named}</Link> : named}</h2>}
          <Badge variant={answer.freshness === "exact" || answer.freshness === "fresh" ? "secondary" : "destructive"} title={freshness.meaning}>{freshness.label}</Badge>
          {read.answer.origin === "live"
            ? <Badge variant="secondary" title="Read now from the cell as your account.">Live</Badge>
            : <Badge variant="outline" title="A document the engine produced, replayed exactly. Not a live connection.">Recorded snapshot</Badge>}
        </div>
        {refusal && <RefusedNote refusal={refusal} />}
        <p className="text-meta text-muted-foreground">{scope(question)} · {period(question.window)}</p>
      </div>
      <div className="space-y-2 px-1" data-slot="answer-lead">
        {lead.bucket && (
          <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
            {lead.bucket.suppressed ? <span className="text-figure leading-none font-semibold tracking-[-0.02em] text-muted-foreground">—</span> : (
              <AnswerProvenance variant="block" className="rounded-md text-start outline-none focus-visible:ring-3 focus-visible:ring-ring/30"
                label={lead.bucket.label} value={shown(lead.bucket)} provenance={provenanceOf(lead.bucket)} records={lead.bucket.members.length} onTrace={() => setOpen(lead.bucket)}>
                <span className="block text-figure leading-none font-semibold tracking-[-0.02em] tabular-nums underline decoration-dotted decoration-muted-foreground/40 underline-offset-8">{shown(lead.bucket)}</span>
              </AnswerProvenance>
            )}
            <span className="text-meta text-muted-foreground tabular-nums">{lead.bucket.label}{lead.bucket.totals.length > 1 ? ` · ${fraction(lead.bucket.totals)} = ${lead.bucket.value} exactly` : ""}</span>
          </div>
        )}
        <p className="max-w-[72ch] text-panel text-pretty">{lead.sentence}</p>
        <div className="flex flex-wrap items-center gap-1.5 text-meta text-muted-foreground" aria-label="As of">
          <Clock aria-hidden className="size-3.5" />
          <span className="rounded-md bg-muted px-1.5 py-0.5 tabular-nums" title="The cell's knowledge when it answered: its committed changes">Known after {changes(answer.knownAsOf)}</span>
          <span className="rounded-md bg-muted px-1.5 py-0.5 tabular-nums" title="The cell's clock when it answered">Read {stamp(answer.nowMs)}</span>
          {engineOf(answer) && <span className="rounded-md bg-muted px-1.5 py-0.5">{engineOf(answer)}</span>}
          {sources.length > 0 && <span className="rounded-md bg-muted px-1.5 py-0.5">Sources: {sources.join(", ")}</span>}
        </div>
      </div>
      <Panel icon={ChartLine} label={headline(question)} action={frame ? `${period(question.window)} · scale ${frame.scale[0]}${frame.unit}–${frame.scale[1]}${frame.unit}` : period(question.window)}>
        <Well className="px-2 pt-4 pb-1">
          {frame ? (
          <ChartContainer config={{ position: { label: headline(question), color: "var(--chart-1)" } }} className="h-56 w-full">
            <AreaChart data={[...frame.points]} margin={{ left: 8, right: 8 }}>
              <CartesianGrid vertical={false} strokeDasharray="2 4" />
              <XAxis dataKey="label" tickLine={false} axisLine={false} tickFormatter={(label: string) => label.replace(/^\w+ of /, "")} minTickGap={24} />
              <YAxis domain={[...frame.scale]} allowDataOverflow tickLine={false} axisLine={false} width={48} tickCount={frame.unit === "%" ? frame.scale[1] - frame.scale[0] + 1 : 5} tickFormatter={(tick: number) => `${tick}${frame.unit}`} />
              <ChartTooltip content={<ChartTooltipContent formatter={(_value, _name, item) => (
                <div className="flex w-full justify-between gap-4">
                  <span className="text-muted-foreground">{headline(question)}</span>
                  <span className="font-mono tabular-nums">{String(item.payload?.shown)}{item.payload?.position === null ? null : <span className="text-muted-foreground"> ({String(item.payload?.exact)})</span>}</span>
                </div>
              )} />} />
              <Area dataKey="position" type="monotone" stroke="var(--color-position)" strokeWidth={1.5} fill="var(--color-position)" fillOpacity={0.08} dot={false} isAnimationActive={false} />
            </AreaChart>
          </ChartContainer>
          ) : <EmptyState icon={ChartLine}>{chartEmpty(rows)}</EmptyState>}
        </Well>
      </Panel>
      <Panel icon={Rows3} label={grain ? `Every ${grain}` : "Every bucket"} action={rows.length === 0 ? undefined : `Select a ${grain ?? "bucket"} to see its records`}>
        {rows.length === 0 ? <Well><EmptyState icon={Rows3}>{chartEmpty(rows)}</EmptyState></Well> : <TableCard><Table>
          <TableHeader><TableRow><TableHead>{grain ? "Period" : "Bucket"}</TableHead><TableHead className="text-right">Answer</TableHead><TableHead className="text-right max-sm:hidden">Exactly</TableHead><TableHead className="text-right max-sm:hidden">Records</TableHead><TableHead className="w-10"><span className="sr-only">Drill</span></TableHead></TableRow></TableHeader>
          <TableBody>
            {rows.map((bucket) => (
              <TableRow key={bucket.key} className="cursor-pointer" onClick={() => setOpen(bucket)}>
                <TableCell>
                  <button type="button" className="rounded-sm underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none" onClick={(event) => { event.stopPropagation(); setOpen(bucket) }}>
                    {bucket.label}
                  </button>
                </TableCell>
                <TableCell className="text-right font-medium tabular-nums">
                  {bucket.suppressed ? shown(bucket) : <AnswerProvenance label={bucket.label} value={shown(bucket)} provenance={provenanceOf(bucket)} records={bucket.members.length} onTrace={() => setOpen(bucket)}>{shown(bucket)}</AnswerProvenance>}
                </TableCell>
                <TableCell className="text-right text-muted-foreground tabular-nums max-sm:hidden">{bucket.suppressed ? "" : `${fraction(bucket.totals)} = ${bucket.value}`}</TableCell>
                <TableCell className="text-right tabular-nums max-sm:hidden">{bucket.members.length}</TableCell>
                <TableCell className="w-10 p-1 text-right"><DrillMenu bucket={bucket} question={question} live={live} onRecords={() => setOpen(bucket)} onDrill={setDrill} /></TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table></TableCard>}
      </Panel>
      <DrillDown bucket={open} onClose={() => setOpen(undefined)} live={read.answer.origin === "live" ? { question: question.name, lineageRef: answer.lineageRef, fetchTrace, onStale } : undefined} />
      <Sheet open={drill !== undefined} onOpenChange={(next) => { if (!next) setDrill(undefined) }}>
        <SheetContent className="w-full overflow-y-auto sm:max-w-4xl">
          {drill && <>
            <SheetHeader>
              <SheetTitle>{named}, {drill.label.charAt(0).toLowerCase() + drill.label.slice(1)}</SheetTitle>
              <SheetDescription>A question derived from this answer, answered by the cell.</SheetDescription>
            </SheetHeader>
            <div className="px-4 pb-6"><DrillAnswer drill={drill} title={`${named} by ${drill.property.replaceAll("_", " ")}`} parent={read} /></div>
          </>}
        </SheetContent>
      </Sheet>
    </section>
  )
}

/** The cell's question catalog, for the properties a drill may break an answer out by. */
const readQuestionCatalog = () => cellRead<{ readonly objectTypes: readonly { readonly name: string; readonly properties: readonly CatalogProperty[] }[] }>("/v1/questions:catalog", "The question catalog is")

/**
 * One bucket's drill menu: its records with their receipts, then each question derived from it
 * (break out by a property the cell's catalog declares). A recorded snapshot offers only its records.
 */
function DrillMenu({ bucket, question, live, onRecords, onDrill }: {
  readonly bucket: Bucket
  readonly question: Question
  readonly live: boolean
  readonly onRecords: () => void
  readonly onDrill: (drill: Drill) => void
}) {
  const [open, setOpen] = useState(false)
  const catalog = useQuery({ queryKey: ["question-catalog"], queryFn: readQuestionCatalog, enabled: live && open, retry: false, staleTime: 60_000 })
  const properties = catalog.data?.document.objectTypes.find(({ name }) => name === question.objectSet)?.properties ?? []
  const drills = bucket.suppressed ? [] : breakOuts(question, bucket, properties)
  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger asChild>
        <Button type="button" variant="ghost" size="icon" className="size-7" aria-label={`Drill into ${bucket.label}`} onClick={(event) => event.stopPropagation()}><Ellipsis /></Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-56 rounded-node border-node-border shadow-float" onClick={(event) => event.stopPropagation()}>
        <DropdownMenuLabel>{bucket.label}</DropdownMenuLabel>
        <DropdownMenuItem onSelect={onRecords}><Rows3 />See the {bucket.members.length} {bucket.members.length === 1 ? "record" : "records"}</DropdownMenuItem>
        {live && <>
          <DropdownMenuSeparator />
          {catalog.isPending && <DropdownMenuItem disabled>Reading what the cell can break this out by…</DropdownMenuItem>}
          {catalog.isError && <DropdownMenuItem disabled>{catalog.error.message}</DropdownMenuItem>}
          {catalog.isSuccess && drills.length === 0 && <DropdownMenuItem disabled>The cell names nothing to break this out by</DropdownMenuItem>}
          {drills.map((drill) => <DropdownMenuItem key={drill.property} onSelect={() => onDrill(drill)}><Split />{drill.label}</DropdownMenuItem>)}
        </>}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

/** A drill's answer: the derived question as the cell answers it, refuses it, or why it is not run. */
function DrillAnswer({ drill, title, parent }: { readonly drill: Drill; readonly title: string; readonly parent: QuestionAnswer }) {
  const run = useQuery({ queryKey: ["drill", JSON.stringify(drill.question)], queryFn: () => evaluateQuestion(drill.question), retry: false })
  const result = run.data
  if (run.isPending) return <p className="text-sm text-muted-foreground">Asking the cell…</p>
  if (run.isError || result === undefined) return <p role="alert" className="text-sm text-destructive">Not run: {run.error instanceof Error ? run.error.message : "no answer"}</p>
  if (result.kind === "refused") return <RefusedNote refusal={result.refusal} />
  if (result.kind === "not-a-question") return <div role="status" className="space-y-1 rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm">{result.findings.map(({ code, message }) => <div key={`${code}:${message}`}><span className="font-medium">{message}</span> <span className="font-mono text-xs text-muted-foreground">{code}</span></div>)}</div>
  if (result.kind !== "answer") return <p role="status" className="text-sm text-muted-foreground">{result.kind === "too-large" ? result.message : result.kind === "bad-request" ? result.message : "The cell did not answer."}</p>
  const read: QuestionAnswer = { ...parent, question: readDocument<Question>(JSON.stringify(drill.question), "live"), answer: result.read }
  return <AnswerView read={read} title={title} heading="h3" fetchTrace={(lineageRef, bucket) => traceQuestion(drill.question, lineageRef, bucket, undefined, { knownAsOf: result.read.document.knownAsOf, nowMs: result.read.document.nowMs })} />
}
