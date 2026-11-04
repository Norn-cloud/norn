import { useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { useState } from "react"
import { ChartLine, Clock, Rows3, Sigma } from "lucide-react"
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from "recharts"
import { Badge } from "@/components/ui/badge"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { ListDetail } from "@/components/list-detail"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableFooter, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { conceptTitle } from "./lib/catalog.ts"
import { chartFrame } from "./lib/chart.ts"
import { member } from "./lib/exact.ts"
import { FRESHNESS, bucketLabel, day, headline, percent, period, scope } from "./lib/format.ts"
import { recordedTransport, type QuestionAnswer } from "./lib/transport.ts"
import { measured, operandUnits } from "./lib/units.ts"
import type { MenuEntry } from "./shell.tsx"

interface Bucket {
  readonly key: string
  readonly label: string
  readonly value: string
  /** One sum, or numerator then denominator, as the server wrote them, each with its declared unit. */
  readonly totals: readonly string[]
  readonly members: readonly { readonly id: string; readonly amounts: readonly (string | null)[] }[]
}

function buckets({ question, answer, metrics, objectType }: QuestionAnswer): Bucket[] {
  const units = operandUnits(question.document, metrics.document, objectType.document)
  const text = (pointer: string) => answer.numbers.get(pointer) ?? "unavailable"
  const operand = (pointer: string, index: number) => measured(text(pointer), units[index])
  return Object.entries(answer.document.rows).map(([key, row]) => {
    const at = `/rows/${member(key)}`
    return {
      key,
      label: bucketLabel(row.bucket, question.document),
      value: text(`${at}/value`),
      totals: row.totals.map((_, index) => operand(`${at}/totals/${index}`, index)),
      members: Object.entries(row.members).map(([id, { amounts }]) => ({
        id,
        amounts: amounts.map((amount, index) => amount === null ? null : operand(`${at}/members/${member(id)}/amounts/${index}`, index)),
      })),
    }
  })
}

const fraction = (totals: readonly (string | null)[]): string => totals.map((total) => total ?? "left out").join(" / ")

/** A ratio reads as a percentage at one precision everywhere; a sum stays the server's text. */
const shown = (bucket: Pick<Bucket, "value" | "totals">): string => bucket.totals.length > 1 ? percent(bucket.value) : bucket.value

function Exact({ value, totals }: { readonly value: string; readonly totals: readonly string[] }) {
  return totals.length > 1
    ? <span className="tabular-nums"><span className="text-muted-foreground">{fraction(totals)} = </span><b>{value}</b></span>
    : <b className="tabular-nums">{value}</b>
}

function DrillDown({ bucket, onClose }: { readonly bucket: Bucket | undefined; readonly onClose: () => void }) {
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
            <TableCard><Table>
              <TableHeader><TableRow><TableHead>Record</TableHead><TableHead className="text-right">Contributes</TableHead></TableRow></TableHeader>
              <TableBody>
                {bucket.members.map(({ id, amounts }) => (
                  <TableRow key={id}><TableCell className="font-mono">{id}</TableCell><TableCell className="text-right tabular-nums">{fraction(amounts)}</TableCell></TableRow>
                ))}
              </TableBody>
              <TableFooter><TableRow><TableCell>Total</TableCell><TableCell className="text-right"><Exact value={bucket.value} totals={bucket.totals} /></TableCell></TableRow></TableFooter>
            </Table></TableCard>
            <p className="pt-4 text-xs text-muted-foreground">The source rows behind each record are not available yet.</p>
          </div>
        </>}
      </SheetContent>
    </Sheet>
  )
}

const changes = (count: number) => `${count} committed ${count === 1 ? "change" : "changes"}`

function QuestionCard({ read, heading = "h2" }: { readonly read: QuestionAnswer; readonly heading?: "h1" | "h2" }) {
  const [open, setOpen] = useState<Bucket>()
  const question = read.question.document
  const answer = read.answer.document
  const rows = buckets(read)
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
          {read.answer.origin === "recorded" && <Badge variant="outline" title="A document the engine produced, replayed exactly. Not a live connection.">Recorded</Badge>}
        </div>
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
      <DrillDown bucket={open} onClose={() => setOpen(undefined)} />
    </section>
  )
}

/** The questions on the board, for the submenu island. */
export function useQuestionsMenu(): readonly MenuEntry[] | undefined {
  const board = useQuery({ queryKey: ["question-board"], queryFn: recordedTransport.questionBoard })
  return board.data?.map((read) => ({ href: `/questions/${read.question.document.name}`, label: headline(read.question.document), group: subject(read) }))
}

/** The object type a question counts, which is how the list groups questions. */
const subject = ({ objectType }: QuestionAnswer) => conceptTitle(objectType.document.name)

/** One question as a record page, listed beside the others grouped by the object type it counts. */
export function QuestionPage() {
  const { name } = useParams({ from: "/questions/$name" })
  const board = useQuery({ queryKey: ["question-board"], queryFn: recordedTransport.questionBoard })
  const read = board.data?.find(({ question }) => question.document.name === name)
  return (
    <ListDetail title="Questions" placeholder="Find a question" selected={name}
      items={(board.data ?? []).map((item) => ({
        id: item.question.document.name, to: `/questions/${item.question.document.name}`,
        title: headline(item.question.document), subtitle: scope(item.question.document), group: subject(item),
      }))}>
      {board.isPending && <p className="text-muted-foreground">Reading answers…</p>}
      {board.isError && <p className="text-destructive">{board.error.message}</p>}
      {board.data && (read ? <QuestionCard read={read} heading="h1" /> : <EmptyState icon={ChartLine}>No question named {name} is on the board.</EmptyState>)}
    </ListDetail>
  )
}

export function QuestionsPage() {
  const board = useQuery({ queryKey: ["question-board"], queryFn: recordedTransport.questionBoard })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Questions</h1>
        <p className="max-w-2xl text-muted-foreground">Business questions answered exactly by the server. Every number opens to the records it was added up from.</p>
      </div>
      {board.isPending && <p className="text-muted-foreground">Reading answers…</p>}
      {board.isError && <p className="text-destructive">{board.error.message}</p>}
      {board.data?.map((read) => <QuestionCard key={read.question.document.name} read={read} />)}
    </section>
  )
}
