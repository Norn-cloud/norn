import { useQueries, useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { Fragment, useEffect, useMemo, useRef, useState } from "react"
import { CalendarRange, ChartLine, Clock, Database, Plus, Rows3, Sigma, X } from "lucide-react"
import { FactChip } from "@/components/chip"
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
import { metricHeading, metricQuestion, readMetrics, recentWindow, timeLabel, windowOf, type Condition, type MetricSummary, type Window } from "./lib/metrics.ts"
import { figure, unitGroups, type UnitGroup } from "./lib/answer-anatomy.ts"
import { evaluateQuestion } from "./lib/evaluate.ts"
import { readDocument } from "./lib/exact.ts"
import { ProfileTimedOut, readProfile } from "./lib/profile.ts"
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

function MetricCard({ metric, turn, onSettled }: {
  readonly metric: MetricSummary
  /** Whether every question above this one has its answer: the cell answers one at a time, top first. */
  readonly turn: boolean
  /** Called once this question is settled (answered, refused, or nothing to ask). */
  readonly onSettled: () => void
}) {
  const { view } = useView()
  // A question asks the cell only once it is near view and the ones above it have their answers.
  const [ref, seen] = useSeen<HTMLElement>()
  const active = seen && turn
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const [conditions, setConditions] = useState<readonly Condition[]>([])
  // The unit group whose weekly answer is open: chosen from the groups the cell returned, never by default.
  const [picked, setPicked] = useState<UnitGroup | null>(null)
  const profile = useQuery({ queryKey: ["profile", metric.objectType], queryFn: () => readProfile(metric.objectType), retry: false, enabled: active })
  const typeNamed = (name: string) => catalog.data?.document.flatMap((artifact) => artifact.kind === "entity" && "objectType" in artifact.payload && artifact.payload.objectType.name === name ? [artifact.payload.objectType] : [])[0]
  // A time read through links (a posting's time is its event's) is profiled on the type the links reach, as the
  // cell declares them; the window then spans every date that type carries.
  const timeType = (metric.timeLinks ?? []).reduce<string | undefined>((type, link) => type === undefined ? undefined
    : (typeNamed(type) as { readonly links?: readonly { readonly id: string; readonly target: string }[] } | undefined)?.links?.find(({ id }) => id === link)?.target, metric.objectType)
  const timeProfile = useQuery({ queryKey: ["profile", timeType], queryFn: () => readProfile(timeType!), retry: false, enabled: active && timeType !== undefined && timeType !== metric.objectType })
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
  const what = metricHeading(metric, window?.grain)
  const sources = metric.unitPartition ?? []
  const change = (next: readonly Condition[]) => { setConditions(next); setPicked(null) }
  // Nothing to ask (no records, a refused profile, no window): this question is settled and the next may go.
  const nothingToAsk = (profile.isError && !late) || profile.data?.document.rows === 0 || (window === undefined && (profile.data !== undefined || late))
  useEffect(() => { if (nothingToAsk) onSettled() }, [nothingToAsk, onSettled])
  return (
    // One question, one section: its title, its controls, then the answer's own surfaces (no card around cards).
    <section ref={ref} data-slot="panel" className="min-w-0 space-y-4">
      <header className="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-1">
        <h2 className="text-[1.625rem] font-normal tracking-[-0.025em]">{what}{metric.timeProperty !== null && <span className="text-muted-foreground"> · by {timeLabel(metric)}</span>}</h2>
        {view === "technical" && <span className="font-mono text-xs text-muted-foreground" title={metric.name}>{metric.kind}</span>}
      </header>
      <div className="space-y-4">
        {profile.isPending && <p className="text-sm text-muted-foreground">{!seen ? "Waits until you scroll to it." : !turn ? "Waits for the questions above it." : `Reading ${conceptTitle(metric.objectType).toLowerCase()} records…`}</p>}
        {profile.isError && !late && <p className="text-sm text-muted-foreground">{profile.error.message || "The cell did not say what it holds for this type."}</p>}
        {profile.data && profile.data.document.rows === 0 && (
          <EmptyState icon={Sigma} title={profile.data.document.withheldRows + profile.data.document.quarantinedRows === 0 ? "No records yet" : "No records to add up"}
            next={profile.data.document.withheldRows + profile.data.document.quarantinedRows === 0 && <Button asChild size="sm" variant="outline"><Link to="/sources"><Database />Load data or connect a source</Link></Button>}>
            {profile.data.document.withheldRows + profile.data.document.quarantinedRows === 0
              ? <>Nothing has been loaded for {conceptTitle(metric.objectType).toLowerCase()}, so there is nothing to add up.</>
              : <>{[profile.data.document.withheldRows > 0 && `${profile.data.document.withheldRows} withheld from you`, profile.data.document.quarantinedRows > 0 && `${profile.data.document.quarantinedRows} held in quarantine`].filter(Boolean).join(", ")}; none you can read is left to add up.</>}
            <span className="block pt-1 text-meta">Checked with the cell at {new Date(profile.dataUpdatedAt).toLocaleTimeString(undefined, { timeStyle: "short" })}.</span>
          </EmptyState>
        )}
        {profile.data && profile.data.document.rows > 0 && window === undefined && <p className="text-sm text-muted-foreground">No {metric.timeProperty?.replaceAll("_", " ") ?? "date"} values to ask over yet.</p>}
        {window && <>
          <div className="flex flex-wrap items-start gap-2">
            <FactChip icon={CalendarRange} label={`Per ${window.grain}`} className="h-8" title={assumed ? (late ? "The cell's profile of this type didn't arrive in time, so Studio asks the latest 52 weeks." : "The cell sent no range for this date, so Studio asks the latest 52 weeks.") : "Every date its records carry."}>{assumed ? "Latest 52 weeks" : `${day(window.start)} – ${day(Date.parse(window.end) - 86_400_000)}`}</FactChip>
            <div className="min-w-0 flex-1"><Conditions fields={(objectType?.properties ?? []).map(({ id }) => id).filter((id) => id !== metric.expression && id !== metric.timeProperty)} conditions={conditions} onChange={change} /></div>
          </div>
          {sources.length === 0
            ? <AskedAnswer question={metricQuestion(metric, window, conditions) as Question} title={what} declared={declared} heading="none" onSettled={onSettled} />
            : <UnitGroups metric={metric} window={window} conditions={conditions} sources={sources} title={what} declared={declared} picked={picked} onPick={setPicked} onSettled={onSettled} />}
        </>}
      </div>
    </section>
  )
}

/**
 * A metric whose values are in several units, asked at once: one exact total per unit group the cell
 * returns (it groups by the unit sources itself), labelled by its unit, most records first; the
 * conditions narrow it. A group opens its answer per period.
 */
function UnitGroups({ metric, window, conditions, sources, title, declared, picked, onPick, onSettled }: {
  readonly metric: MetricSummary
  readonly window: Window
  readonly conditions: readonly Condition[]
  readonly sources: readonly string[]
  readonly title: string
  readonly declared: Pick<QuestionAnswer, "metrics" | "objectType">
  readonly picked: UnitGroup | null
  readonly onPick: (group: UnitGroup | null) => void
  readonly onSettled: () => void
}) {
  const asked = metricQuestion(metric, window, conditions) as Question
  const totals: Question = { ...asked, groupBy: sources.map((property) => ({ path: { property } })) }
  const run = useQuery({ queryKey: ["unit-groups", JSON.stringify(totals)], queryFn: () => evaluateQuestion(totals), retry: false, staleTime: 30_000 })
  const result = run.data
  // Settled once the totals are back, unless one group opens its answer per period (that answer settles it).
  const single = result?.kind === "answer" && unitGroups(sources, buckets({ question: totals, metrics: [], objectType: declared.objectType.document }, { answer: result.read.document, numbers: result.read.numbers, at: "" })).length === 1
  useEffect(() => { if (!run.isPending && !single) onSettled() }, [run.isPending, single, onSettled])
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
      {groups.length === 0 ? <EmptyState icon={Sigma}>No records in this window match.</EmptyState> : groups.length === 1 ? null : (
        <div className="space-y-2">
          <p className={groups.length === 1 ? "sr-only" : "text-meta text-muted-foreground"}>{groups.length === 1 ? `In one unit, ${period(totals.window)}.` : `In ${groups.length} units, ${period(totals.window)}; most records first. Choose one to see it per ${window.grain}.`}</p>
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
      {/* One group: its total for the window is the first number on screen while its answer per period comes. */}
      {open && weekly && <AskedAnswer question={weekly} title={title} unit={open.unit} declared={declared} heading="none" onSettled={onSettled}
        placeholder={groups.length === 1 ? <div data-slot="answer-total" className="rounded-2xl bg-card p-5 shadow-[var(--elevation-1)] ring-1 ring-foreground/5 sm:p-7">
          <div className="text-meta text-muted-foreground">Total, {period(totals.window)}</div>
          <div className="pt-2 text-[3rem] leading-none font-light tracking-[-0.045em] tabular-nums">{figure(open.bucket, open.unit)}</div>
          <div className="pt-3 text-meta text-muted-foreground">{open.bucket.members.length.toLocaleString("en")} records · reading each {window.grain}…</div>
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
  return <TypeSections types={types} list={list} />
}

/**
 * The metrics by type. A type the cell holds no readable records for does not take a card per
 * metric: its metrics are listed together, compactly, after the types that have records.
 */
function TypeSections({ types, list }: { readonly types: readonly string[]; readonly list: readonly MetricSummary[] }) {
  // The profiles each card reads as it comes into view (one analytics read at a time): read here, never asked here.
  const profiles = useQueries({ queries: types.map((type) => ({ queryKey: ["profile", type], queryFn: () => readProfile(type), retry: false, enabled: false })) })
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
        <h2 className="text-section font-medium"><Link to="/objects/$type" params={{ type }} className="underline-offset-4 hover:underline">{conceptTitle(type)}</Link></h2>
        <div className="grid gap-3">{list.filter(({ objectType }) => objectType === type).map((metric) => <MetricCard key={metric.name} metric={metric} turn={turnOf(metric.name)} onSettled={settle.get(metric.name)!} />)}</div>
      </div>
    ))}
    {without.length > 0 && (
      <Panel icon={Sigma} label="No records yet">
        <Well className="p-1.5">
          <ul className="divide-y divide-border/70">
            {without.map((type) => (
              <li key={type} className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 px-2.5 py-2 text-sm">
                <Link to="/objects/$type" params={{ type }} className="font-medium underline-offset-4 hover:underline">{conceptTitle(type)}</Link>
                <span className="min-w-0 text-meta text-muted-foreground">{list.filter(({ objectType }) => objectType === type).map((metric) => metricHeading(metric)).join(" · ")}</span>
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
