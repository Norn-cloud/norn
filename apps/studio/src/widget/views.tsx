import { ChartLine, Clock, FileCheck2, Rows3, Sigma } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from "recharts"
import { LiveRecords, RefusedNote, type FetchTrace } from "@/answer-view"
import { CopyId } from "@/components/copy-id"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { EmptyState, Panel, Well } from "@/components/panel"
import { ProvenanceChain } from "@/components/provenance"
import { TableCard } from "@/components/table-card"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { SUPPRESSED, fraction, shown, type Bucket } from "@/lib/answer"
import { chartEmpty, chartFrame } from "@/lib/chart"
import { member, readDocument, type Read } from "@/lib/exact"
import { FRESHNESS, day } from "@/lib/format"
import type { Refusal, Trace } from "@/lib/live"
import type { Answer, SourceLineage } from "@/lib/transport"
import { documentText, type Bridge, type ToolResult } from "./bridge"

/**
 * What Norn renders inside a chat: one tool result, drawn with Studio's own components from the
 * cell's own documents. Numbers are the exact text the server sent (decimals keep their scale);
 * a refusal is shown in the server's words; more detail is asked of the same server tools.
 */

export type View = "answer" | "lineage" | "dashboard" | "receipt"

/** The tool's input and result as the host delivered them. */
export interface Delivery {
  readonly input?: Record<string, unknown>
  readonly result?: ToolResult
}

/** The document a tool returned, read exactly; null when the result carries none. */
export function exactDocument<T>(result: ToolResult): Read<T> | null {
  const text = documentText(result)
  if (text === null) return null
  try { return readDocument<T>(text, "live") } catch { return null }
}

const asRefusal = (value: unknown): Refusal | null => {
  const refusal = value as { code?: unknown; message?: unknown } | null
  return refusal !== null && typeof refusal === "object" && typeof refusal.code === "string" && typeof refusal.message === "string" ? refusal as Refusal : null
}

/** A bucket key is the canonical JSON of its grouping value; show the value, not the JSON. */
export function bucketTitle(key: string): string {
  try {
    const value: unknown = JSON.parse(key)
    if (typeof value === "string" || typeof value === "number") return String(value)
    if (Array.isArray(value)) return value.map(String).join(" · ")
    if (value !== null && typeof value === "object") return Object.values(value as Record<string, unknown>).map(String).join(" · ")
  } catch { /* not JSON: the key is its own label */ }
  return key
}

/** The answer's buckets with every number as the server's exact text. */
export function answerBuckets(read: Read<Answer>): Bucket[] {
  const text = (pointer: string) => read.numbers.get(pointer) ?? "unavailable"
  return Object.entries(read.document.rows).map(([key, row]) => {
    const path = `/rows/${member(key)}`
    const suppressed = (row as { suppressed?: unknown }).suppressed === true
    return {
      key,
      label: bucketTitle(key),
      value: suppressed ? SUPPRESSED : text(`${path}/value`),
      suppressed,
      totals: suppressed ? [] : row.totals.map((_, index) => text(`${path}/totals/${index}`)),
      members: Object.entries(row.members).map(([id, { version, amounts }]) => ({
        id,
        version,
        amounts: amounts === null ? null : amounts.map((amount, index) => amount === null ? null : text(`${path}/members/${member(id)}/amounts/${index}`)),
      })),
    }
  })
}

/** Trace one bucket through the server tool that answered it: the named question's, or the ad-hoc one's. */
export function traceThrough(bridge: Bridge, input: Record<string, unknown> | undefined): FetchTrace {
  return async (lineageRef, bucket) => {
    const question = input?.question
    const result = typeof question === "string"
      ? await bridge.callTool("norn_trace_answer", { question, lineageRef, bucket })
      : await bridge.callTool("norn_trace_evaluated", { question, lineageRef, bucket })
    const read = exactDocument<Trace>(result)
    if (result.isError === true) {
      const refusal = asRefusal(read?.document ?? result.structuredContent)
      return refusal === null ? { kind: "unavailable", status: 502 } : { kind: "refused", refusal }
    }
    return read === null ? { kind: "unavailable", status: 502 } : { kind: "read", read }
  }
}

function AnswerCard({ read, input, bridge }: { readonly read: Read<Answer>; readonly input?: Record<string, unknown>; readonly bridge: Bridge }) {
  const [open, setOpen] = useState<Bucket>()
  const answer = read.document
  const rows = answerBuckets(read)
  const latest = rows.at(-1)
  const freshness = FRESHNESS[answer.freshness]
  const frame = chartFrame(rows)
  const title = typeof input?.question === "string" ? input.question : "Answer"
  const fetchTrace = traceThrough(bridge, input)
  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-center gap-2 px-1">
        <h2 className="text-section font-medium">{title}</h2>
        <Badge variant={answer.freshness === "exact" || answer.freshness === "fresh" ? "secondary" : "destructive"} title={freshness.meaning}>{freshness.label}</Badge>
        <Badge variant="secondary" title="Read now from the cell as your account.">Live</Badge>
      </div>
      {latest && (
        <StatStrip>
          <button type="button" className="block h-full rounded-lg text-start outline-none focus-visible:ring-3 focus-visible:ring-ring/30" onClick={() => setOpen(latest)}>
            <KpiTile icon={Sigma} label={latest.label} value={shown(latest)} sub={latest.totals.length > 1 ? `${fraction(latest.totals)} = ${latest.value} exactly` : undefined} />
          </button>
          <KpiTile icon={Rows3} label="Records counted" value={latest.members.length} sub={latest.label} />
          <KpiTile icon={Clock} label="Freshness" value={freshness.label} sub={`Read ${day(answer.nowMs)}, after ${answer.knownAsOf} committed changes`} />
        </StatStrip>
      )}
      <Panel icon={ChartLine} label={title}>
        <Well className="px-2 pt-4 pb-1">
          {frame ? (
            <ChartContainer config={{ position: { label: title, color: "var(--chart-1)" } }} className="h-48 w-full">
              <AreaChart data={[...frame.points]} margin={{ left: 8, right: 8 }}>
                <CartesianGrid vertical={false} strokeDasharray="2 4" />
                <XAxis dataKey="label" tickLine={false} axisLine={false} minTickGap={24} />
                <YAxis domain={[...frame.scale]} allowDataOverflow tickLine={false} axisLine={false} width={48} tickFormatter={(tick: number) => `${tick}${frame.unit}`} />
                <ChartTooltip content={<ChartTooltipContent formatter={(_value, _name, item) => <span className="font-mono tabular-nums">{String(item.payload?.shown)}</span>} />} />
                <Area dataKey="position" type="monotone" stroke="var(--color-position)" strokeWidth={1.5} fill="var(--color-position)" fillOpacity={0.08} dot={false} isAnimationActive={false} />
              </AreaChart>
            </ChartContainer>
          ) : <EmptyState icon={ChartLine}>{chartEmpty(rows)}</EmptyState>}
        </Well>
      </Panel>
      <Panel icon={Rows3} label="Every bucket" action="Select one to see its records">
        <TableCard><Table>
          <TableHeader><TableRow><TableHead>Bucket</TableHead><TableHead className="text-right">Answer</TableHead><TableHead className="text-right">Records</TableHead></TableRow></TableHeader>
          <TableBody>
            {rows.map((bucket) => (
              <TableRow key={bucket.key} className="cursor-pointer" data-state={open?.key === bucket.key ? "selected" : undefined} onClick={() => setOpen(bucket)}>
                <TableCell>{bucket.label}</TableCell>
                <TableCell className="text-right font-medium tabular-nums">{shown(bucket)}</TableCell>
                <TableCell className="text-right tabular-nums">{bucket.members.length}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table></TableCard>
      </Panel>
      {open && (
        <Panel icon={Rows3} label={`${open.label}: the records behind ${shown(open)}`}>
          <LiveRecords question={title} lineageRef={answer.lineageRef} bucket={open} fetchTrace={fetchTrace} />
        </Panel>
      )}
    </section>
  )
}

/** One bucket's records exactly as the trace tool returned them, through Studio's own records table. */
function TraceCard({ read, input }: { readonly read: Read<Trace>; readonly input?: Record<string, unknown> }) {
  const bucket = typeof input?.bucket === "string" ? input.bucket : read.document.bucket
  const fetchTrace: FetchTrace = async () => ({ kind: "read", read })
  const asBucket: Bucket = { key: bucket, label: bucketTitle(bucket), value: "", totals: [], members: [] }
  const restricted = read.document.contributions.filter((contribution) => contribution.restricted).length
  return (
    <section className="space-y-3">
      <div className="space-y-1 px-1">
        <h2 className="text-section font-medium">Where {bucketTitle(bucket)} comes from</h2>
        <p className="text-meta text-muted-foreground">
          {read.document.contributions.length} records, each with the commit and receipt that recorded it{restricted > 0 ? `; ${restricted} withheld from your role` : ""}.
        </p>
      </div>
      <LiveRecords question={typeof input?.question === "string" ? input.question : "trace"} lineageRef={typeof input?.lineageRef === "string" ? input.lineageRef : ""} bucket={asBucket} fetchTrace={fetchTrace} />
    </section>
  )
}

/** One record's source lineage: for each recorded value, the source row, mapping and digests. */
function ObjectLineage({ read }: { readonly read: Read<SourceLineage> }) {
  const facts = Object.keys(read.document.fieldProvenance)
  if (facts.length === 0) return <EmptyState icon={Rows3}>The server recorded no source for this record.</EmptyState>
  return (
    <section className="space-y-3">
      {facts.map((fact) => (
        <Panel key={fact} icon={Rows3} label={fact}><div className="p-3"><ProvenanceChain lineage={read.document} fact={fact} /></div></Panel>
      ))}
    </section>
  )
}

interface Admission {
  readonly admitted: boolean
  readonly diagnostics?: readonly { readonly code: string; readonly severity?: string; readonly message: string; readonly span?: { readonly pointer?: string } }[]
  readonly restatement?: string
  readonly restatementSha256?: string
}

/** A proposed dashboard or question: the checker's verdict, its plain-words restatement and every finding. */
function ProposalCard({ read, input, studio, bridge }: { readonly read: Read<Admission>; readonly input?: Record<string, unknown>; readonly studio: string | null; readonly bridge: Bridge }) {
  const { admitted, diagnostics = [], restatement } = read.document
  const kind = typeof input?.kind === "string" ? input.kind : "definition"
  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-center gap-2 px-1">
        <h2 className="text-section font-medium">{typeof input?.id === "string" ? input.id : `Proposed ${kind}`}</h2>
        <Badge variant={admitted ? "secondary" : "destructive"}>{admitted ? "The checker admits it" : "The checker refuses it"}</Badge>
      </div>
      {restatement && <Panel icon={FileCheck2} label="What it says, in plain words"><p className="whitespace-pre-wrap p-3 text-sm">{restatement}</p></Panel>}
      {diagnostics.length > 0 && (
        <TableCard><Table>
          <TableHeader><TableRow><TableHead>Finding</TableHead><TableHead>Where</TableHead></TableRow></TableHeader>
          <TableBody>{diagnostics.map((diagnostic, index) => (
            <TableRow key={`${diagnostic.code}-${index}`}><TableCell><div>{diagnostic.message}</div><div className="font-mono text-xs text-muted-foreground">{diagnostic.code}</div></TableCell><TableCell className="font-mono text-xs">{diagnostic.span?.pointer ?? ""}</TableCell></TableRow>
          ))}</TableBody>
        </Table></TableCard>
      )}
      <p className="px-1 text-sm text-muted-foreground">Nothing is saved yet. A person installs it in Norn Studio by signing this restatement.</p>
      {admitted && studio !== null && <Button variant="outline" onClick={() => bridge.openLink(new URL("/dashboards", studio).href)}>Open Norn Studio</Button>}
    </section>
  )
}

interface Receipt {
  readonly outcome: string
  readonly objectType?: string
  readonly objectId?: string
  readonly version?: number
  readonly receipt?: { readonly receiptHash: string; readonly commitSeq: number; readonly recordedAtMs: number }
}

/** A governed action's answer: accepted with its receipt, or refused with the server's reason and no effects. */
function ReceiptCard({ read, tool }: { readonly read: Read<Receipt>; readonly tool?: string }) {
  const { outcome, objectType, objectId, version, receipt } = read.document
  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-center gap-2 px-1">
        <h2 className="text-section font-medium">{tool ?? "Action"}</h2>
        <Badge variant="secondary">{outcome === "replayed" ? "Already done: the original receipt" : "Accepted"}</Badge>
      </div>
      <StatStrip>
        {objectId !== undefined && <KpiTile icon={Rows3} label={objectType ?? "Record"} value={<span className="font-mono text-base">{objectId}</span>} sub={version === undefined ? undefined : `version ${version}`} />}
        {receipt && <KpiTile icon={FileCheck2} label="Receipt" value={<CopyId value={receipt.receiptHash} short={10} />} sub={`commit ${receipt.commitSeq} · ${day(receipt.recordedAtMs)}`} />}
      </StatStrip>
    </section>
  )
}

/** Refusals keep the server's words and code; nothing was changed. */
function Refused({ refusal, action }: { readonly refusal: Refusal; readonly action: boolean }) {
  return (
    <section className="space-y-2">
      <RefusedNote refusal={refusal} />
      {action && <p className="px-1 text-sm text-muted-foreground">Nothing was changed.</p>}
    </section>
  )
}

/** The view for one delivery, chosen by the template the host loaded and the document's own shape. */
export function DeliveryView({ view, delivery, bridge, studio, tool }: { readonly view: View; readonly delivery: Delivery; readonly bridge: Bridge; readonly studio: string | null; readonly tool?: string }) {
  const { input, result } = delivery
  if (result === undefined) return <p className="p-3 text-sm text-muted-foreground">Asking Norn…</p>
  const read = exactDocument<unknown>(result)
  if (result.isError === true || read === null) {
    const refusal = asRefusal(read?.document ?? result.structuredContent)
    return refusal === null ? <p className="p-3 text-sm text-muted-foreground">Norn sent nothing to show.</p> : <Refused refusal={refusal} action={view === "receipt"} />
  }
  const document = read.document as Record<string, unknown>
  if (view === "answer" && typeof document.lineageRef === "string" && typeof document.rows === "object") return <AnswerCard read={read as Read<Answer>} input={input} bridge={bridge} />
  if (view === "lineage" && Array.isArray(document.contributions)) return <TraceCard read={read as Read<Trace>} input={input} />
  if (view === "lineage" && typeof document.fieldProvenance === "object") return <ObjectLineage read={read as Read<SourceLineage>} />
  if (view === "dashboard" && typeof document.admitted === "boolean") return <ProposalCard read={read as Read<Admission>} input={input} studio={studio} bridge={bridge} />
  if (view === "receipt" && typeof document.outcome === "string") return <ReceiptCard read={read as Read<Receipt>} tool={tool} />
  return <p className="p-3 text-sm text-muted-foreground">Norn answered with a document this view does not draw.</p>
}

/** Follow the host's tool input and result as they arrive. */
export function useDelivery(bridge: Bridge): Delivery {
  const [delivery, setDelivery] = useState<Delivery>({})
  useEffect(() => {
    const offInput = bridge.on("ui/notifications/tool-input", (params) => setDelivery((current) => ({ ...current, input: (params as { arguments?: Record<string, unknown> } | null)?.arguments })))
    const offResult = bridge.on("ui/notifications/tool-result", (params) => setDelivery((current) => ({ ...current, result: params as ToolResult })))
    return () => { offInput(); offResult() }
  }, [bridge])
  return delivery
}

export function Frame({ children }: { readonly children: ReactNode }) {
  return <div className="bg-background p-3 text-foreground">{children}</div>
}

