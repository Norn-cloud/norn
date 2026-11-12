import { useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { useState } from "react"
import { ChartLine, Clock, Plus, Rows3, Sigma } from "lucide-react"
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from "recharts"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableFooter, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { buckets, contribution, declarations, fraction, shown, type Bucket } from "./lib/answer.ts"
import { conceptTitle } from "./lib/catalog.ts"
import { chartFrame } from "./lib/chart.ts"
import { FRESHNESS, day, headline, period, scope } from "./lib/format.ts"
import { amountTexts, liveAnswer, liveTrace, type LiveRead, type Refusal, type Trace } from "./lib/live.ts"
import type { Read } from "./lib/exact.ts"
import { transport, type QuestionAnswer } from "./lib/transport.ts"
import { CopyId } from "@/components/copy-id"
import { RestatementCard } from "./restatement.tsx"
import type { MenuEntry } from "./shell.tsx"

import { AnswerView, RefusedNote } from "./answer-view.tsx"
import { RunQuestion } from "./run.tsx"
import type { QuestionCatalog } from "./builder.tsx"
import { NotServed, useMode } from "./lib/live-transport.ts"
import { metricQuestion, readMetrics, windowOf, type MetricSummary } from "./lib/metrics.ts"
import { readProfile } from "./lib/profile.ts"
export { AnswerView, LiveRecords, RefusedNote, type FetchTrace } from "./answer-view.tsx"

/** One metric the cell derives, asked per day or week over every date its records carry. */
function MetricCard({ metric }: { readonly metric: MetricSummary }) {
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const profile = useQuery({ queryKey: ["profile", metric.objectType], queryFn: () => readProfile(metric.objectType), retry: false })
  const window = profile.data && metric.timeProperty !== null ? windowOf(profile.data.document, metric.timeProperty) : undefined
  // The answer is read with the type's own declaration, as the cell admitted it.
  const objectType = catalog.data?.document.flatMap((artifact) => artifact.kind === "entity" && "objectType" in artifact.payload && artifact.payload.objectType.name === metric.objectType ? [artifact.payload.objectType] : [])[0]
  const declarations: QuestionCatalog = {
    objectSet: metric.objectType, properties: [], metrics: [], grains: [], operators: [],
    example: { irVersion: "0.1", calendar: "iso_utc", freshness: { kind: "live", maxAgeMs: 60_000 } },
    declarations: { metrics: [], objectType: objectType ?? { name: metric.objectType, properties: [] } },
  }
  const what = metric.kind === "simple" && metric.expression !== null ? `Total ${metric.expression.replaceAll("_", " ")}` : metric.label
  return (
    <Panel icon={Sigma} label={<span>{what}{metric.timeProperty !== null && <span className="text-muted-foreground"> by {metric.timeProperty.replaceAll("_", " ")}</span>}</span>}
      action={<span className="font-mono text-xs text-muted-foreground" title={metric.name}>{metric.kind}</span>}>
      <Well className="space-y-3 p-4">
        {profile.isError && <p className="text-sm text-muted-foreground">{profile.error.message}</p>}
        {profile.data && window === undefined && <p className="text-sm text-muted-foreground">No {metric.timeProperty?.replaceAll("_", " ") ?? "date"} values to ask over yet.</p>}
        {window && <>
          <p className="text-meta text-muted-foreground">Per {window.grain}, {window.start.slice(0, 10)} to {window.end.slice(0, 10)}: every date its records carry.</p>
          <RunQuestion question={metricQuestion(metric, window)} catalog={declarations} label="Ask the cell" />
        </>}
      </Well>
    </Panel>
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
      <div className="grid gap-3 xl:grid-cols-2">{list.filter(({ objectType }) => objectType === type).map((metric) => <MetricCard key={metric.name} metric={metric} />)}</div>
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
      <div className="space-y-2">
        <h1 className="text-title font-medium">Questions</h1>
        <p className="max-w-2xl text-muted-foreground">Business questions answered exactly by the server. Every number opens to the records it was added up from.</p>
        <div className="flex flex-wrap gap-2"><Button asChild size="sm"><Link to="/questions/build"><Plus />Build a question</Link></Button><Button asChild variant="outline" size="sm"><Link to="/questions/new"><Plus />New question (form)</Link></Button></div>
      </div>
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
