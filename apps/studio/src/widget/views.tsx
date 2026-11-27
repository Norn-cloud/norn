import { useEffect, useRef, useState, type ReactNode } from "react"
import type { FetchTrace } from "@/answer-view"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { SUPPRESSED, type Bucket } from "@/lib/answer"
import { member, readDocument, type Read } from "@/lib/exact"
import { FRESHNESS } from "@/lib/format"
import type { Refusal, Trace } from "@/lib/live"
import type { Answer, SourceLineage } from "@/lib/transport"
import { documentText, type Bridge, type ToolHost, type ToolResult } from "./bridge"

/**
 * What Norn renders inside a chat: one tool result, drawn with Studio's own components from the
 * cell's own documents. Numbers are the exact text the server sent (decimals keep their scale);
 * a refusal is shown in the server's words; more detail is asked of the same server tools.
 */

export type View = "answer" | "lineage" | "dashboard" | "receipt" | "records" | "findings" | "plan" | "inbox"

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
export function traceThrough(bridge: ToolHost, input: Record<string, unknown> | undefined): FetchTrace {
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

/** A status chip: neutral, with colour only in its dot. */
function Chip({ tone, children, title }: { readonly tone?: "ok" | "bad" | "warn"; readonly children: ReactNode; readonly title?: string }) {
  const dot = tone === "ok" ? "bg-success" : tone === "bad" ? "bg-destructive" : tone === "warn" ? "bg-warning" : null
  return (
    <Badge variant="outline" className="gap-1.5 font-normal text-muted-foreground" title={title}>
      {dot && <span aria-hidden className={`size-1.5 rounded-full ${dot}`} />}
      {children}
    </Badge>
  )
}

/** Exact decimal text with thousands grouped; the digits are the server's, never re-rounded. */
export function grouped(text: string): string {
  const match = /^(-?)(\d+)(\.\d+)?$/.exec(text)
  if (match === null) return text
  const [, sign, whole, fraction = ""] = match
  return `${sign}${(whole as string).replace(/\B(?=(\d{3})+(?!\d))/g, ",")}${fraction}`
}

/** A member key (`demo/holding/owner/east/gold`) as its last two segments: `east / gold`. */
const memberLabel = (id: string) => id.split("/").filter(Boolean).slice(-2).join(" / ") || id
const short = (hash: string | null | undefined) => (hash ? `${hash.slice(0, 10)}…` : "—")

/** A bar table: label, a monochrome bar scaled to the largest value, and the exact value. */
function BarTable({ rows, label, valueLabel, onSelect }: { readonly rows: readonly { readonly id?: string; readonly key: string; readonly label: string; readonly value: string }[]; readonly label: string; readonly valueLabel: string; readonly onSelect?: (key: string) => void }) {
  const max = Math.max(0, ...rows.map((row) => Math.abs(Number(row.value)) || 0))
  return (
    <Table>
      <TableHeader><TableRow><TableHead>{label}</TableHead><TableHead className="w-[45%]" /><TableHead className="text-end">{valueLabel}</TableHead></TableRow></TableHeader>
      <TableBody>
        {rows.map((row) => (
          <TableRow key={row.id ?? row.key} className={onSelect ? "cursor-pointer" : undefined} onClick={onSelect ? () => onSelect(row.key) : undefined}>
            <TableCell>{row.label}</TableCell>
            <TableCell><div className="h-1.5 rounded-full bg-foreground/70" style={{ width: `${max > 0 ? Math.max(2, (Math.abs(Number(row.value)) / max) * 100) : 0}%` }} /></TableCell>
            <TableCell className="text-end tabular-nums">{grouped(row.value)}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

function AnswerCard({ read, input, bridge }: { readonly read: Read<Answer>; readonly input?: Record<string, unknown>; readonly bridge: ToolHost }) {
  const [traced, setTraced] = useState<{ readonly bucket: string; readonly read: Read<Trace> } | { readonly bucket: string; readonly refusal: Refusal } | { readonly bucket: string; readonly unavailable: true } | null>(null)
  const traceSeq = useRef(0)
  const mounted = useRef(true)
  const answer = read.document
  useEffect(() => {
    mounted.current = true
    return () => { mounted.current = false }
  }, [])
  useEffect(() => {
    traceSeq.current += 1
    setTraced(null)
  }, [answer.lineageRef])
  const buckets = answerBuckets(read)
  const latest = buckets.at(-1)
  const freshness = FRESHNESS[answer.freshness]
  const engine = (answer as { engine?: unknown }).engine
  const question = input?.question
  const title = typeof question === "string" ? question : typeof (question as { name?: unknown } | undefined)?.name === "string" ? String((question as { name: string }).name) : "Answer"
  const fetchTrace = traceThrough(bridge, input)
  const trace = async (bucket: string) => {
    const seq = ++traceSeq.current
    try {
      const result = await fetchTrace(answer.lineageRef, bucket)
      if (!mounted.current || traceSeq.current !== seq) return
      if (result.kind === "read") setTraced({ bucket, read: result.read })
      else if (result.kind === "refused") setTraced({ bucket, refusal: result.refusal })
      else setTraced({ bucket, unavailable: true })
    } catch {
      if (!mounted.current || traceSeq.current !== seq) return
      setTraced({ bucket, unavailable: true })
    }
  }
  // One bucket: its members are the breakdown. Several (a series): the buckets are.
  const rows = buckets.length === 1 && latest
    ? latest.members.map((entry, index) => ({ id: `${entry.id}:${index}`, key: latest.key, label: memberLabel(entry.id), value: entry.amounts?.[0] ?? "—" })).sort((a, b) => Number(b.value) - Number(a.value))
    : buckets.map((bucket) => ({ key: bucket.key, label: bucket.label, value: bucket.suppressed ? SUPPRESSED : bucket.value }))
  return (
    <Card size="sm">
      <CardHeader>
        <CardDescription>{title}{latest && buckets.length === 1 ? `, ${latest.label}` : ""}</CardDescription>
        <CardTitle className="text-figure tabular-nums">{latest ? (latest.suppressed ? SUPPRESSED : grouped(latest.value)) : "—"}</CardTitle>
        <CardAction className="flex flex-wrap justify-end gap-1">
          <Chip tone={answer.freshness === "exact" || answer.freshness === "fresh" ? "ok" : "warn"} title={freshness.meaning}>{answer.freshness}</Chip>
          {typeof engine === "string" && <Chip>{engine}</Chip>}
          {latest && <Chip>{latest.members.length} records</Chip>}
          <Chip>as of commit {answer.knownAsOf}</Chip>
        </CardAction>
      </CardHeader>
      <CardContent>
        {rows.length > 0 ? <BarTable rows={rows} label={buckets.length === 1 ? "Record" : "Bucket"} valueLabel="Value" onSelect={(key) => void trace(key)} /> : <p className="text-body text-muted-foreground">No records in this answer.</p>}
      </CardContent>
      {traced && "read" in traced && <CardContent><LineageTable read={traced.read} /></CardContent>}
      {traced && "refusal" in traced && <CardContent><RefusalBody refusal={traced.refusal} /></CardContent>}
      {traced && "unavailable" in traced && <CardContent><p role="status" className="text-body text-muted-foreground">Lineage trace is unavailable right now.</p></CardContent>}
      <CardFooter className="text-meta text-muted-foreground">Select a row to trace it to its source records.</CardFooter>
    </Card>
  )
}

/** One bucket's contributions: value, source system, source record, receipt. */
function LineageTable({ read }: { readonly read: Read<Trace> }) {
  const contributions = read.document.contributions
  // A sum has one amount; a ratio its numerator and denominator, each the server's exact text.
  const text = (index: number) => (contributions[index]?.amounts ?? []).map((_, at) => grouped(read.numbers.get(`/contributions/${index}/amounts/${at}`) ?? "—")).join(" / ") || "—"
  const mapping = contributions.flatMap((contribution) => {
    const provenance = contribution.source.status === "resolved" ? contribution.source.provenance : undefined
    const fields = (provenance as { fieldProvenance?: Record<string, { mappingVersion?: string; sourceFields?: string[] }> } | undefined)?.fieldProvenance
    return fields ? Object.entries(fields).slice(0, 1).map(([name, field]) => `${name} ← ${field.mappingVersion ?? "mapping"} (${(field.sourceFields ?? []).join(", ")})`) : []
  })[0]
  return (
    <div className="space-y-2">
      <Table>
        <TableHeader><TableRow><TableHead className="text-end">Value</TableHead><TableHead>Source system</TableHead><TableHead>Source record</TableHead><TableHead>Receipt</TableHead></TableRow></TableHeader>
        <TableBody>
          {contributions.map((contribution, index) => {
            const ref = contribution.source.status === "resolved" ? (contribution.source.provenance as { documentRef?: { system?: string; record?: string } } | undefined)?.documentRef : undefined
            return (
              <TableRow key={`${contribution.fact.key}-${index}`}>
                <TableCell className="text-end tabular-nums">{contribution.restricted ? "withheld" : text(index)}</TableCell>
                <TableCell>{ref?.system ?? contribution.source.status}</TableCell>
                <TableCell className="font-mono text-meta">{ref?.record ?? contribution.fact.key}</TableCell>
                <TableCell className="font-mono text-meta" title={contribution.receiptHash ?? undefined}>{short(contribution.receiptHash)}</TableCell>
              </TableRow>
            )
          })}
        </TableBody>
      </Table>
      {mapping && <p className="text-meta text-muted-foreground">Each field carries its mapping version and source fields, e.g. {mapping}.</p>}
    </div>
  )
}

function TraceCard({ read, input }: { readonly read: Read<Trace>; readonly input?: Record<string, unknown> }) {
  const bucket = typeof input?.bucket === "string" ? input.bucket : read.document.bucket
  const restricted = read.document.contributions.filter((contribution) => contribution.restricted).length
  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>Where {bucketTitle(bucket)} comes from</CardTitle>
        <CardDescription>Every value traced to its source record and receipt.</CardDescription>
        <CardAction><Chip>{read.document.contributions.length} contributions{restricted > 0 ? `, ${restricted} withheld` : ""}</Chip></CardAction>
      </CardHeader>
      <CardContent><LineageTable read={read} /></CardContent>
    </Card>
  )
}

/** One record's source lineage: for each recorded value, the source record, mapping and fields. */
function ObjectLineage({ read }: { readonly read: Read<SourceLineage> }) {
  const lineage = read.document as SourceLineage & { readonly documentRef?: { readonly system?: string; readonly record?: string } }
  const fields = Object.entries(lineage.fieldProvenance as Record<string, { mappingVersion?: string; transform?: string; sourceFields?: string[]; source?: { system?: string; record?: string } }>)
  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>Where this record comes from</CardTitle>
        <CardDescription className="font-mono text-meta">{lineage.documentRef?.system} · {lineage.documentRef?.record}</CardDescription>
      </CardHeader>
      <CardContent>
        {fields.length === 0 ? <p className="text-body text-muted-foreground">The server recorded no source for this record.</p> : (
          <Table>
            <TableHeader><TableRow><TableHead>Field</TableHead><TableHead>Source fields</TableHead><TableHead>Mapping</TableHead></TableRow></TableHeader>
            <TableBody>{fields.map(([name, field]) => (
              <TableRow key={name}><TableCell>{name}</TableCell><TableCell className="font-mono text-meta">{(field.sourceFields ?? []).join(", ")}</TableCell><TableCell className="font-mono text-meta">{field.mappingVersion}</TableCell></TableRow>
            ))}</TableBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}

interface RecordsPage {
  readonly objectType: string
  readonly records: readonly { readonly id: string; readonly values: Record<string, unknown>; readonly masked?: Record<string, { readonly mode?: string; readonly markings?: readonly string[]; readonly revealable?: boolean }>; readonly withheldProperties?: readonly string[] }[]
  readonly total?: { readonly count?: number; readonly approximate?: boolean }
  readonly quarantineCount?: number
  readonly next?: string | null
}

/** A page of records: a table of their visible values; masked fields are the server's pseudonyms. */
function RecordsCard({ read }: { readonly read: Read<RecordsPage> }) {
  const page = read.document
  const columns = [...new Set(page.records.flatMap((record) => Object.keys(record.values)))].filter((column) => column !== "id").slice(0, 6)
  const masked = new Map<string, { readonly markings: readonly string[]; readonly revealable: boolean }>()
  for (const record of page.records) for (const [name, mask] of Object.entries(record.masked ?? {})) masked.set(name, { markings: mask.markings ?? [], revealable: mask.revealable === true })
  const cell = (index: number, column: string, value: unknown) => {
    if (value === null || value === undefined) return "—"
    if (typeof value === "number") return grouped(read.numbers.get(`/records/${index}/values/${member(column)}`) ?? String(value))
    return typeof value === "object" ? JSON.stringify(value) : String(value)
  }
  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>{page.objectType}</CardTitle>
        <CardAction className="flex flex-wrap justify-end gap-1">
          <Chip>{page.total?.count ?? page.records.length}{page.total?.approximate ? "+" : ""} records</Chip>
          {(page.quarantineCount ?? 0) > 0 && <Chip tone="bad">{page.quarantineCount} held back</Chip>}
        </CardAction>
      </CardHeader>
      <CardContent>
        <Table>
          <TableHeader><TableRow><TableHead>Id</TableHead>{columns.map((column) => <TableHead key={column}>{column.replaceAll("_", " ")}</TableHead>)}</TableRow></TableHeader>
          <TableBody>{page.records.map((record, index) => (
            <TableRow key={record.id}>
              <TableCell className="font-mono text-meta">{record.id}</TableCell>
              {columns.map((column) => <TableCell key={column} className={typeof record.values[column] === "number" ? "text-end tabular-nums" : undefined}>{record.withheldProperties?.includes(column) ? "withheld" : cell(index, column, record.values[column])}</TableCell>)}
            </TableRow>
          ))}</TableBody>
        </Table>
      </CardContent>
      {masked.size > 0 && (
        <CardFooter className="flex-col items-start gap-1 text-meta text-muted-foreground">
          {[...masked].map(([name, mask]) => (
            <span key={name}>{name.replaceAll("_", " ")} is a pseudonym ({mask.markings.join(", ")}) · reveal: <b className="text-foreground">{mask.revealable ? "allowed" : "not allowed"}</b>{mask.revealable ? ", and every reveal leaves a receipt" : ""}</span>
          ))}
        </CardFooter>
      )}
    </Card>
  )
}

interface Finding {
  readonly id?: string
  readonly law?: string
  readonly ruleRef?: { readonly lawId?: string }
  readonly message?: string
  readonly status?: string
  readonly severity?: string
  readonly firstSeen?: string
  readonly values?: Record<string, unknown>
}
interface Findings {
  readonly objectType: string
  readonly id?: string
  readonly quarantined?: boolean
  readonly findings?: readonly Finding[]
  readonly items?: readonly { readonly id: string; readonly openFindings?: number; readonly findings?: readonly Finding[] }[]
  readonly remedies?: readonly string[]
}

/** The values a law compared, exactly as the server sent them. */
function Compared({ read, pointer, values }: { readonly read: Read<unknown>; readonly pointer: string; readonly values: Record<string, unknown> }) {
  const shownValue = (key: string, value: unknown, path: string): string => Array.isArray(value)
    ? value.map((item, index) => shownValue(key, item, `${path}/${index}`)).join(" + ")
    : typeof value === "number" ? grouped(read.numbers.get(path) ?? String(value)) : String(value)
  return (
    <span className="tabular-nums">{Object.entries(values).map(([key, value], index) => (
      <span key={key}>{index > 0 ? " · " : ""}{key} <b>{shownValue(key, value, `${pointer}/${member(key)}`)}</b></span>
    ))}</span>
  )
}

/** Held-back records, or one record's findings: the law, its message, the values it compared and the remedy. */
function FindingsCard({ read }: { readonly read: Read<Findings> }) {
  const document = read.document
  const remedies = document.remedies ?? []
  if (document.items !== undefined) {
    return (
      <Card size="sm">
        <CardHeader>
          <CardTitle>Held back: {document.objectType}</CardTitle>
          <CardAction><Chip tone={document.items.length > 0 ? "bad" : "ok"}>{document.items.length} held</Chip></CardAction>
        </CardHeader>
        <CardContent>
          <Table>
            <TableHeader><TableRow><TableHead>Record</TableHead><TableHead>Law</TableHead><TableHead>Values compared</TableHead></TableRow></TableHeader>
            <TableBody>{document.items.map((item, index) => {
              const finding = item.findings?.[0]
              return (
                <TableRow key={item.id}>
                  <TableCell className="font-mono text-meta">{item.id}</TableCell>
                  <TableCell className="font-mono text-meta">{finding?.law ?? `${item.openFindings ?? 0} open`}</TableCell>
                  <TableCell>{finding?.values ? <Compared read={read as Read<unknown>} pointer={`/items/${index}/findings/0/values`} values={finding.values} /> : "—"}</TableCell>
                </TableRow>
              )
            })}</TableBody>
          </Table>
        </CardContent>
        {remedies.length > 0 && <CardFooter className="text-meta text-muted-foreground">Fix with {remedies.join(", ")}; the laws decide again.</CardFooter>}
      </Card>
    )
  }
  const findings = document.findings ?? []
  return (
    <Card size="sm">
      <CardHeader>
        <CardDescription className="font-mono text-meta">{document.objectType} · {document.id}</CardDescription>
        <CardTitle>{document.quarantined ? "Held back by law" : findings.length > 0 ? "Findings" : "No findings"}</CardTitle>
        <CardAction><Chip tone={document.quarantined ? "bad" : "ok"}>{document.quarantined ? "held" : "released"}</Chip></CardAction>
      </CardHeader>
      <CardContent className="space-y-3">
        {findings.map((finding, index) => (
          <div key={finding.id ?? index} className="space-y-1 text-body">
            <div className="font-mono text-meta text-muted-foreground">{finding.ruleRef?.lawId ?? finding.law}</div>
            {finding.values && <div><Compared read={read as Read<unknown>} pointer={`/findings/${index}/values`} values={finding.values} /></div>}
            <div className="text-muted-foreground">{finding.message?.replace(/^[^:]+:\s*/, "")}</div>
            <div className="text-meta text-muted-foreground">{finding.status ?? "open"}{finding.firstSeen ? ` since ${finding.firstSeen.slice(0, 10)}` : ""}</div>
          </div>
        ))}
      </CardContent>
      {remedies.length > 0 && <CardFooter className="text-meta text-muted-foreground">Fix with {remedies.join(", ")}: the laws decide again, and answer with a receipt or the reason.</CardFooter>}
    </Card>
  )
}

interface Planned { readonly artifactId: string; readonly kind: string; readonly fromVersion?: string; readonly toVersion: string }
interface Plan {
  readonly baseVersion: number
  readonly nextVersion: number
  readonly unchanged: boolean
  readonly added: readonly Planned[]
  readonly changed: readonly Planned[]
  readonly same: readonly string[]
  readonly newObjectTypes: readonly string[]
  readonly checkedObjects: number
  readonly diagnostics: readonly { readonly code: string; readonly message: string }[]
}

/** A planned definitions change: what it adds and replaces, what it found, and the version apply would install. */
function PlanCard({ plan, title }: { readonly plan: Plan; readonly title?: string }) {
  const rows = [...plan.added.map((artifact) => ({ artifact, change: "add" })), ...plan.changed.map((artifact) => ({ artifact, change: "replace" }))]
  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>{title ?? (plan.unchanged ? "Nothing to change" : `Plan: version ${plan.baseVersion} → ${plan.nextVersion}`)}</CardTitle>
        <CardAction className="flex flex-wrap justify-end gap-1">
          <Chip>{plan.added.length} added</Chip>
          <Chip>{plan.changed.length} replaced</Chip>
          <Chip tone={plan.diagnostics.length > 0 ? "warn" : "ok"}>{plan.checkedObjects} records checked</Chip>
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-3">
        {plan.newObjectTypes.length > 0 && <p className="text-body">New types: {plan.newObjectTypes.join(", ")}</p>}
        {rows.length > 0 && (
          <Table>
            <TableHeader><TableRow><TableHead>Change</TableHead><TableHead>Artifact</TableHead><TableHead>Kind</TableHead><TableHead>Version</TableHead></TableRow></TableHeader>
            <TableBody>{rows.map(({ artifact, change }) => (
              <TableRow key={artifact.artifactId}><TableCell>{change}</TableCell><TableCell className="font-mono text-meta">{artifact.artifactId}</TableCell><TableCell>{artifact.kind}</TableCell><TableCell className="font-mono text-meta">{artifact.fromVersion ? `${artifact.fromVersion} → ` : ""}{artifact.toVersion}</TableCell></TableRow>
            ))}</TableBody>
          </Table>
        )}
        {plan.diagnostics.map((diagnostic, index) => <p key={`${diagnostic.code}-${index}`} className="text-body"><span className="font-mono text-meta text-muted-foreground">{diagnostic.code}</span> {diagnostic.message}</p>)}
      </CardContent>
      {!plan.unchanged && title === undefined && <CardFooter className="text-meta text-muted-foreground">Nothing is installed yet. Apply with expectedVersion {plan.baseVersion}.</CardFooter>}
    </Card>
  )
}

interface InboxRecord {
  readonly id: string
  readonly finding?: { readonly words?: string; readonly message?: string } | null
  readonly suggestedFix?: { readonly title?: string; readonly label?: string; readonly needs?: readonly string[] } | null
}
interface Inbox {
  readonly items: readonly ({ readonly kind: "held_back"; readonly title: string; readonly objectType: string; readonly count: number; readonly approximate?: boolean; readonly records: readonly InboxRecord[] }
    | { readonly kind: "decision"; readonly decision: { readonly question?: string; readonly risk?: unknown } }
    | { readonly kind: "source_error"; readonly id: string; readonly values?: Record<string, unknown> })[]
  readonly sections: readonly { readonly kind: string; readonly count: number; readonly approximate?: boolean }[]
}

const SECTION_LABEL: Readonly<Record<string, string>> = { held_back: "held back", decision: "decisions", source_error: "source errors" }

/** What needs the caller's attention: held-back records by law with the server's fix, decisions, failing sources. */
function InboxCard({ read }: { readonly read: Read<Inbox> }) {
  const { items, sections } = read.document
  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>What needs your attention</CardTitle>
        <CardAction className="flex flex-wrap justify-end gap-1">
          {sections.map((section) => <Chip key={section.kind} tone={section.count > 0 ? (section.kind === "decision" ? "warn" : "bad") : "ok"}>{section.count}{section.approximate ? "+" : ""} {SECTION_LABEL[section.kind] ?? section.kind}</Chip>)}
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-4">
        {items.length === 0 && <p className="text-body text-muted-foreground">Nothing needs you right now.</p>}
        {items.map((item, index) => {
          if (item.kind === "held_back") return (
            <div key={index} className="space-y-1">
              <div className="text-body">{item.title} <span className="font-normal text-muted-foreground">· {item.count}{item.approximate ? "+" : ""} {item.objectType}</span></div>
              <Table>
                <TableHeader><TableRow><TableHead>Record</TableHead><TableHead>Finding</TableHead><TableHead>Fix</TableHead></TableRow></TableHeader>
                <TableBody>{item.records.map((record) => (
                  <TableRow key={record.id}>
                    <TableCell className="font-mono text-meta">{record.id}</TableCell>
                    <TableCell className="tabular-nums">{record.finding?.words ?? "—"}</TableCell>
                    <TableCell>{record.suggestedFix ? <span title={record.suggestedFix.title}>{record.suggestedFix.label}{record.suggestedFix.needs?.length ? <span className="text-muted-foreground"> · needs {record.suggestedFix.needs.join(", ")}</span> : null}</span> : "—"}</TableCell>
                  </TableRow>
                ))}</TableBody>
              </Table>
            </div>
          )
          if (item.kind === "decision") return <div key={index} className="text-body"><Chip tone="warn">decision</Chip> {item.decision.question ?? "A decision waits on you."}</div>
          return <div key={index} className="text-body"><Chip tone="bad">source</Chip> <span className="font-mono text-meta">{item.id}</span> last sync failed</div>
        })}
      </CardContent>
      <CardFooter className="text-meta text-muted-foreground">Fixes are the server's suggestions; each is applied only through its action tool, and the laws decide again.</CardFooter>
    </Card>
  )
}

interface Admission {
  readonly admitted: boolean
  readonly diagnostics?: readonly { readonly code: string; readonly severity?: string; readonly message: string; readonly span?: { readonly pointer?: string } }[]
  readonly restatement?: string
  readonly restatementSha256?: string
}

/** A proposed dashboard or question: the checker's verdict, its plain-words restatement and every finding. */
function ProposalCard({ read, input, studio, bridge }: { readonly read: Read<Admission>; readonly input?: Record<string, unknown>; readonly studio: string | null; readonly bridge: ToolHost }) {
  const { admitted, diagnostics = [], restatement } = read.document
  const kind = typeof input?.kind === "string" ? input.kind : "definition"
  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>{typeof input?.id === "string" ? input.id : `Proposed ${kind}`}</CardTitle>
        <CardAction><Chip tone={admitted ? "ok" : "bad"}>{admitted ? "admitted" : "refused"}</Chip></CardAction>
      </CardHeader>
      <CardContent className="space-y-3">
        {restatement && <p className="whitespace-pre-wrap text-body">{restatement}</p>}
        {diagnostics.length > 0 && (
          <Table>
            <TableHeader><TableRow><TableHead>Finding</TableHead><TableHead>Where</TableHead></TableRow></TableHeader>
            <TableBody>{diagnostics.map((diagnostic, index) => (
              <TableRow key={`${diagnostic.code}-${index}`}><TableCell><div>{diagnostic.message}</div><div className="font-mono text-meta text-muted-foreground">{diagnostic.code}</div></TableCell><TableCell className="font-mono text-meta">{diagnostic.span?.pointer ?? ""}</TableCell></TableRow>
            ))}</TableBody>
          </Table>
        )}
      </CardContent>
      <CardFooter className="justify-between gap-2 text-meta text-muted-foreground">
        <span>Nothing is saved yet. A person installs it in Norn Studio by signing this restatement.</span>
        {admitted && studio !== null && <Button size="sm" variant="outline" onClick={() => bridge.openLink(new URL("/dashboards", studio).href)}>Open Studio</Button>}
      </CardFooter>
    </Card>
  )
}

interface Receipt {
  readonly outcome: string
  readonly objectType?: string
  readonly objectId?: string
  readonly version?: number
  readonly receipt?: { readonly receiptHash: string; readonly commitSeq: number; readonly recordedAtMs: number }
  readonly definitionsVersion?: number
  readonly previousVersion?: number
  readonly receiptSha256?: string
  readonly commitSeq?: number
  readonly plan?: Plan
}

/** A governed action's answer, or an installed definitions version: what changed, and its receipt. */
function ReceiptCard({ read, tool }: { readonly read: Read<Receipt>; readonly tool?: string }) {
  const document = read.document
  if (document.definitionsVersion !== undefined && document.plan !== undefined) {
    return <PlanCard plan={document.plan} title={document.outcome === "unchanged" ? "Already installed: nothing changed" : `Installed: version ${document.previousVersion} → ${document.definitionsVersion} · commit ${document.commitSeq} · receipt ${short(document.receiptSha256)}`} />
  }
  const { outcome, objectType, objectId, version, receipt } = document
  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>{outcome === "replayed" ? "Already done" : outcome === "pending_approval" || outcome === "held" ? "Waiting for approval" : "Accepted"}</CardTitle>
        <CardAction><Chip tone={outcome === "committed" || outcome === "replayed" ? "ok" : "warn"}>{outcome}</Chip></CardAction>
      </CardHeader>
      <CardContent className="text-body text-muted-foreground">
        <span className="font-mono text-meta text-foreground">{objectId ?? tool ?? objectType}</span>
        {version !== undefined && ` → version ${version}`}
        {receipt && <> · commit {receipt.commitSeq} · receipt <span className="font-mono text-meta" title={receipt.receiptHash}>{short(receipt.receiptHash)}</span></>}
      </CardContent>
    </Card>
  )
}

/** A refusal in the server's words: the law and the values it compared when it says them. */
function RefusalBody({ refusal }: { readonly refusal: Refusal }) {
  const reason = (refusal as { reason?: { law?: string; values?: Record<string, unknown> } | null }).reason
  const explanation = (refusal as { explanation?: string }).explanation
  return (
    <div className="space-y-1 text-body">
      {reason?.law && <div className="font-mono text-meta text-muted-foreground">{reason.law}</div>}
      {reason?.values && <div className="tabular-nums">{Object.entries(reason.values).map(([key, value], index) => <span key={key}>{index > 0 ? " · " : ""}{key} <b>{Array.isArray(value) ? value.map((item) => grouped(String(item))).join(" + ") : grouped(String(value))}</b></span>)}</div>}
      <div className="text-muted-foreground">{explanation ?? refusal.message}</div>
    </div>
  )
}

/** Refusals keep the server's words and code; nothing was changed. */
function Refused({ refusal, action }: { readonly refusal: Refusal; readonly action: boolean }) {
  const reason = (refusal as { reason?: { law?: string } | null }).reason
  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>{reason?.law ? "Refused by law" : "Refused"}</CardTitle>
        <CardAction><Chip tone="bad">{refusal.code}</Chip></CardAction>
      </CardHeader>
      <CardContent><RefusalBody refusal={refusal} /></CardContent>
      {action && <CardFooter className="text-meta text-muted-foreground">Nothing was changed. The agent asked; the workspace's laws and policy decided.</CardFooter>}
    </Card>
  )
}

/** The view for one delivery, chosen by the template the host loaded and the document's own shape. */
export function DeliveryView({ view, delivery, bridge, studio, tool }: { readonly view: View; readonly delivery: Delivery; readonly bridge: ToolHost; readonly studio: string | null; readonly tool?: string }) {
  const { input, result } = delivery
  if (result === undefined) return <p className="p-3 text-body text-muted-foreground">Asking Norn…</p>
  const read = exactDocument<unknown>(result)
  if (result.isError === true || read === null) {
    const refusal = asRefusal(read?.document ?? result.structuredContent)
    return refusal === null ? <p className="p-3 text-body text-muted-foreground">Norn sent nothing to show.</p> : <Refused refusal={refusal} action={view === "receipt"} />
  }
  const document = read.document as Record<string, unknown>
  if (view === "answer" && typeof document.lineageRef === "string" && typeof document.rows === "object") return <AnswerCard read={read as Read<Answer>} input={input} bridge={bridge} />
  if (view === "lineage" && Array.isArray(document.contributions)) return <TraceCard read={read as Read<Trace>} input={input} />
  if (view === "lineage" && typeof document.fieldProvenance === "object") return <ObjectLineage read={read as Read<SourceLineage>} />
  if (view === "dashboard" && typeof document.admitted === "boolean") return <ProposalCard read={read as Read<Admission>} input={input} studio={studio} bridge={bridge} />
  if (view === "receipt" && typeof document.outcome === "string") return <ReceiptCard read={read as Read<Receipt>} tool={tool} />
  if (view === "records" && Array.isArray(document.records)) return <RecordsCard read={read as Read<RecordsPage>} />
  if (view === "findings" && (Array.isArray(document.findings) || Array.isArray(document.items))) return <FindingsCard read={read as Read<Findings>} />
  if (view === "inbox" && Array.isArray(document.items) && Array.isArray(document.sections)) return <InboxCard read={read as Read<Inbox>} />
  if (view === "plan" && typeof document.baseVersion === "number") return <PlanCard plan={document as unknown as Plan} />
  return <p className="p-3 text-body text-muted-foreground">Norn answered with a document this view does not draw.</p>
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

