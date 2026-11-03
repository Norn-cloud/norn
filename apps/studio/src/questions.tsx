import { useQuery } from "@tanstack/react-query"
import { useState } from "react"
import { CartesianGrid, Line, LineChart, XAxis, YAxis } from "recharts"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { Table, TableBody, TableCell, TableFooter, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { member } from "./lib/exact.ts"
import { FRESHNESS, bucketLabel, day, headline, period, scope } from "./lib/format.ts"
import { recordedTransport, type QuestionAnswer } from "./lib/transport.ts"

interface Bucket {
  readonly key: string
  readonly label: string
  readonly value: string
  /** One sum, or numerator then denominator, as the server wrote them. */
  readonly totals: readonly string[]
  readonly members: readonly { readonly id: string; readonly amounts: readonly (string | null)[] }[]
}

function buckets({ question, answer }: QuestionAnswer): Bucket[] {
  const text = (pointer: string) => answer.numbers.get(pointer) ?? "unavailable"
  return Object.entries(answer.document.rows).map(([key, row]) => {
    const at = `/rows/${member(key)}`
    return {
      key,
      label: bucketLabel(row.bucket, question.document),
      value: text(`${at}/value`),
      totals: row.totals.map((_, index) => text(`${at}/totals/${index}`)),
      members: Object.entries(row.members).map(([id, { amounts }]) => ({
        id,
        amounts: amounts.map((amount, index) => amount === null ? null : text(`${at}/members/${member(id)}/amounts/${index}`)),
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
      <SheetContent className="w-full sm:max-w-lg">
        {bucket && <>
          <SheetHeader>
            <SheetTitle>{bucket.label}</SheetTitle>
            <SheetDescription className="text-base"><Exact value={bucket.value} totals={bucket.totals} /></SheetDescription>
          </SheetHeader>
          <div className="px-4">
            <p className="pb-3 text-sm text-muted-foreground">The {bucket.members.length} records the server added up, exactly as it counted them.</p>
            <Table>
              <TableHeader><TableRow><TableHead>Record</TableHead><TableHead className="text-right">Contributes</TableHead></TableRow></TableHeader>
              <TableBody>
                {bucket.members.map(({ id, amounts }) => (
                  <TableRow key={id}><TableCell className="font-mono text-xs">{id}</TableCell><TableCell className="text-right tabular-nums">{fraction(amounts)}</TableCell></TableRow>
                ))}
              </TableBody>
              <TableFooter><TableRow><TableCell>Total</TableCell><TableCell className="text-right"><Exact value={bucket.value} totals={bucket.totals} /></TableCell></TableRow></TableFooter>
            </Table>
            <p className="pt-4 text-xs text-muted-foreground">The source rows behind each record are not available yet.</p>
          </div>
        </>}
      </SheetContent>
    </Sheet>
  )
}

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
    <Card>
      <CardHeader>
        <div className="flex flex-wrap items-center gap-2">
          <CardTitle className="text-xl">{headline(question)}</CardTitle>
          <Badge variant={answer.freshness === "exact" || answer.freshness === "fresh" ? "secondary" : "destructive"} title={freshness.meaning}>{freshness.label}</Badge>
          {read.answer.origin === "recorded" && <Badge variant="outline" title="A document the engine produced, replayed exactly. Not a live connection.">Recorded</Badge>}
        </div>
        <CardDescription>{scope(question)} · {period(question.window)}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-8">
        {latest && (
          <button type="button" className="block text-left" onClick={() => setOpen(latest)}>
            <div className="text-sm text-muted-foreground">{latest.label}</div>
            <div className="text-5xl font-semibold tracking-tight tabular-nums">{latest.value}</div>
            <div className="text-muted-foreground tabular-nums">{fraction(latest.totals)}</div>
          </button>
        )}
        <ChartContainer config={{ value: { label: headline(question), color: "var(--chart-1)" } }} className="h-56 w-full">
          <LineChart data={rows.map(({ label, value }) => ({ label, value: Number(value) }))} margin={{ left: 8, right: 8 }}>
            <CartesianGrid vertical={false} />
            <XAxis dataKey="label" tickLine={false} axisLine={false} tickFormatter={(label: string) => label.replace(/^\w+ of /, "")} minTickGap={24} />
            <YAxis domain={["auto", "auto"]} tickLine={false} axisLine={false} width={56} tickFormatter={(tick: number) => tick.toFixed(4)} />
            <ChartTooltip content={<ChartTooltipContent />} />
            <Line dataKey="value" stroke="var(--color-value)" strokeWidth={2} dot={false} isAnimationActive={false} />
          </LineChart>
        </ChartContainer>
        <Table>
          <TableHeader><TableRow><TableHead>Period</TableHead><TableHead className="text-right">Answer</TableHead><TableHead className="text-right">Records</TableHead></TableRow></TableHeader>
          <TableBody>
            {rows.map((bucket) => (
              <TableRow key={bucket.key} className="cursor-pointer" onClick={() => setOpen(bucket)}>
                <TableCell>{bucket.label}</TableCell>
                <TableCell className="text-right"><Exact value={bucket.value} totals={bucket.totals} /></TableCell>
                <TableCell className="text-right tabular-nums">{bucket.members.length}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
      <CardFooter className="text-sm text-muted-foreground">
        After {answer.knownAsOf} committed {answer.knownAsOf === 1 ? "change" : "changes"}, read {day(answer.nowMs)} · {sources.join(", ")}. Select any number to see the records behind it.
      </CardFooter>
      <DrillDown bucket={open} onClose={() => setOpen(undefined)} />
    </Card>
  )
}

export function QuestionsPage() {
  const board = useQuery({ queryKey: ["question-board"], queryFn: recordedTransport.questionBoard })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-3xl font-semibold tracking-tight">Questions</h1>
        <p className="max-w-2xl text-muted-foreground">Business questions answered exactly by the server. Every number opens to the records it was added up from.</p>
      </div>
      {board.isPending && <p className="text-muted-foreground">Reading answers…</p>}
      {board.isError && <p className="text-destructive">{board.error.message}</p>}
      {board.data?.map((read) => <QuestionCard key={read.question.document.name} read={read} />)}
    </section>
  )
}
