import { useQuery } from "@tanstack/react-query"
import { NO_DATA_CELL } from "@/lib/definitions"
import { Link } from "@tanstack/react-router"
import { useState } from "react"
import { ChartLine, Clock, Rows3, Sigma } from "lucide-react"
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from "recharts"
import { Badge } from "@/components/ui/badge"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableFooter, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { buckets, contribution, declarations, fraction, shown, type Bucket } from "./lib/answer.ts"
import { chartEmpty, chartFrame } from "./lib/chart.ts"
import { FRESHNESS, day, headline, period, scope } from "./lib/format.ts"
import { amountTexts, liveTrace, type LiveRead, type Refusal, type Trace } from "./lib/live.ts"
import type { Read } from "./lib/exact.ts"
import type { QuestionAnswer } from "./lib/transport.ts"
import { CopyId } from "@/components/copy-id"
import { AnswerProvenance } from "@/components/provenance"
import { systemName } from "./lib/catalog.ts"
import { bucketProvenance } from "./lib/provenance.ts"

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
export function AnswerView({ read, heading = "h2", link = false, refusal, fetchTrace, onStale, openBucket }: {
  readonly read: QuestionAnswer
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
  const provenanceOf = (bucket: Bucket) => bucketProvenance(declared, answer, bucket, engineOf(answer) ?? null)
  const latest = rows.at(-1)
  const freshness = FRESHNESS[answer.freshness]
  const frame = chartFrame(rows)
  const sources = Object.entries(answer.watermarks).flatMap(([source, partitions]) =>
    Object.values(partitions).map(({ complete }) => `${source} ${complete ? "complete" : "incomplete"}`))
  return (
    <section className="space-y-3">
      <div className="space-y-1 px-1">
        <div className="flex flex-wrap items-center gap-2">
          {heading === "h1" ? <h1 className="text-title font-semibold tracking-tight">{headline(question)}</h1>
            : heading === "h3" ? <h3 className="font-medium">{headline(question)}</h3>
            : <h2 className="text-section font-medium">{link ? <Link to="/questions/$name" params={{ name: question.name }} className="underline-offset-4 hover:underline">{headline(question)}</Link> : headline(question)}</h2>}
          <Badge variant={answer.freshness === "exact" || answer.freshness === "fresh" ? "secondary" : "destructive"} title={freshness.meaning}>{freshness.label}</Badge>
          {read.answer.origin === "live"
            ? <Badge variant="secondary" title="Read now from the cell as your account.">Live</Badge>
            : <Badge variant="outline" title="A document the engine produced, replayed exactly. Not a live connection.">Recorded snapshot</Badge>}
        </div>
        {refusal && <RefusedNote refusal={refusal} />}
        <p className="text-meta text-muted-foreground">{scope(question)} · {period(question.window)} · Sources: {sources.join(", ")}</p>
      </div>
      {latest && (
        <StatStrip>
          <AnswerProvenance variant="block" className="block h-full rounded-lg text-start transition-transform duration-150 ease-spring outline-none focus-visible:ring-3 focus-visible:ring-ring/30 active:translate-y-px"
            label={latest.label} value={shown(latest)} provenance={provenanceOf(latest)} records={latest.members.length} onTrace={() => setOpen(latest)}>
            <KpiTile icon={Sigma} label={latest.label} value={shown(latest)} sub={latest.totals.length > 1 ? `${fraction(latest.totals)} = ${latest.value} exactly` : undefined} />
          </AnswerProvenance>
          <KpiTile icon={Rows3} label="Records counted" value={latest.members.length} sub={latest.label} />
          <KpiTile icon={Clock} label="Freshness" value={freshness.label} sub={<>{`Read ${day(answer.nowMs)}, after ${changes(answer.knownAsOf)}`}{engineOf(answer) && <span className="block">{engineOf(answer)}</span>}</>} />
        </StatStrip>
      )}
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
      <Panel icon={Rows3} label="Every week" action={rows.length === 0 ? undefined : "Select a week to see its records"}>
        {rows.length === 0 ? <Well><EmptyState icon={Rows3}>{chartEmpty(rows)}</EmptyState></Well> : <TableCard><Table>
          <TableHeader><TableRow><TableHead>Period</TableHead><TableHead className="text-right">Answer</TableHead><TableHead className="text-right max-sm:hidden">Exactly</TableHead><TableHead className="text-right max-sm:hidden">Records</TableHead></TableRow></TableHeader>
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
              </TableRow>
            ))}
          </TableBody>
        </Table></TableCard>}
      </Panel>
      <DrillDown bucket={open} onClose={() => setOpen(undefined)} live={read.answer.origin === "live" ? { question: question.name, lineageRef: answer.lineageRef, fetchTrace, onStale } : undefined} />
    </section>
  )
}
