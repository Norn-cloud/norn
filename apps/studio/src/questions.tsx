import { useQueries, useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { Fragment, useEffect, useMemo, useRef, useState } from "react"
import { CalendarRange, ChartLine, Clock, Database, ListTree, Plus, Rows3, Sigma, X } from "lucide-react"
import { FactChip } from "@/components/system/chip"
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from "recharts"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { KpiTile, StatStrip } from "@/components/system/kpi-tile"
import { PageHeader } from "@/components/panel"
import { EmptyState, Panel, Well } from "@/components/system/panel"
import { LoadState, WorkspaceUnavailable } from "@/components/load-state"
import { useView } from "@/lib/view"
import { TableCard } from "@/components/system/table-card"
import { Table, TableBody, TableCell, TableFooter, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { buckets, contribution, declarations, fraction, shown, type Bucket } from "./lib/answer.ts"
import { conceptTitle } from "./lib/catalog.ts"
import { chartFrame } from "./lib/chart.ts"
import { FRESHNESS, day, period, scope } from "./lib/format.ts"
import { amountTexts, liveAnswer, liveTrace, type LiveRead, type Refusal, type Trace } from "./lib/live.ts"
import type { Read } from "./lib/exact.ts"
import { transport, type Metric, type ObjectType, type Question, type QuestionAnswer } from "./lib/transport.ts"
import { CopyId } from "@/components/copy-id"
import { RestatementCard } from "./restatement.tsx"

import { AnswerView, AskedAnswer, RefusedNote } from "./answer-view.tsx"
import { useStudioScope, type StudioScope } from "./lib/definitions.ts"
import { useMode } from "./lib/live-transport.ts"
import { metricHeading, metricQuestion, readMetrics, recentWindow, timeLabel, windowOf, type CellMetric, type Condition, type MetricSummary } from "./lib/metrics.ts"
import { figure, unitGroups, type UnitGroup } from "./lib/answer-anatomy.ts"
import { evaluateQuestion } from "./lib/evaluate.ts"
import { readDocument } from "./lib/exact.ts"
import { ProfileTimedOut, profileQueryKey, readProfile } from "./lib/profile.ts"
import { useLiveRefresh } from "./lib/live-reads.ts"
export { AnswerView, LiveRecords, RefusedNote, type FetchTrace } from "./answer-view.tsx"

/** One metric the cell derives, asked per day or week over every date its records carry. */
/** Whether an element has come near the viewport (once seen, stays seen); without an observer (a test render) it is seen. */
function useSeen<T extends Element>() {
  const ref = useRef<T>(null)
  const [seen, setSeen] = useState(() => typeof IntersectionObserver === "undefined")
  useEffect(() => {
    if (seen || ref.current === null) return
    const observer = new IntersectionObserver((entries) => { if (entries.some((entry) => entry.isIntersecting)) { setSeen(true); observer.disconnect() } }, { rootMargin: "300px" })
    observer.observe(ref.current)
    return () => observer.disconnect()
  }, [seen])
  return [ref, seen] as const
}

function MetricCard({ metric, turn, onSettled, scope: boundScope }: {
  readonly metric: MetricSummary
  /** Whether every question above this one has its answer: the cell answers one at a time, top first. */
  readonly turn: boolean
  /** Called once this question is settled (answered, refused, or nothing to ask). */
  readonly onSettled: () => void
  readonly scope: StudioScope | null
}) {
  const { view } = useView()
  // A question asks the cell only once it is near view and the ones above it have their answers.
  const [ref, seen] = useSeen<HTMLElement>()
  const active = seen && turn
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const [conditions, setConditions] = useState<readonly Condition[]>([])
  // The unit group whose weekly answer is open: chosen from the groups the cell returned, never by default.
  const [picked, setPicked] = useState<UnitGroup | null>(null)
  const profile = useQuery({ queryKey: profileQueryKey(metric.objectType, boundScope), queryFn: () => readProfile(metric.objectType, boundScope), retry: false, enabled: active })
  const typeNamed = (name: string) => catalog.data?.document.flatMap((artifact) => artifact.kind === "entity" && "objectType" in artifact.payload && artifact.payload.objectType.name === name ? [artifact.payload.objectType] : [])[0]
  // A time read through links (a posting's time is its event's) is profiled on the type the links reach, as the
  // cell declares them; the window then spans every date that type carries.
  const timeType = (metric.timeLinks ?? []).reduce<string | undefined>((type, link) => type === undefined ? undefined
    : (typeNamed(type) as { readonly links?: readonly { readonly id: string; readonly target: string }[] } | undefined)?.links?.find(({ id }) => id === link)?.target, metric.objectType)
  const timeProfile = useQuery({ queryKey: profileQueryKey(timeType ?? "", boundScope), queryFn: () => readProfile(timeType!, boundScope), retry: false, enabled: active && timeType !== undefined && timeType !== metric.objectType })
  const timeDocument = timeType === metric.objectType ? profile.data?.document : timeProfile.data?.document
  const profiledWindow = timeDocument && metric.timeProperty !== null ? windowOf(timeDocument, metric.timeProperty) : undefined
  // The profile sent no range for the time property (a hosted cell may withhold it): ask the latest 52 weeks, and say so.
  // A profile that did not settle within its budget is not waited for either: the latest 52 weeks are asked, and the chip says why.
  const late = profile.error instanceof ProfileTimedOut || timeProfile.error instanceof ProfileTimedOut
  const assumed = profiledWindow === undefined && metric.timeProperty !== null && (late || ((profile.data?.document.rows ?? 0) > 0 && (timeDocument !== undefined || timeProfile.isError)))
  const window = profiledWindow ?? (assumed ? recentWindow(Date.now()) : undefined)
  // The answer is read with the type's own declaration, as the cell admitted it.
  const objectType = typeNamed(metric.objectType)
  const declared = { metrics: readDocument<readonly Metric[]>("[]", "live"), objectType: readDocument<ObjectType>(JSON.stringify(objectType ?? { name: metric.objectType, properties: [] }), "live") }
  const what = metricHeading(metric, window?.grain, view)
  const sources = metric.unitPartition ?? []
  const change = (next: readonly Condition[]) => { setConditions(next); setPicked(null) }
  // Nothing to ask (no records, a refused profile, no window): this question is settled and the next may go.
  const nothingToAsk = (profile.isError && !late) || profile.data?.document.rows === 0 || (window === undefined && (profile.data !== undefined || late))
  useEffect(() => { if (nothingToAsk) onSettled() }, [nothingToAsk, onSettled])
  return (
    // One question, one section: its title, its controls, then the answer's own surfaces (no card around cards).
    <section ref={ref} data-slot="panel" className="min-w-0 space-y-4">
      <header className="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-1">
        <h2 className="text-heading font-normal tracking-[-0.025em]">{what}{metric.timeProperty !== null && <span className="text-muted-foreground"> · by {timeLabel(metric)}</span>}</h2>
        {view === "technical" && <span className="font-mono text-meta text-muted-foreground" title={metric.name}>{metric.kind}</span>}
      </header>
      <div className="space-y-4">
        {profile.isPending && (!seen || !turn
          ? <p className="text-body text-muted-foreground">{!seen ? "Waits until you scroll to it." : "Waits for the questions above it."}</p>
          : <LoadState status="pending" skeleton="panel" loadingLabel={`Reading ${conceptTitle(metric.objectType).toLowerCase()} records`} onRetry={() => void profile.refetch()}>{() => null}</LoadState>)}
        {profile.isError && !late && <LoadState status="error" error={profile.error} isFetching={profile.isFetching} onRetry={() => void profile.refetch()}>{() => null}</LoadState>}
        {profile.data && profile.data.document.rows === 0 && (
          <EmptyState icon={Sigma} title={profile.data.document.withheldRows + profile.data.document.quarantinedRows === 0 ? "No records yet" : "No records to add up"}
            next={profile.data.document.withheldRows + profile.data.document.quarantinedRows === 0 && <Button asChild size="sm" variant="outline"><Link to="/sources"><Database />Load data or connect a source</Link></Button>}>
            {profile.data.document.withheldRows + profile.data.document.quarantinedRows === 0
              ? <>Nothing has been loaded for {conceptTitle(metric.objectType).toLowerCase()}, so there is nothing to add up.</>
              : <>{[profile.data.document.withheldRows > 0 && `${profile.data.document.withheldRows} withheld from you`, profile.data.document.quarantinedRows > 0 && `${profile.data.document.quarantinedRows} held in quarantine`].filter(Boolean).join(", ")}; none you can read is left to add up.</>}
            <span className="block pt-1 text-meta">Checked with the cell at {new Date(profile.dataUpdatedAt).toLocaleTimeString(undefined, { timeStyle: "short" })}.</span>
          </EmptyState>
        )}
        {profile.data && profile.data.document.rows > 0 && window === undefined && <p className="text-body text-muted-foreground">No {metric.timeProperty?.replaceAll("_", " ") ?? "date"} values to ask over yet.</p>}
        {window && <>
          <div className="flex flex-wrap items-start gap-2">
            <FactChip icon={CalendarRange} label={`Per ${window.grain}`} className="h-8" title={assumed ? (late ? "The cell's profile of this type didn't arrive in time, so Studio asks the latest 52 weeks." : "The cell sent no range for this date, so Studio asks the latest 52 weeks.") : "Every date its records carry."}>{assumed ? "Latest 52 weeks" : `${day(window.start)} – ${day(Date.parse(window.end) - 86_400_000)}`}</FactChip>
            <div className="min-w-0 flex-1"><Conditions fields={(objectType?.properties ?? []).map(({ id }) => id).filter((id) => id !== metric.expression && id !== metric.timeProperty)} conditions={conditions} onChange={change} /></div>
          </div>
          {sources.length === 0
            ? <AskedAnswer question={metricQuestion(metric, window, conditions) as Question} title={what} declared={declared} heading="none" onSettled={onSettled} scope={boundScope} />
            : <UnitGroups asked={metricQuestion(metric, window, conditions) as Question} grain={window.grain} sources={sources} title={what} declared={declared} picked={picked} onPick={setPicked} onSettled={onSettled} scope={boundScope} />}
        </>}
      </div>
    </section>
  )
}

/**
 * A metric whose values are in several units, asked at once: one exact total per unit group the cell
 * returns (it groups by the unit sources itself), labelled by its unit, most records first; the
 * question's own conditions narrow it. A group opens the question as asked, held to that group.
 */
export function UnitGroups({ asked, grain, sources, title, declared, picked, onPick, onSettled, scope }: {
  /** The question as asked: its window, conditions and grouping; the unit groups are read over the same window. */
  readonly asked: Question
  /** The period the opened group's answer is read by, in words ("week"); the question's own when it groups by a time. */
  readonly grain: string
  /** The properties the cell names as the metric's units (its unitPartition). */
  readonly sources: readonly string[]
  readonly title: string
  readonly declared: Pick<QuestionAnswer, "metrics" | "objectType">
  readonly picked: UnitGroup | null
  readonly onPick: (group: UnitGroup | null) => void
  readonly onSettled: () => void
  readonly scope?: StudioScope | null
}) {
  const scopeKey = scope ? `${scope.orgId}:${scope.environmentId}` : ""
  const totals: Question = { ...asked, groupBy: sources.map((property) => ({ path: { property } })) }
  const run = useQuery({ queryKey: ["unit-groups", scopeKey, JSON.stringify(totals)], queryFn: () => evaluateQuestion(totals, undefined, scope), retry: false, staleTime: 30_000 })
  const result = run.data
  // Settled once the totals are back, unless one group opens its answer per period (that answer settles it).
  const single = result?.kind === "answer" && unitGroups(sources, buckets({ question: totals, metrics: [], objectType: declared.objectType.document }, { answer: result.read.document, numbers: result.read.numbers, at: "" })).length === 1
  useEffect(() => { if (!run.isPending && !single) onSettled() }, [run.isPending, single, onSettled])
  if (run.isPending) return <LoadState status="pending" skeleton="panel" loadingLabel="Reading metric totals" onRetry={() => void run.refetch()}>{() => null}</LoadState>
  if (run.isError || result === undefined) return <LoadState status="error" error={run.error ?? new Error("The cell did not answer.")} isFetching={run.isFetching} onRetry={() => void run.refetch()}>{() => null}</LoadState>
  if (result.kind === "refused") return <RefusedNote refusal={result.refusal} />
  if (result.kind !== "answer") return <p role="status" className="text-body text-muted-foreground">{result.kind === "not-a-question" ? result.findings.map(({ message }) => message).join(" ") : result.kind === "too-large" || result.kind === "bad-request" ? result.message : "The cell did not answer."}</p>
  const rows = buckets({ question: totals, metrics: [], objectType: declared.objectType.document }, { answer: result.read.document, numbers: result.read.numbers, at: "" })
  const groups = unitGroups(sources, rows)
  // One group is not a choice: its answer per period opens with it.
  const open = groups.length === 1 ? groups[0] ?? null : picked
  const weekly = open && ({ ...asked, filter: [...(asked.filter ?? []).filter(({ path }) => !sources.includes(path.property)), ...sources.map((property) => ({ path: { property }, operator: "eq", value: open.values[property] ?? null }))] } as Question)
  return (
    <div className="space-y-4">
      {groups.length === 0 ? <EmptyState icon={Sigma}>No records in this window match.</EmptyState> : groups.length === 1 ? null : (
        <div className="space-y-2">
          <p className={groups.length === 1 ? "sr-only" : "text-meta text-muted-foreground"}>{groups.length === 1 ? `In one unit, ${period(totals.window)}.` : `In ${groups.length} units, ${period(totals.window)}; most records first. Choose one to see it per ${grain}.`}</p>
          <TableCard><Table>
            <TableHeader><TableRow><TableHead>Unit</TableHead><TableHead className="text-end">{title}</TableHead><TableHead className="text-end">Records</TableHead></TableRow></TableHeader>
            <TableBody>
              {groups.map((group) => (
                <TableRow key={group.key} data-state={open?.key === group.key ? "selected" : undefined} className="cursor-pointer" onClick={() => onPick(open?.key === group.key ? null : group)}>
                  <TableCell><Button type="button" variant="link" size="sm" className="h-auto px-0 text-foreground" aria-pressed={open?.key === group.key} onClick={(event) => { event.stopPropagation(); onPick(open?.key === group.key ? null : group) }}>{group.unit ?? "No unit"}</Button></TableCell>
                  <TableCell className="text-end tabular-nums">{figure(group.bucket, group.unit)}</TableCell>
                  <TableCell className="text-end tabular-nums">{group.bucket.members.length.toLocaleString("en")}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table></TableCard>
        </div>
      )}
      {/* One group: its total for the window is the first number on screen while its answer per period comes. */}
      {open && weekly && <AskedAnswer question={weekly} title={title} unit={open.unit} declared={declared} heading="none" onSettled={onSettled} scope={scope}
        placeholder={groups.length === 1 ? <div data-slot="answer-total" className="rounded-2xl bg-card p-5 shadow-[var(--elevation-1)] ring-1 ring-foreground/5 sm:p-7">
          <div className="text-meta text-muted-foreground">Total, {period(totals.window)}</div>
          <div className="pt-2 text-figure leading-none tracking-[-0.045em] tabular-nums">{figure(open.bucket, open.unit)}</div>
          <div className="pt-3 text-meta text-muted-foreground">{open.bucket.members.length.toLocaleString("en")} records · reading each {grain}…</div>
        </div> : undefined}
        total={{ label: `Total, ${period(totals.window)}`, value: figure(open.bucket, open.unit) }} />}
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
        <div className="grid grid-cols-[auto_minmax(0,1fr)_auto_minmax(0,1fr)_auto_auto] items-center gap-x-2 gap-y-2 text-body">
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
  const scope = useStudioScope()
  const boundScope = scope.data ?? null
  const scopeKey = boundScope ? `${boundScope.orgId}:${boundScope.environmentId}` : ""
  const metrics = useQuery({ queryKey: ["metrics", scopeKey], queryFn: ({ signal }) => readMetrics({ signal, scope: boundScope }), enabled: scope.isSuccess, retry: false })
  if (scope.isError) return <p role="alert" className="text-destructive">{scope.error.message}</p>
  return (
    <LoadState
      status={scope.isPending ? "pending" : metrics.status}
      data={metrics.data}
      error={metrics.error}
      isFetching={metrics.isFetching}
      onRetry={() => void metrics.refetch()}
      skeleton="cards"
      loadingLabel="Reading metrics"
      notServed={{ icon: ChartLine, title: "Not served" }}
      isEmpty={(data) => data.document.metrics.length === 0 && data.document.combined.length === 0}
      empty={{
        icon: ChartLine,
        title: "No metrics yet",
        description: "No admitted type declares an additive measure yet, so there is nothing to add up.",
        next: <Button asChild size="sm" variant="outline"><Link to="/sources"><Database />Open Sources</Link></Button>,
      }}
    >
      {(data) => {
        const list = data.document.metrics
        const types = [...new Set(list.map(({ objectType }) => objectType))]
        return <>
          <TypeSections types={types} list={list} scope={boundScope} />
          <CombinedMetrics list={data.document.combined} />
        </>
      }}
    </LoadState>
  )
}

/** The metrics the cell lists over no single type (they combine others): named, never asked as if they had one. */
function CombinedMetrics({ list }: { readonly list: readonly CellMetric[] }) {
  const { view } = useView()
  if (list.length === 0) return null
  return (
    <Panel icon={Sigma} label="Combined metrics">
      <Well className="p-1.5">
        <ul className="divide-y divide-border/70" data-combined-metrics>
          {list.map((metric) => (
            <li key={metric.name} className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 px-2.5 py-2 text-body">
              <span>{metricHeading(metric, "period", view)}</span>
              {view === "technical" && <span className="font-mono text-meta text-muted-foreground">{metric.name}</span>}
            </li>
          ))}
        </ul>
        <p className="px-2.5 pt-1 pb-1.5 text-meta text-muted-foreground">Each combines other metrics, so it is asked from a question or a dashboard tile, not on its own here.</p>
      </Well>
    </Panel>
  )
}

/**
 * The metrics by type. A type the cell holds no readable records for does not take a card per
 * metric: its metrics are listed together, compactly, after the types that have records.
 */
function TypeSections({ types, list, scope: boundScope }: { readonly types: readonly string[]; readonly list: readonly MetricSummary[]; readonly scope: StudioScope | null }) {
  const { view } = useView()
  // The profiles each card reads as it comes into view (one analytics read at a time): read here, never asked here.
  const profiles = useQueries({ queries: types.map((type) => ({ queryKey: profileQueryKey(type, boundScope), queryFn: () => readProfile(type, boundScope), retry: false, enabled: false })) })
  const empty = (index: number) => profiles[index]?.data?.document.rows === 0
  const held = types.filter((_, index) => !empty(index))
  const without = types.filter((_, index) => empty(index))
  // The cell answers one question at a time, top first: a question's turn comes when every shown question above it is settled.
  const [settled, setSettled] = useState<ReadonlySet<string>>(new Set())
  const shown = held.flatMap((type) => list.filter(({ objectType }) => objectType === type))
  const turnOf = (name: string) => shown.slice(0, shown.findIndex((metric) => metric.name === name)).every((metric) => settled.has(metric.name))
  const settle = useMemo(() => new Map(list.map(({ name }) => [name, () => setSettled((current) => current.has(name) ? current : new Set([...current, name]))])), [list])
  return <>
    {held.map((type) => (
      <div key={type} className="space-y-3">
        <h2 className="text-heading font-medium"><Link to="/objects/$type" params={{ type }} className="underline-offset-4 hover:underline">{conceptTitle(type)}</Link></h2>
        <div className="grid gap-3">{list.filter(({ objectType }) => objectType === type).map((metric) => <MetricCard key={metric.name} metric={metric} turn={turnOf(metric.name)} onSettled={settle.get(metric.name)!} scope={boundScope} />)}</div>
      </div>
    ))}
    {without.length > 0 && (
      <Panel icon={Sigma} label="No records yet">
        <Well className="p-1.5">
          <ul className="divide-y divide-border/70">
            {without.map((type) => (
              <li key={type} className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 px-2.5 py-2 text-body">
                <Link to="/objects/$type" params={{ type }} className="underline-offset-4 hover:underline">{conceptTitle(type)}</Link>
                <span className="min-w-0 text-meta text-muted-foreground">{list.filter(({ objectType }) => objectType === type).map((metric) => metricHeading(metric, "period", view)).join(" · ")}</span>
                {(() => {
                  const profile = profiles[types.indexOf(type)]?.data?.document
                  const hidden = (profile?.withheldRows ?? 0) + (profile?.quarantinedRows ?? 0)
                  return hidden > 0 && <span className="text-meta text-muted-foreground">{[profile && profile.withheldRows > 0 && `${profile.withheldRows} withheld from you`, profile && profile.quarantinedRows > 0 && `${profile.quarantinedRows} held in quarantine`].filter(Boolean).join(", ")}</span>
                })()}
              </li>
            ))}
          </ul>
          <p className="px-2.5 pt-1 pb-1.5 text-meta text-muted-foreground">Nothing readable is loaded for these yet, so there is nothing to add up.</p>
        </Well>
      </Panel>
    )}
  </>
}

function QuestionCard({ read: recorded, heading = "h2" }: { readonly read: QuestionAnswer; readonly heading?: "h1" | "h2" }) {
  // The live answer when the host serves one; otherwise the recording, labelled as such.
  // A new commit that changes the answer, or a policy change, reaches the card when the cell sends it (UI-LIVE-1).
  const live = useQuery({
    queryKey: ["live-answer", recorded.question.document.name],
    queryFn: () => liveAnswer(recorded.question.document.name),
    retry: false,
    staleTime: 60_000,
  })
  useLiveRefresh(`/v1/questions/${encodeURIComponent(recorded.question.document.name)}/answer`, ["live-answer", recorded.question.document.name])
  const read: QuestionAnswer = live.data?.kind === "read" ? { ...recorded, answer: live.data.read } : recorded
  return <AnswerView read={read} heading={heading} link refusal={live.data?.kind === "refused" ? live.data.refusal : undefined} />
}


export { useQuestionsMenu } from "./lib/nav-menus.ts"

/** One question as a record page; the submenu lists the others by the object type they count. */
export function QuestionPage() {
  const { name } = useParams({ from: "/metrics/$name" })
  const board = useQuery({ queryKey: ["question-board"], queryFn: transport.questionBoard })
  return (
    <LoadState
      status={board.status}
      data={board.data}
      error={board.error}
      isFetching={board.isFetching}
      onRetry={() => void board.refetch()}
      skeleton="detail"
      loadingLabel="Reading metric"
    >
      {(rows) => {
        const read = rows.find(({ question }) => question.document.name === name)
        return read
          ? <QuestionCard read={read} heading="h1" />
          : <EmptyState icon={ChartLine} title="Metric not found" next={<Button asChild size="sm" variant="outline"><Link to="/metrics">All metrics</Link></Button>}>No metric named {name} is on the board.</EmptyState>
      }}
    </LoadState>
  )
}

export function QuestionsPage() {
  const mode = useMode()
  const live = mode.data?.kind === "live"
  const board = useQuery({ queryKey: ["question-board"], queryFn: transport.questionBoard, enabled: mode.data?.kind === "recorded" })
  return (
    <section className="space-y-6">
      <PageHeader title="Metrics" description="The numbers your records add up to, answered exactly by the cell. Every number opens to the records behind it."
        primary={<Button asChild><Link to="/metrics/new"><Plus />Explore a metric</Link></Button>}
        secondary={[{ label: "Build step by step", icon: ListTree, to: "/metrics/build" }]} />
      {mode.isPending && <LoadState status="pending" skeleton="cards" loadingLabel="Connecting to workspace" onRetry={() => void mode.refetch()}>{() => null}</LoadState>}
      {mode.isError && <WorkspaceUnavailable error={mode.error} />}
      {live ? <LiveQuestionsBoard /> : mode.data && (
        <LoadState
          status={board.status}
          data={board.data}
          error={board.error}
          isFetching={board.isFetching}
          onRetry={() => void board.refetch()}
          skeleton="cards"
          loadingLabel="Reading metrics"
          isEmpty={(rows) => rows.length === 0}
          empty={{
            icon: ChartLine,
            title: "No metrics yet",
            description: "Explore a metric or load records to see answers here.",
            next: <Button asChild size="sm" variant="outline"><Link to="/metrics/new"><Plus />Explore a metric</Link></Button>,
          }}
        >
          {(rows) => <>
            {rows.map((read) => <QuestionCard key={read.question.document.name} read={read} />)}
            <RestatementCard />
          </>}
        </LoadState>
      )}
    </section>
  )
}
