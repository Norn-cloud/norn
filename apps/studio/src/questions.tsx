import { useQuery } from "@tanstack/react-query"
import { useState } from "react"
import { ChartLine, Clock, Rows3, Sigma } from "lucide-react"
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from "recharts"
import { Badge } from "@/components/ui/badge"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableFooter, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { member } from "./lib/exact.ts"
import { FRESHNESS, bucketLabel, day, headline, period, scope } from "./lib/format.ts"
import { recordedTransport, type QuestionAnswer } from "./lib/transport.ts"
import { measured, operandUnits } from "./lib/units.ts"
import { MenuItem } from "./shell.tsx"

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

function QuestionCard({ read }: { readonly read: QuestionAnswer }) {
  const [open, setOpen] = useState<Bucket>()
  const question = read.question.document
  const answer = read.answer.document
  const rows = buckets(read)
  const latest = rows.at(-1)
  const freshness = FRESHNESS[answer.freshness]
  const sources = Object.entries(answer.watermarks).flatMap(([source, partitions]) =>
    Object.values(partitions).map(({ complete }) => `${source} ${complete ? "complete" : "incomplete"}`))
  return (
    <section id={question.name} className="scroll-mt-4 space-y-3">
      <div className="space-y-1 px-1">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="text-section font-medium">{headline(question)}</h2>
          <Badge variant={answer.freshness === "exact" || answer.freshness === "fresh" ? "secondary" : "destructive"} title={freshness.meaning}>{freshness.label}</Badge>
          {read.answer.origin === "recorded" && <Badge variant="outline" title="A document the engine produced, replayed exactly. Not a live connection.">Recorded</Badge>}
        </div>
        <p className="text-meta text-muted-foreground">{scope(question)} · {period(question.window)} · Sources: {sources.join(", ")}</p>
      </div>
      {latest && (
        <StatStrip>
          <button type="button" className="block h-full rounded-xl text-start transition-transform duration-150 ease-spring outline-none focus-visible:ring-3 focus-visible:ring-ring/30 active:translate-y-px" onClick={() => setOpen(latest)}>
            <KpiTile icon={Sigma} label={latest.label} value={latest.value} sub={fraction(latest.totals)} />
          </button>
          <KpiTile icon={Rows3} label="Records counted" value={latest.members.length} sub={latest.label} />
          <KpiTile icon={Clock} label="Freshness" value={freshness.label} sub={`Read ${day(answer.nowMs)}, after ${changes(answer.knownAsOf)}`} />
        </StatStrip>
      )}
      <Panel icon={ChartLine} label={headline(question)} action={period(question.window)}>
        <Well className="px-2 pt-4 pb-1">
          <ChartContainer config={{ value: { label: headline(question), color: "var(--chart-1)" } }} className="h-56 w-full">
            <AreaChart data={rows.map(({ label, value }) => ({ label, value: Number(value), exact: value }))} margin={{ left: 8, right: 8 }}>
              <CartesianGrid vertical={false} strokeDasharray="2 4" />
              <XAxis dataKey="label" tickLine={false} axisLine={false} tickFormatter={(label: string) => label.replace(/^\w+ of /, "")} minTickGap={24} />
              <YAxis domain={["auto", "auto"]} tickLine={false} axisLine={false} width={56} tickFormatter={(tick: number) => tick.toFixed(4)} />
              <ChartTooltip content={<ChartTooltipContent formatter={(_value, _name, item) => (
                <div className="flex w-full justify-between gap-4">
                  <span className="text-muted-foreground">{headline(question)}</span>
                  <span className="font-mono tabular-nums">{String(item.payload?.exact)}</span>
                </div>
              )} />} />
              <Area dataKey="value" type="monotone" stroke="var(--color-value)" strokeWidth={1.5} fill="var(--color-value)" fillOpacity={0.08} dot={false} isAnimationActive={false} />
            </AreaChart>
          </ChartContainer>
        </Well>
      </Panel>
      <Panel icon={Rows3} label="Every week" action="Select a week to see its records">
        <TableCard><Table>
          <TableHeader><TableRow><TableHead>Period</TableHead><TableHead className="text-right">Answer</TableHead><TableHead className="text-right">Records</TableHead></TableRow></TableHeader>
          <TableBody>
            {rows.map((bucket) => (
              <TableRow key={bucket.key} className="cursor-pointer" onClick={() => setOpen(bucket)}>
                <TableCell>
                  <button type="button" className="rounded-sm underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none" onClick={(event) => { event.stopPropagation(); setOpen(bucket) }}>
                    {bucket.label}
                  </button>
                </TableCell>
                <TableCell className="text-right"><Exact value={bucket.value} totals={bucket.totals} /></TableCell>
                <TableCell className="text-right tabular-nums">{bucket.members.length}</TableCell>
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
export function QuestionsMenu() {
  const board = useQuery({ queryKey: ["question-board"], queryFn: recordedTransport.questionBoard })
  return <>{board.data?.map(({ question }) => (
    <MenuItem key={question.document.name} href={`#${question.document.name}`}>{headline(question.document)}</MenuItem>
  ))}</>
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
