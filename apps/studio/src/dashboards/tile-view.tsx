import { useQuery } from "@tanstack/react-query"
import { ChartArea, ChartColumn, ChartLine, ChartPie, Hash, Maximize2, Table2, type LucideIcon } from "lucide-react"
import { useState } from "react"
import { Area, AreaChart, Bar, BarChart, CartesianGrid, Cell, Line, LineChart, Pie, PieChart, XAxis, YAxis, type PieLabelRenderProps } from "recharts"
import { ValidationPanel } from "@/components/editor"
import { EmptyState, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { Skeleton } from "@/components/ui/skeleton"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { buckets, declarations, fraction, shown, type Bucket } from "@/lib/answer"
import { chartEmpty, chartFrame, pieSlices, type ChartFrame } from "@/lib/chart"
import type { Tile } from "@/lib/definitions"
import { evaluateQuestion, traceQuestion, type Evaluation } from "@/lib/evaluate"
import { FRESHNESS } from "@/lib/format"
import { AnswerView, RefusedNote } from "../answer-view.tsx"
import { fieldOf, type QuestionCatalog } from "../builder.tsx"
import { answerRead, notRun } from "../run.tsx"

export type View = Tile["view"]

/** The view picker's choices, in the order the picker lists them. */
export const VIEWS = [["number", "Number"], ["chart:line", "Line chart"], ["chart:area", "Area chart"], ["chart:bar", "Bar chart"], ["chart:pie", "Pie chart"], ["table", "Table"]] as const
export const viewKey = ({ kind, chart }: View): string => kind === "chart" ? `chart:${chart ?? "line"}` : kind
export const viewOf = (key: string): View => key === "table" || key === "number" ? { kind: key } : { kind: "chart", chart: key.slice("chart:".length) as NonNullable<View["chart"]> }

const ICONS: Record<string, LucideIcon> = { number: Hash, table: Table2, "chart:line": ChartLine, "chart:area": ChartArea, "chart:bar": ChartColumn, "chart:pie": ChartPie }
export const viewIcon = (view: View): LucideIcon => ICONS[viewKey(view)] ?? ChartLine
export function ViewIcon({ view }: { readonly view: View }) {
  const Icon = viewIcon(view)
  return <Icon aria-hidden className="size-4 text-muted-foreground" />
}

/**
 * The view a new tile starts with, from its question's shape: one value is a
 * number, a time series a line, a breakdown a bar. The cell still decides
 * whether the view fits; this is only the first pick.
 */
export function defaultView(question: { readonly groupBy?: readonly { readonly grain?: string }[] }): View {
  const [by] = question.groupBy ?? []
  if (by === undefined) return { kind: "number" }
  return by.grain === undefined ? { kind: "chart", chart: "bar" } : { kind: "chart", chart: "line" }
}

const PIE_FILLS = ["var(--chart-1)", "var(--chart-2)", "var(--chart-3)", "var(--chart-4)", "var(--chart-5)"]
const short = (label: string) => label.replace(/^\w+ of /, "")

function Tip({ name }: { readonly name: string }) {
  return <ChartTooltip content={<ChartTooltipContent formatter={(_value, _name, item) => (
    <div className="flex w-full justify-between gap-4">
      <span className="text-muted-foreground">{name}</span>
      <span className="font-mono tabular-nums">{String(item.payload?.shown ?? item.payload?.exact)}{item.payload?.share ? <span className="text-muted-foreground"> ({String(item.payload.share)} of the total)</span> : null}</span>
    </div>
  )} />} />
}

function Series({ chart, frame, name }: { readonly chart: "line" | "bar" | "area"; readonly frame: ChartFrame; readonly name: string }) {
  const data = [...frame.points]
  const axes = <>
    <CartesianGrid vertical={false} strokeDasharray="2 4" />
    <XAxis dataKey="label" tickLine={false} axisLine={false} tickFormatter={short} minTickGap={24} />
    <YAxis domain={chart === "bar" ? [Math.min(0, frame.scale[0]), Math.max(0, frame.scale[1])] : [...frame.scale]} allowDataOverflow tickLine={false} axisLine={false} width={48} tickFormatter={(tick: number) => `${tick}${frame.unit}`} />
    <Tip name={name} />
  </>
  return (
    <ChartContainer config={{ position: { label: name, color: "var(--chart-1)" } }} className="h-52 w-full">
      {chart === "bar" ? (
        <BarChart data={data} margin={{ left: 8, right: 8 }}>{axes}<Bar dataKey="position" fill="var(--color-position)" fillOpacity={0.85} radius={[3, 3, 0, 0]} isAnimationActive={false} /></BarChart>
      ) : chart === "area" ? (
        <AreaChart data={data} margin={{ left: 8, right: 8 }}>{axes}<Area dataKey="position" type="monotone" stroke="var(--color-position)" strokeWidth={1.5} fill="var(--color-position)" fillOpacity={0.08} dot={false} isAnimationActive={false} /></AreaChart>
      ) : (
        <LineChart data={data} margin={{ left: 8, right: 8 }}>{axes}<Line dataKey="position" type="monotone" stroke="var(--color-position)" strokeWidth={1.5} dot={false} connectNulls={false} isAnimationActive={false} /></LineChart>
      )}
    </ChartContainer>
  )
}

function Rows({ rows }: { readonly rows: readonly Bucket[] }) {
  return (
    <TableCard><Table>
      <TableHeader><TableRow><TableHead>Bucket</TableHead><TableHead className="text-right">Answer</TableHead><TableHead className="text-right max-sm:hidden">Records</TableHead></TableRow></TableHeader>
      <TableBody>
        {rows.map((bucket) => (
          <TableRow key={bucket.key}>
            <TableCell>{bucket.label}</TableCell>
            <TableCell className="text-right font-medium tabular-nums">{shown(bucket)}</TableCell>
            <TableCell className="text-right tabular-nums max-sm:hidden">{bucket.members.length}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table></TableCard>
  )
}

/** One answer drawn as the tile's declared view. Every label is the server's text; positions only place marks. */
export function DrawnAnswer({ view, rows, name }: { readonly view: View; readonly rows: readonly Bucket[]; readonly name: string }) {
  // No buckets is said in words, whatever the view: never an empty table, number or chart.
  if (rows.length === 0) return <EmptyState icon={viewIcon(view)}>{chartEmpty(rows)}</EmptyState>
  if (view.kind === "table") return <Rows rows={rows} />
  if (view.kind === "number") {
    const [only] = rows
    // The cell admits a number only for a question without a group-by: one bucket.
    if (only === undefined || rows.length > 1) return <Rows rows={rows} />
    return (
      <div className="px-4 py-5" data-view="number">
        <div className="text-figure leading-none font-normal tracking-[-0.02em] tabular-nums">{shown(only)}</div>
        <div className="pt-2 text-meta text-muted-foreground tabular-nums">{only.suppressed ? "" : only.totals.length > 1 ? `${fraction(only.totals)} = ${only.value} exactly · ` : ""}{only.members.length} {only.members.length === 1 ? "record" : "records"}</div>
      </div>
    )
  }
  if (view.chart === "pie") {
    const slices = pieSlices(rows)
    if (typeof slices === "string") return <div className="space-y-2"><p role="status" className="px-2 text-meta text-muted-foreground">{slices}</p><Rows rows={rows} /></div>
    // A slice's share beside it when it is wide enough to carry one; every part is named in the table either way.
    const sliceLabel = (props: PieLabelRenderProps) => {
      const [cx, cy, midAngle, outerRadius, percent] = [props.cx, props.cy, props.midAngle, props.outerRadius, props.percent].map(Number)
      if (!((percent ?? 0) >= 0.06)) return null
      const angle = (-(midAngle ?? 0) * Math.PI) / 180
      const radius = (outerRadius ?? 0) + 14
      return <text x={(cx ?? 0) + radius * Math.cos(angle)} y={(cy ?? 0) + radius * Math.sin(angle)} textAnchor={Math.cos(angle) >= 0 ? "start" : "end"} dominantBaseline="central" className="fill-muted-foreground text-[11px] tabular-nums">{slices[Number(props.index)]?.share}</text>
    }
    return (
      <div className="grid items-center gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]" data-view="pie">
        <ChartContainer config={Object.fromEntries(slices.map((slice, index) => [`s${index}`, { label: slice.label, color: PIE_FILLS[index % PIE_FILLS.length] }]))} className="h-52 w-full">
          <PieChart>
            <Tip name={name} />
            <Pie data={slices.map((slice) => ({ ...slice, shown: slice.exact }))} dataKey="value" nameKey="label" innerRadius="50%" outerRadius="72%" strokeWidth={2} stroke="var(--card)" label={sliceLabel} labelLine={false} isAnimationActive={false}>
              {slices.map((slice, index) => <Cell key={slice.label} fill={PIE_FILLS[index % PIE_FILLS.length]} />)}
            </Pie>
          </PieChart>
        </ChartContainer>
        <table className="w-full text-meta tabular-nums" aria-label={`${name}, by part`}>
          <caption className="sr-only">{name}: each part's exact value and its share of the whole</caption>
          <thead className="sr-only"><tr><th scope="col">Part</th><th scope="col">Value</th><th scope="col">Share</th></tr></thead>
          <tbody>
            {slices.map((slice, index) => (
              <tr key={slice.label} className="border-b last:border-b-0">
                <th scope="row" className="py-1.5 pe-2 text-start font-normal">
                  <span className="flex min-w-0 items-center gap-2"><span aria-hidden className="size-2.5 shrink-0 rounded-[3px] border" style={{ background: PIE_FILLS[index % PIE_FILLS.length] }} /><span className="truncate">{slice.label}</span></span>
                </th>
                <td className="py-1.5 text-end">{slice.exact}</td>
                <td className="w-14 py-1.5 text-end text-muted-foreground">{slice.share}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    )
  }
  const frame = chartFrame(rows)
  return frame ? <div data-view={view.chart ?? "line"}><Series chart={view.chart ?? "line"} frame={frame} name={name} /></div> : <EmptyState icon={viewIcon(view)}>{chartEmpty(rows)}</EmptyState>
}

/** Why a tile has no answer: the checker's findings, the read gates' refusal, or a plain reason. Never an empty chart. */
function NoAnswer({ result }: { readonly result: Exclude<Evaluation, { kind: "answer" }> }) {
  if (result.kind === "not-a-question") return <ValidationPanel title="The cell refused this question" issues={result.findings.map(({ code, message, pointer }) => ({ code, message, field: fieldOf(pointer) }))} />
  if (result.kind === "refused") return <RefusedNote refusal={result.refusal} />
  return <p role="status" className="px-2 py-6 text-center text-meta text-muted-foreground">{notRun(result)}</p>
}

/**
 * A tile's answer, read live from the cell as the signed-in account (the
 * same evaluate route and read gates as a published question), drawn as its
 * view. Details opens the full answer: every bucket and its records.
 */
export function TileAnswer({ tile, catalog }: { readonly tile: Pick<Tile, "title" | "question" | "view">; readonly catalog: QuestionCatalog }) {
  const { question } = tile
  const run = useQuery({ queryKey: ["tile-answer", JSON.stringify(question)], queryFn: () => evaluateQuestion(question), retry: false, staleTime: 30_000 })
  const [details, setDetails] = useState(false)
  const result = run.data
  if (run.isPending) return <Well className="p-3"><Skeleton className="h-40 w-full rounded-md" /></Well>
  if (run.isError || result === undefined) return <Well><p role="alert" className="px-2 py-6 text-center text-meta text-destructive">Not run: {run.error instanceof Error ? run.error.message : "no answer"}</p></Well>
  if (result.kind !== "answer") return <Well><NoAnswer result={result} /></Well>
  const read = answerRead(question, catalog, result)
  const rows = buckets(declarations(read), { answer: read.answer.document, numbers: read.answer.numbers, at: "" })
  const freshness = FRESHNESS[read.answer.document.freshness]
  return (
    <div className="space-y-1.5">
      <Well className="px-2 pt-3 pb-1"><DrawnAnswer view={tile.view} rows={rows} name={tile.title} /></Well>
      <div className="flex items-center gap-2 px-1">
        <Badge variant={read.answer.document.freshness === "exact" || read.answer.document.freshness === "fresh" ? "secondary" : "destructive"} title={freshness.meaning}>{freshness.label}</Badge>
        <Button type="button" variant="ghost" size="sm" className="ms-auto" onClick={() => setDetails(true)}><Maximize2 />Details</Button>
      </div>
      <Sheet open={details} onOpenChange={setDetails}>
        <SheetContent className="w-full overflow-y-auto sm:max-w-4xl">
          <SheetHeader>
            <SheetTitle>{tile.title}</SheetTitle>
            <SheetDescription>Every bucket the cell answered, and the records behind each one.</SheetDescription>
          </SheetHeader>
          <div className="p-4">{details && <AnswerView read={read} heading="h3" fetchTrace={(lineageRef, bucket) => traceQuestion(question, lineageRef, bucket)} />}</div>
        </SheetContent>
      </Sheet>
    </div>
  )
}
