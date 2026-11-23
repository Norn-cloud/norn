import { useQueries, useQuery } from "@tanstack/react-query"
import { Link, useNavigate } from "@tanstack/react-router"
import { ArrowRight, ArrowUp, Sigma, Sparkles } from "lucide-react"
import { useState } from "react"
import { useAssistant } from "@/assistant-slot"
import { EmptyState, PageHeader } from "@/components/panel"
import { StatStrip } from "@/components/kpi-tile"
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from "@/components/ui/input-group"
import { Skeleton } from "@/components/ui/skeleton"
import { conceptTitle } from "./lib/catalog.ts"
import { evaluateQuestion } from "./lib/evaluate.ts"
import { member } from "./lib/exact.ts"
import { FRESHNESS, groupDigits } from "./lib/format.ts"
import { useMode } from "./lib/live-transport.ts"
import { metricHeading, readMetrics, recentWindow, type MetricSummary } from "./lib/metrics.ts"
import { Inbox as InboxPanel } from "./inbox.tsx"

/** How many key numbers Home asks for: the cell answers each live, one request apiece. */
const KEY_NUMBERS = 4

/** "What do I do now": ask, what needs you, and the numbers that matter, each one click from where to act. */
export function HomePage() {
  return (
    <section className="max-w-5xl space-y-8">
      <PageHeader title="Home" description="What needs you, and the numbers that matter. Every item opens where you act on it." />
      <AskBar />
      <InboxPanel />
      <KeyNumbers />
    </section>
  )
}

/** One field for a question or an instruction: the assistant takes it where the server serves one, else Metrics. */
function AskBar() {
  const [text, setText] = useState("")
  const assistant = useAssistant()
  const navigate = useNavigate()
  const submit = () => {
    const request = text.trim()
    if (request === "") return
    if (assistant.available) assistant.ask(request)
    else void navigate({ to: "/metrics" })
    setText("")
  }
  return (
    <form onSubmit={(event) => { event.preventDefault(); submit() }}>
      <InputGroup className="h-11 rounded-lg border-border bg-card dark:bg-card">
        <InputGroupAddon><Sparkles aria-hidden /></InputGroupAddon>
        <InputGroupInput aria-label="Ask or tell Norn anything" placeholder="Ask or tell Norn anything" value={text} onChange={(event) => setText(event.target.value)} />
        <InputGroupAddon align="inline-end">
          <InputGroupButton type="submit" size="sm" variant="default" disabled={text.trim() === ""}><ArrowUp aria-hidden />Ask</InputGroupButton>
        </InputGroupAddon>
      </InputGroup>
    </form>
  )
}

/** A metric this page can total on its own: additive, over one unit, with a time to bound it. */
const totalable = (metric: MetricSummary) => metric.timeProperty !== null && (metric.unitPartition ?? []).length === 0 && metric.numerator === null && metric.additivity?.kind !== "non_additive"

/** The first admitted metrics, each answered live by the cell as one total over the latest 26 weeks. */
function KeyNumbers() {
  const live = useMode().data?.kind === "live"
  const metrics = useQuery({ queryKey: ["metrics"], queryFn: readMetrics, retry: false, enabled: live })
  const picked = (metrics.data?.document.metrics ?? []).filter(totalable).slice(0, KEY_NUMBERS)
  const window = recentWindow(Date.now())
  const answers = useQueries({
    queries: picked.map((metric) => ({
      queryKey: ["key-number", metric.name, window.start],
      queryFn: () => evaluateQuestion({
        irVersion: "0.1", name: "studio.key_number", objectSet: metric.objectType, aggregate: [{ metric: metric.name }],
        window: { start: window.start, end: window.end }, freshness: { kind: "live", maxAgeMs: 60_000 }, calendar: "iso_utc",
      }),
      retry: false,
      staleTime: 60_000,
    })),
  })
  if (!live) return null
  return (
    <section aria-labelledby="key-numbers" className="space-y-3">
      <h2 id="key-numbers" className="text-section font-medium">Key numbers</h2>
      {metrics.isPending && <Skeleton className="h-28 rounded-lg" />}
      {metrics.isError && <p className="text-sm text-muted-foreground">{metrics.error.message}</p>}
      {metrics.data && picked.length === 0 && <div className="rounded-lg border"><EmptyState icon={Sigma}>No admitted measure adds up on its own yet.</EmptyState></div>}
      {picked.length > 0 && (
        <StatStrip>
          {picked.map((metric, index) => {
            const answer = answers[index]
            const result = answer?.data?.kind === "answer" ? answer.data.read : undefined
            const [key] = Object.keys(result?.document.rows ?? {})
            const value = key === undefined ? undefined : result?.numbers.get(`/rows/${member(key)}/value`)
            return (
              <section key={metric.name} data-slot="kpi" className="flex min-w-0 flex-col gap-1.5 px-4 py-3">
                <h3 className="truncate text-meta text-muted-foreground" title={metricHeading(metric)}>{metricHeading(metric)}</h3>
                {answer?.isPending && <Skeleton className="h-7 w-24" />}
                {result && <p className="text-figure leading-none font-medium tracking-tight tabular-nums [overflow-wrap:anywhere]">{value === undefined ? "—" : groupDigits(value)}</p>}
                {answer?.data && answer.data.kind !== "answer" && <p className="text-sm text-muted-foreground">The cell did not answer this one.</p>}
                <p className="flex min-w-0 items-center gap-1.5 text-meta text-muted-foreground">
                  <span className="truncate">{conceptTitle(metric.objectType)}</span>
                  {result && <><span aria-hidden>·</span><span title={FRESHNESS[result.document.freshness].meaning}>{FRESHNESS[result.document.freshness].label}</span></>}
                  <Link to="/metrics" className="ms-auto inline-flex shrink-0 items-center gap-0.5 text-foreground underline-offset-4 hover:underline">Why<ArrowRight aria-hidden className="size-3.5" /></Link>
                </p>
              </section>
            )
          })}
        </StatStrip>
      )}
    </section>
  )
}
