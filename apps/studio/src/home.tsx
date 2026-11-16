import { useQueries, useQuery } from "@tanstack/react-query"
import { Link, useNavigate } from "@tanstack/react-router"
import { ArrowRight, ArrowUp, Sigma, Sparkles } from "lucide-react"
import { useState } from "react"
import { useAssistant } from "@/assistant-slot"
import { EmptyState, PageHeader } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card"
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from "@/components/ui/input-group"
import { Skeleton } from "@/components/ui/skeleton"
import { conceptTitle } from "./lib/catalog.ts"
import { evaluateQuestion, type Evaluation } from "./lib/evaluate.ts"
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
    <section className="space-y-8">
      <PageHeader title="Home" description="Ask, act on what needs you, and read the numbers that matter. Every item opens where you act on it." />
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
      <InputGroup className="h-12 rounded-2xl bg-card shadow-elevation">
        <InputGroupAddon><Sparkles aria-hidden /></InputGroupAddon>
        <InputGroupInput aria-label="Ask or tell Norn anything" placeholder="Ask or tell Norn anything" value={text} onChange={(event) => setText(event.target.value)} />
        <InputGroupAddon align="inline-end">
          <InputGroupButton type="submit" size="sm" variant="default" className="rounded-full" disabled={text.trim() === ""}><ArrowUp aria-hidden />Ask</InputGroupButton>
        </InputGroupAddon>
      </InputGroup>
    </form>
  )
}

/** Why the server gave no number, in its own words: the checker's findings, the read gate's refusal, or its bound. */
function refusalReason(result: Exclude<Evaluation, { kind: "answer" }>): string {
  switch (result.kind) {
    case "not-a-question": return result.findings.map(({ message }) => message).filter(Boolean).join(" ") || "The server's checker refused it."
    case "refused": return result.refusal.message
    case "too-large": case "bad-request": return result.message || "The server refused it."
    default: return "The cell did not answer."
  }
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
  // A key number the server refused (or did not answer) leaves Home; its reason, in the server's words, is listed below.
  const refused = picked.flatMap((metric, index) => { const data = answers[index]?.data; return data !== undefined && data.kind !== "answer" ? [{ metric, reason: refusalReason(data) }] : [] })
  const shown = picked.filter((metric) => !refused.some((entry) => entry.metric === metric))
  return (
    <section aria-labelledby="key-numbers" className="space-y-3">
      <h2 id="key-numbers" className="text-section font-medium">Key numbers</h2>
      {metrics.isPending && <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">{Array.from({ length: KEY_NUMBERS }, (_, index) => <Skeleton key={index} className="h-32 rounded-xl" />)}</div>}
      {metrics.isError && <p className="text-sm text-muted-foreground">{metrics.error.message}</p>}
      {metrics.data && picked.length === 0 && <EmptyState icon={Sigma}>No admitted measure adds up on its own yet.</EmptyState>}
      {shown.length > 0 && (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          {shown.map((metric) => {
            const answer = answers[picked.indexOf(metric)]
            const result = answer?.data?.kind === "answer" ? answer.data.read : undefined
            const [key] = Object.keys(result?.document.rows ?? {})
            const value = key === undefined ? undefined : result?.numbers.get(`/rows/${member(key)}/value`)
            return (
              <Card key={metric.name} size="sm">
                <CardHeader>
                  <CardDescription>{conceptTitle(metric.objectType)}</CardDescription>
                  <CardTitle className="text-sm font-medium">{metricHeading(metric)}</CardTitle>
                  {result && <CardAction><Badge variant="outline" title={FRESHNESS[result.document.freshness].meaning}>{FRESHNESS[result.document.freshness].label}</Badge></CardAction>}
                </CardHeader>
                <CardContent>
                  {answer?.isPending && <Skeleton className="h-8 w-24" />}
                  {result && <p className="text-2xl font-semibold tracking-tight tabular-nums">{value === undefined ? "—" : groupDigits(value)}</p>}
                </CardContent>
                <CardFooter>
                  <Button asChild variant="link" size="sm" className="h-auto px-0"><Link to="/metrics">Why this number<ArrowRight aria-hidden /></Link></Button>
                </CardFooter>
              </Card>
            )
          })}
        </div>
      )}
      {refused.length > 0 && (
        <ul className="space-y-0.5 text-meta text-muted-foreground" aria-label="Key numbers not shown">
          {refused.map(({ metric, reason }) => <li key={metric.name} data-refused-number={metric.name}>{conceptTitle(metric.objectType)}, {metricHeading(metric).toLowerCase()}: not shown. {reason}</li>)}
        </ul>
      )}
    </section>
  )
}

