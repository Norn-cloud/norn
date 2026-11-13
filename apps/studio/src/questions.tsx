import { useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { Fragment, useState } from "react"
import { ChartLine, Clock, Plus, Rows3, Sigma, X } from "lucide-react"
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from "recharts"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { EmptyState, PageHeader, Panel, Well } from "@/components/panel"
import { useView } from "@/lib/view"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableFooter, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { buckets, contribution, declarations, fraction, shown, type Bucket } from "./lib/answer.ts"
import { conceptTitle } from "./lib/catalog.ts"
import { chartFrame } from "./lib/chart.ts"
import { FRESHNESS, day, headline, period, scope } from "./lib/format.ts"
import { amountTexts, liveAnswer, liveTrace, type LiveRead, type Refusal, type Trace } from "./lib/live.ts"
import type { Read } from "./lib/exact.ts"
import { transport, type Metric, type ObjectType, type Question, type QuestionAnswer } from "./lib/transport.ts"
import { CopyId } from "@/components/copy-id"
import { RestatementCard } from "./restatement.tsx"
import type { MenuEntry } from "./shell.tsx"

import { AnswerView, AskedAnswer, RefusedNote } from "./answer-view.tsx"
import { NotServed, useMode } from "./lib/live-transport.ts"
import { metricHeading, metricQuestion, readMetrics, windowOf, type Condition, type MetricSummary, type Window } from "./lib/metrics.ts"
import { figure, unitGroups, type UnitGroup } from "./lib/answer-anatomy.ts"
import { evaluateQuestion } from "./lib/evaluate.ts"
import { readDocument } from "./lib/exact.ts"
import { readProfile } from "./lib/profile.ts"
export { AnswerView, LiveRecords, RefusedNote, type FetchTrace } from "./answer-view.tsx"

/** One metric the cell derives, asked per day or week over every date its records carry. */
function MetricCard({ metric }: { readonly metric: MetricSummary }) {
  const { view } = useView()
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const [conditions, setConditions] = useState<readonly Condition[]>([])
  // The unit group whose weekly answer is open: chosen from the groups the cell returned, never by default.
  const [picked, setPicked] = useState<UnitGroup | null>(null)
  const profile = useQuery({ queryKey: ["profile", metric.objectType], queryFn: () => readProfile(metric.objectType), retry: false })
  const window = profile.data && metric.timeProperty !== null ? windowOf(profile.data.document, metric.timeProperty) : undefined
  // The answer is read with the type's own declaration, as the cell admitted it.
  const objectType = catalog.data?.document.flatMap((artifact) => artifact.kind === "entity" && "objectType" in artifact.payload && artifact.payload.objectType.name === metric.objectType ? [artifact.payload.objectType] : [])[0]
  const declared = { metrics: readDocument<readonly Metric[]>("[]", "live"), objectType: readDocument<ObjectType>(JSON.stringify(objectType ?? { name: metric.objectType, properties: [] }), "live") }
  const what = metricHeading(metric, window?.grain)
  const sources = metric.unitPartition ?? []
  const change = (next: readonly Condition[]) => { setConditions(next); setPicked(null) }
  return (
    <Panel icon={Sigma} label={<span>{what}{metric.timeProperty !== null && <span className="text-muted-foreground"> · by {metric.timeProperty.replaceAll("_", " ")}</span>}</span>}
      action={view === "technical" ? <span className="font-mono text-xs text-muted-foreground" title={metric.name}>{metric.kind}</span> : undefined}>
      <Well className="space-y-4 p-4">
        {profile.isPending && <p className="text-sm text-muted-foreground">Reading {conceptTitle(metric.objectType).toLowerCase()} records…</p>}
        {profile.isError && <p className="text-sm text-muted-foreground">{profile.error.message || "The cell did not say what it holds for this type."}</p>}
        {profile.data && profile.data.document.rows === 0 && (
          <EmptyState icon={Sigma} title={profile.data.document.withheldRows + profile.data.document.quarantinedRows === 0 ? "No records yet" : "No records to add up"}>
            {profile.data.document.withheldRows + profile.data.document.quarantinedRows === 0
              ? <>Nothing has been loaded for {conceptTitle(metric.objectType).toLowerCase()}, so there is nothing to add up.</>
              : <>{[profile.data.document.withheldRows > 0 && `${profile.data.document.withheldRows} withheld from you`, profile.data.document.quarantinedRows > 0 && `${profile.data.document.quarantinedRows} held in quarantine`].filter(Boolean).join(", ")}; none you can read is left to add up.</>}
            <span className="block pt-1 text-meta">Checked with the cell at {new Date(profile.dataUpdatedAt).toLocaleTimeString(undefined, { timeStyle: "short" })}.</span>
          </EmptyState>
        )}
        {profile.data && profile.data.document.rows > 0 && window === undefined && <p className="text-sm text-muted-foreground">No {metric.timeProperty?.replaceAll("_", " ") ?? "date"} values to ask over yet.</p>}
        {window && <>
          <p className="text-meta text-muted-foreground">Per {window.grain}, {day(window.start)} – {day(Date.parse(window.end) - 86_400_000)}: every date its records carry.</p>
          <Conditions fields={(objectType?.properties ?? []).map(({ id }) => id).filter((id) => id !== metric.expression && id !== metric.timeProperty)} conditions={conditions} onChange={change} />
          {sources.length === 0
            ? <AskedAnswer question={metricQuestion(metric, window, conditions) as Question} title={what} declared={declared} />
            : <UnitGroups metric={metric} window={window} conditions={conditions} sources={sources} title={what} declared={declared} picked={picked} onPick={setPicked} />}
        </>}
      </Well>
    </Panel>
  )
}

/**
 * A metric whose values are in several units, asked at once: one exact total per unit group the cell
 * returns (it groups by the unit sources itself), labelled by its unit, most records first; the
 * conditions narrow it. A group opens its answer per period.
 */
function UnitGroups({ metric, window, conditions, sources, title, declared, picked, onPick }: {
  readonly metric: MetricSummary
  readonly window: Window
  readonly conditions: readonly Condition[]
  readonly sources: readonly string[]
  readonly title: string
  readonly declared: Pick<QuestionAnswer, "metrics" | "objectType">
  readonly picked: UnitGroup | null
  readonly onPick: (group: UnitGroup | null) => void
}) {
  const asked = metricQuestion(metric, window, conditions) as Question
  const totals: Question = { ...asked, groupBy: sources.map((property) => ({ path: { property } })) }
  const run = useQuery({ queryKey: ["unit-groups", JSON.stringify(totals)], queryFn: () => evaluateQuestion(totals), retry: false, staleTime: 30_000 })
  const result = run.data
  if (run.isPending) return <p className="text-sm text-muted-foreground">Asking the cell…</p>
  if (run.isError || result === undefined) return <p role="alert" className="text-sm text-destructive">Not run: {run.error instanceof Error ? run.error.message : "no answer"}</p>
  if (result.kind === "refused") return <RefusedNote refusal={result.refusal} />
  if (result.kind !== "answer") return <p role="status" className="text-sm text-muted-foreground">{result.kind === "not-a-question" ? result.findings.map(({ message }) => message).join(" ") : result.kind === "too-large" || result.kind === "bad-request" ? result.message : "The cell did not answer."}</p>
  const rows = buckets({ question: totals, metrics: [], objectType: declared.objectType.document }, { answer: result.read.document, numbers: result.read.numbers, at: "" })
  const groups = unitGroups(sources, rows)
  // One group is not a choice: its answer per period opens with it.
  const open = groups.length === 1 ? groups[0] ?? null : picked
  const weekly = open && ({ ...asked, filter: [...(asked.filter ?? []).filter(({ path }) => !sources.includes(path.property)), ...sources.map((property) => ({ path: { property }, operator: "eq", value: open.values[property] ?? null }))] } as Question)
  return (
    <div className="space-y-4">
      {groups.length === 0 ? <EmptyState icon={Sigma}>No records in this window match.</EmptyState> : (
        <div className="space-y-2">
          <p className="text-meta text-muted-foreground">{groups.length === 1 ? `In one unit, ${period(totals.window)}.` : `In ${groups.length} units, ${period(totals.window)}; most records first. Choose one to see it per ${window.grain}.`}</p>
          <TableCard><Table>
            <TableHeader><TableRow><TableHead>Unit</TableHead><TableHead className="text-right">{title}</TableHead><TableHead className="text-right">Records</TableHead></TableRow></TableHeader>
            <TableBody>
              {groups.map((group) => (
                <TableRow key={group.key} data-state={open?.key === group.key ? "selected" : undefined} className="cursor-pointer" onClick={() => onPick(open?.key === group.key ? null : group)}>
                  <TableCell><button type="button" aria-pressed={open?.key === group.key} className="rounded-sm font-medium underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none" onClick={(event) => { event.stopPropagation(); onPick(open?.key === group.key ? null : group) }}>{group.unit ?? "No unit"}</button></TableCell>
                  <TableCell className="text-right font-medium tabular-nums">{figure(group.bucket, group.unit)}</TableCell>
                  <TableCell className="text-right tabular-nums">{group.bucket.members.length.toLocaleString("en")}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table></TableCard>
        </div>
      )}
      {open && weekly && <AskedAnswer question={weekly} title={title} unit={open.unit} declared={declared} />}
    </div>
  )
}

/**
 * "Only where" conditions on a metric's question: a field equal to a value, or empty. The cell
 * checks the question as asked; a sum it will not add up across (mixed units, say) comes back as
 * its refusal, in its own words.
 */
function Conditions({ fields, conditions, onChange }: { readonly fields: readonly string[]; readonly conditions: readonly Condition[]; readonly onChange: (next: readonly Condition[]) => void }) {
  const set = (index: number, next: Partial<Condition>) => onChange(conditions.map((condition, at) => at === index ? { ...condition, ...next } : condition))
  return (
    <div className="space-y-2">
      {conditions.length > 0 && (
        // One row per condition on a shared grid, so fields, values and the remove control line up.
        <div className="grid grid-cols-[auto_minmax(0,1fr)_auto_minmax(0,1fr)_auto_auto] items-center gap-x-2 gap-y-2 text-sm">
          {conditions.map((condition, index) => (
            <Fragment key={index}>
              <span className="text-muted-foreground">{index === 0 ? "Only where" : "and"}</span>
              <select aria-label={`Field of condition ${index + 1}`} className="h-8 min-w-0 rounded-md border bg-background px-2" value={condition.property} onChange={(event) => set(index, { property: event.target.value })}>
                {fields.map((field) => <option key={field} value={field}>{field.replaceAll("_", " ")}</option>)}
              </select>
              <span className="text-muted-foreground">is</span>
              {condition.value === null
                ? <span className="flex h-8 items-center rounded-md border border-dashed px-2 text-muted-foreground">empty</span>
                : <input aria-label={`Value of condition ${index + 1}`} className="h-8 min-w-0 rounded-md border bg-background px-2" value={condition.value} onChange={(event) => set(index, { value: event.target.value })} />}
              <label className="flex items-center gap-1.5 text-muted-foreground"><input type="checkbox" checked={condition.value === null} onChange={(event) => set(index, { value: event.target.checked ? null : "" })} />empty</label>
              <Button type="button" variant="ghost" size="icon" className="size-8 text-muted-foreground" aria-label={`Remove condition ${index + 1}`} title="Remove" onClick={() => onChange(conditions.filter((_, at) => at !== index))}><X /></Button>
            </Fragment>
          ))}
        </div>
      )}
      {fields.length > 0 && <Button type="button" variant="outline" size="sm" onClick={() => onChange([...conditions, { property: fields[0] ?? "", value: "" }])}><Plus />Only where…</Button>}
    </div>
  )
}

/** The questions a connected cell can answer: the metrics it derives from its admitted types, grouped by type. */
function LiveQuestionsBoard() {
  const metrics = useQuery({ queryKey: ["metrics"], queryFn: readMetrics, retry: false })
  if (metrics.isPending) return <p className="text-muted-foreground">Reading what the cell can answer…</p>
  if (metrics.isError) return metrics.error instanceof NotServed
    ? <EmptyState icon={ChartLine} title="Not served">{metrics.error.message}</EmptyState>
    : <p role="alert" className="text-destructive">{metrics.error.message}</p>
  const list = metrics.data.document.metrics
  if (list.length === 0) return <EmptyState icon={ChartLine}>No admitted type declares an additive measure yet, so there is nothing to add up.</EmptyState>
  const types = [...new Set(list.map(({ objectType }) => objectType))]
  return <>{types.map((type) => (
    <div key={type} className="space-y-3">
      <h2 className="text-section font-medium"><Link to="/objects/$type" params={{ type }} className="underline-offset-4 hover:underline">{conceptTitle(type)}</Link></h2>
      <div className="grid gap-3">{list.filter(({ objectType }) => objectType === type).map((metric) => <MetricCard key={metric.name} metric={metric} />)}</div>
    </div>
  ))}</>
}

function QuestionCard({ read: recorded, heading = "h2" }: { readonly read: QuestionAnswer; readonly heading?: "h1" | "h2" }) {
  // The live answer when the host serves one; otherwise the recording, labelled as such.
  // A refused or unavailable live read is asked again every 30 s, so a policy change or a recovered host shows without a reload.
  const live = useQuery({
    queryKey: ["live-answer", recorded.question.document.name],
    queryFn: () => liveAnswer(recorded.question.document.name),
    retry: false,
    staleTime: 60_000,
    refetchInterval: (query) => query.state.data?.kind === "read" ? false : 30_000,
  })
  const read: QuestionAnswer = live.data?.kind === "read" ? { ...recorded, answer: live.data.read } : recorded
  return <AnswerView read={read} heading={heading} link refusal={live.data?.kind === "refused" ? live.data.refusal : undefined} />
}


/** The questions on the board, for the submenu island. */
export function useQuestionsMenu(): readonly MenuEntry[] | undefined {
  const board = useQuery({ queryKey: ["question-board"], queryFn: transport.questionBoard })
  return board.data && [
    ...board.data.map((read) => ({ href: `/questions/${read.question.document.name}`, label: headline(read.question.document), group: subject(read) })),
    { href: "#corrections", label: "Corrections", group: "Corrections" },
  ]
}

/** The object type a question counts, which is how the list groups questions. */
const subject = ({ objectType }: QuestionAnswer) => conceptTitle(objectType.document.name)

/** One question as a record page; the submenu lists the others by the object type they count. */
export function QuestionPage() {
  const { name } = useParams({ from: "/questions/$name" })
  const board = useQuery({ queryKey: ["question-board"], queryFn: transport.questionBoard })
  const read = board.data?.find(({ question }) => question.document.name === name)
  return (
    <>
      {board.isPending && <p className="text-muted-foreground">Reading answers…</p>}
      {board.isError && <p className="text-destructive">{board.error.message}</p>}
      {board.data && (read ? <QuestionCard read={read} heading="h1" /> : <EmptyState icon={ChartLine}>No question named {name} is on the board.</EmptyState>)}
    </>
  )
}

export function QuestionsPage() {
  const mode = useMode()
  const live = mode.data?.kind === "live"
  const board = useQuery({ queryKey: ["question-board"], queryFn: transport.questionBoard, enabled: mode.data?.kind === "recorded" })
  return (
    <section className="space-y-6">
      <PageHeader title="Questions" description="Business questions answered exactly by the server. Every number opens to the records it was added up from."
        actions={<><Button asChild variant="outline" size="sm"><Link to="/questions/new"><Plus />New question (form)</Link></Button><Button asChild size="sm"><Link to="/questions/build"><Plus />Build a question</Link></Button></>} />
      {mode.isError && <p className="text-destructive">{mode.error.message}</p>}
      {live ? <LiveQuestionsBoard /> : mode.data && <>
        {board.isPending && <p className="text-muted-foreground">Reading answers…</p>}
        {board.isError && <p className="text-destructive">{board.error.message}</p>}
        {board.data?.map((read) => <QuestionCard key={read.question.document.name} read={read} />)}
        <RestatementCard />
      </>}
    </section>
  )
}
