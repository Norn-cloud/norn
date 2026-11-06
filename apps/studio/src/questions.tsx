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
import { amountTexts, liveAnswer, liveTrace, type Refusal, type Trace } from "./lib/live.ts"
import type { Read } from "./lib/exact.ts"
import { transport, type QuestionAnswer } from "./lib/transport.ts"
import { CopyId } from "@/components/copy-id"
import { RestatementCard } from "./restatement.tsx"
import type { MenuEntry } from "./shell.tsx"

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
export function LiveRecords({ question, lineageRef, bucket }: { readonly question: string; readonly lineageRef: string; readonly bucket: Bucket }) {
  const trace = useQuery({ queryKey: ["live-trace", question, lineageRef, bucket.key], queryFn: () => liveTrace(question, lineageRef, bucket.key), retry: false })
  const data = trace.data
  if (trace.isPending) return <p className="text-sm text-muted-foreground">Reading the records…</p>
  if (trace.isError || data === undefined || data.kind === "unavailable") return <p className="text-sm text-muted-foreground">The live records could not be read.</p>
  if (data.kind === "refused") return <RefusedNote refusal={data.refusal} />
  const read = data.read
  return (
    <TableCard><Table>
      <TableHeader><TableRow><TableHead>Record</TableHead><TableHead className="text-right">Contributes</TableHead><TableHead>Receipt</TableHead></TableRow></TableHeader>
      <TableBody>
        {read.document.contributions.map((contribution, index) => (
          <TableRow key={`${contribution.fact.key}@${contribution.fact.version}`}>
            <TableCell className="font-mono">{contribution.fact.key}{contribution.restricted && <Badge variant="outline" className="ms-2 font-sans" title="The server withheld this record's values from your role.">Values withheld</Badge>}</TableCell>
            <TableCell className="text-right tabular-nums">{amounts(read, index)}</TableCell>
            <TableCell><CopyId value={contribution.receiptHash} short={10} /><div className="text-xs text-muted-foreground">commit {contribution.commitSeq}</div></TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table></TableCard>
  )
}

/** The server's refusal, as an answer: its reason in its own words, and its code. */
function RefusedNote({ refusal }: { readonly refusal: Refusal }) {
  return (
    <div role="status" className="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm">
      <div><span className="font-medium">Refused:</span> {refusal.message}</div>
      <div className="font-mono text-xs text-muted-foreground">{refusal.code}{refusal.explanation ? ` · ${refusal.explanation}` : ""}</div>
    </div>
  )
}

function DrillDown({ bucket, onClose, live }: { readonly bucket: Bucket | undefined; readonly onClose: () => void; readonly live?: { readonly question: string; readonly lineageRef: string } }) {
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
            {live ? <LiveRecords question={live.question} lineageRef={live.lineageRef} bucket={bucket} /> : <>
            <TableCard><Table>
              <TableHeader><TableRow><TableHead>Record</TableHead><TableHead className="text-right">Contributes</TableHead></TableRow></TableHeader>
              <TableBody>
                {bucket.members.map(({ id, amounts }) => (
                  <TableRow key={id}><TableCell className="font-mono">{id}</TableCell><TableCell className={amounts === null ? "text-right text-muted-foreground" : "text-right tabular-nums"}>{contribution(amounts)}</TableCell></TableRow>
                ))}
              </TableBody>
              <TableFooter><TableRow><TableCell>Total</TableCell><TableCell className="text-right"><Exact value={bucket.value} totals={bucket.totals} /></TableCell></TableRow></TableFooter>
            </Table></TableCard>
            <p className="pt-4 text-xs text-muted-foreground">The source rows behind each record are not available yet.</p>
            </>}
          </div>
        </>}
      </SheetContent>
    </Sheet>
  )
}

const changes = (count: number) => `${count} committed ${count === 1 ? "change" : "changes"}`

function QuestionCard({ read: recorded, heading = "h2" }: { readonly read: QuestionAnswer; readonly heading?: "h1" | "h2" }) {
  const [open, setOpen] = useState<Bucket>()
  // The live answer when the host serves one; otherwise the recording, labelled as such.
  const live = useQuery({ queryKey: ["live-answer", recorded.question.document.name], queryFn: () => liveAnswer(recorded.question.document.name), retry: false, staleTime: 60_000 })
  const read: QuestionAnswer = live.data?.kind === "read" ? { ...recorded, answer: live.data.read } : recorded
  const question = read.question.document
  const answer = read.answer.document
  const rows = buckets(declarations(read), { answer, numbers: read.answer.numbers, at: "" })
  const latest = rows.at(-1)
  const freshness = FRESHNESS[answer.freshness]
  const frame = chartFrame(rows)
  const sources = Object.entries(answer.watermarks).flatMap(([source, partitions]) =>
    Object.values(partitions).map(({ complete }) => `${source} ${complete ? "complete" : "incomplete"}`))
  return (
    <section className="space-y-3">
      <div className="space-y-1 px-1">
        <div className="flex flex-wrap items-center gap-2">
          {heading === "h1"
            ? <h1 className="text-title font-medium">{headline(question)}</h1>
            : <h2 className="text-section font-medium"><Link to="/questions/$name" params={{ name: question.name }} className="underline-offset-4 hover:underline">{headline(question)}</Link></h2>}
          <Badge variant={answer.freshness === "exact" || answer.freshness === "fresh" ? "secondary" : "destructive"} title={freshness.meaning}>{freshness.label}</Badge>
          {read.answer.origin === "live"
            ? <Badge variant="secondary" title="Read now from the cell as your account.">Live</Badge>
            : <Badge variant="outline" title="A document the engine produced, replayed exactly. Not a live connection.">Recorded snapshot</Badge>}
        </div>
        {live.data?.kind === "refused" && <RefusedNote refusal={live.data.refusal} />}
        <p className="text-meta text-muted-foreground">{scope(question)} · {period(question.window)} · Sources: {sources.join(", ")}</p>
      </div>
      {latest && (
        <StatStrip>
          <button type="button" className="block h-full rounded-lg text-start transition-transform duration-150 ease-spring outline-none focus-visible:ring-3 focus-visible:ring-ring/30 active:translate-y-px" onClick={() => setOpen(latest)}>
            <KpiTile icon={Sigma} label={latest.label} value={shown(latest)} sub={latest.totals.length > 1 ? `${fraction(latest.totals)} = ${latest.value} exactly` : undefined} />
          </button>
          <KpiTile icon={Rows3} label="Records counted" value={latest.members.length} sub={latest.label} />
          <KpiTile icon={Clock} label="Freshness" value={freshness.label} sub={`Read ${day(answer.nowMs)}, after ${changes(answer.knownAsOf)}`} />
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
                  <span className="font-mono tabular-nums">{String(item.payload?.shown)} <span className="text-muted-foreground">({String(item.payload?.exact)})</span></span>
                </div>
              )} />} />
              <Area dataKey="position" type="monotone" stroke="var(--color-position)" strokeWidth={1.5} fill="var(--color-position)" fillOpacity={0.08} dot={false} isAnimationActive={false} />
            </AreaChart>
          </ChartContainer>
          ) : <EmptyState icon={ChartLine}>The server returned no answers for this window.</EmptyState>}
        </Well>
      </Panel>
      <Panel icon={Rows3} label="Every week" action="Select a week to see its records">
        <TableCard><Table>
          <TableHeader><TableRow><TableHead>Period</TableHead><TableHead className="text-right">Answer</TableHead><TableHead className="text-right max-sm:hidden">Exactly</TableHead><TableHead className="text-right max-sm:hidden">Records</TableHead></TableRow></TableHeader>
          <TableBody>
            {rows.map((bucket) => (
              <TableRow key={bucket.key} className="cursor-pointer" onClick={() => setOpen(bucket)}>
                <TableCell>
                  <button type="button" className="rounded-sm underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none" onClick={(event) => { event.stopPropagation(); setOpen(bucket) }}>
                    {bucket.label}
                  </button>
                </TableCell>
                <TableCell className="text-right font-medium tabular-nums">{shown(bucket)}</TableCell>
                <TableCell className="text-right text-muted-foreground tabular-nums max-sm:hidden">{fraction(bucket.totals)} = {bucket.value}</TableCell>
                <TableCell className="text-right tabular-nums max-sm:hidden">{bucket.members.length}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table></TableCard>
      </Panel>
      <DrillDown bucket={open} onClose={() => setOpen(undefined)} live={read.answer.origin === "live" ? { question: question.name, lineageRef: answer.lineageRef } : undefined} />
    </section>
  )
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
  const board = useQuery({ queryKey: ["question-board"], queryFn: transport.questionBoard })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Questions</h1>
        <p className="max-w-2xl text-muted-foreground">Business questions answered exactly by the server. Every number opens to the records it was added up from.</p>
        <Button asChild variant="outline" size="sm"><Link to="/questions/new"><Plus />New question</Link></Button>
      </div>
      {board.isPending && <p className="text-muted-foreground">Reading answers…</p>}
      {board.isError && <p className="text-destructive">{board.error.message}</p>}
      {board.data?.map((read) => <QuestionCard key={read.question.document.name} read={read} />)}
      <RestatementCard />
    </section>
  )
}
