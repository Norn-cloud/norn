import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate } from "@tanstack/react-router"
import { ArrowRight, ArrowUp, Blend, Database, LayoutDashboard, ShieldCheck, Sigma, Sparkles } from "lucide-react"
import { useState } from "react"
import { useAssistant } from "@/assistant-slot"
import { LoadState, WorkspaceUnavailable } from "@/components/load-state"
import { PageHeader, Panel, Well } from "@/components/panel"
import { StatStrip } from "@/components/kpi-tile"
import { Button } from "@/components/ui/button"
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from "@/components/ui/input-group"
import { Skeleton } from "@/components/ui/skeleton"
import { conceptTitle } from "./lib/catalog.ts"
import { useStudioScope } from "./lib/definitions.ts"
import { evaluateQuestion, type Evaluation } from "./lib/evaluate.ts"
import { member } from "./lib/exact.ts"
import { FRESHNESS, groupDigits } from "./lib/format.ts"
import { retryMode, useMode } from "./lib/live-transport.ts"
import { metricHeading, readMetrics, recentWindow, type MetricSummary } from "./lib/metrics.ts"
import { useView } from "./lib/view.tsx"
import { Inbox as InboxPanel } from "./inbox.tsx"

/** How many key numbers Home asks for: the cell answers each live, one request apiece. */
const KEY_NUMBERS = 4

/** Whether a live cell holds only the host's trusted base (`core.*`, `std.*`) and no tenant model yet. */
export const isEmptyWorkspace = (artifacts: readonly { readonly artifactId: string }[]): boolean =>
  artifacts.every(({ artifactId }) => artifactId.startsWith("core.") || artifactId.startsWith("std."))

const CHECKLIST = [
  { step: "1", title: "Add a source", description: "Connect a system or upload data to bring records into this workspace.", to: "/sources", icon: Database },
  { step: "2", title: "Model it", description: "Define your object types, relationships, and transforms.", to: "/transforms", icon: Blend },
  { step: "3", title: "Rules", description: "Add laws, guarantees, and markings to govern every change.", to: "/rules", icon: ShieldCheck },
  { step: "4", title: "Dashboard", description: "Ask questions and pin live numbers from admitted metrics.", to: "/dashboards", icon: LayoutDashboard },
] as const

/** Guided checklist shown on Home for a freshly provisioned cell with no tenant definitions yet. */
function EmptyWorkspaceChecklist() {
  return (
    <Panel icon={Sparkles} label="Get started" action="4 steps">
      <Well>
        <ol className="grid gap-px p-1.5">
          {CHECKLIST.map(({ step, title, description, to, icon: Icon }) => (
            <li key={to}>
              <Link to={to} className="group flex min-h-12 items-center gap-3 rounded-(--radius-row) px-3 py-2.5 transition-colors hover:bg-(--well-hover)">
                <span className="flex size-[28px] shrink-0 items-center justify-center rounded-full bg-card text-meta text-muted-foreground tabular-nums ring-1 ring-line">{step}</span>
                <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                <span className="min-w-0 flex-1">
                  <span className="block text-body text-foreground">{title}</span>
                  <span className="block text-meta text-muted-foreground">{description}</span>
                </span>
                <ArrowRight aria-hidden className="size-4 shrink-0 text-subtle-foreground transition-transform group-hover:translate-x-0.5 rtl:rotate-180 rtl:group-hover:-translate-x-0.5 motion-reduce:transition-none" />
              </Link>
            </li>
          ))}
        </ol>
      </Well>
    </Panel>
  )
}

/** "What do I do now": ask, what needs you, and the numbers that matter, each one click from where to act. */
export function HomePage() {
  const queryClient = useQueryClient()
  const mode = useMode()
  const retry = () => { void retryMode(queryClient) }
  const empty = mode.data?.kind === "live" && Array.isArray(mode.data.catalog?.document) && isEmptyWorkspace(mode.data.catalog.document)
  return (
    <section className="max-w-5xl space-y-8">
      <PageHeader title="Home" description={empty ? "Your workspace is ready. Follow these steps to bring data in and build your first dashboard." : "What needs you, and the numbers that matter. Every item opens where you act on it."} />
      <AskBar />
      {mode.isError
        ? <WorkspaceUnavailable error={mode.error} isFetching={mode.isFetching} onRetry={retry} />
        : mode.isPending
          ? <LoadState status="pending" bone="home-key-numbers" skeleton="cards" loadingLabel="Connecting to workspace" onRetry={retry}>{() => null}</LoadState>
          : empty
            ? <EmptyWorkspaceChecklist />
            : <>
                <InboxPanel />
                <KeyNumbers />
              </>}
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
      <InputGroup className="h-12 rounded-(--radius-search) max-sm:h-[52px] border-(--panel-border) bg-card px-1 shadow-(--panel-shadow) dark:bg-card">
        <InputGroupAddon><Sparkles aria-hidden className="text-muted-foreground" /></InputGroupAddon>
        <InputGroupInput aria-label="Ask or tell Norn anything" placeholder="Ask or tell Norn anything" value={text} onChange={(event) => setText(event.target.value)} />
        <InputGroupAddon align="inline-end">
          <InputGroupButton type="submit" size="sm" variant="default" className="rounded-full px-3.5" disabled={text.trim() === ""}><ArrowUp aria-hidden />Ask</InputGroupButton>
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
    case "unavailable": return "The cell did not answer."
  }
}

/** A metric this page can total on its own: additive, over one unit, with a time to bound it. */
const totalable = (metric: MetricSummary) => metric.timeProperty !== null && (metric.unitPartition ?? []).length === 0 && metric.numerator === null && metric.additivity?.kind !== "non_additive"

/** The first admitted metrics, each answered live by the cell as one total over the latest 26 weeks. */
function KeyNumbers() {
  const live = useMode().data?.kind === "live"
  const { view } = useView()
  const scope = useStudioScope()
  const scopeKey = scope.data ? `${scope.data.orgId}:${scope.data.environmentId}` : ""
  const metrics = useQuery({ queryKey: ["metrics", scopeKey], queryFn: ({ signal }) => readMetrics({ signal, scope: scope.data }), retry: false, enabled: live && !scope.isPending })
  const picked = (metrics.data?.document.metrics ?? []).filter(totalable).slice(0, KEY_NUMBERS)
  const window = recentWindow(Date.now())
  const answers = useQueries({
    queries: picked.map((metric) => ({
      queryKey: ["key-number", scopeKey, metric.name, window.start],
      queryFn: () => evaluateQuestion({
        irVersion: "0.1", name: "studio.key_number", objectSet: metric.objectType, aggregate: [{ metric: metric.name }],
        window: { start: window.start, end: window.end }, freshness: { kind: "live", maxAgeMs: 60_000 }, calendar: "iso_utc",
      }, undefined, scope.data),
      retry: false,
      staleTime: 60_000,
    })),
  })
  if (!live) return null
  const entries = picked.map((metric, index) => ({ metric, answer: answers[index] }))
  const shown = entries.filter(({ answer }) => answer?.data === undefined || answer.data.kind === "answer")
  const refused = entries.flatMap(({ metric, answer }) => {
    const data = answer?.data
    return data && data.kind !== "answer" ? [{ metric, reason: refusalReason(data) }] : []
  })
  return (
    <section aria-labelledby="key-numbers" className="space-y-3">
      <h2 id="key-numbers" className="text-heading">Key numbers</h2>
      <LoadState
        status={metrics.status}
        data={metrics.data}
        error={metrics.error}
        isFetching={metrics.isFetching}
        onRetry={() => void metrics.refetch()}
        bone="home-key-numbers"
        skeleton="cards"
        loadingLabel="Reading key numbers"
        isEmpty={() => picked.length === 0}
        empty={{
          icon: Sigma,
          title: "No key numbers yet",
          description: "No admitted measure adds up on its own yet.",
          next: <Button asChild size="sm" variant="outline"><Link to="/sources"><Database />Connect a source or upload a CSV</Link></Button>,
        }}
        notServed={{ icon: Sigma, title: "Metrics not served yet", description: "This workspace does not serve derived metrics yet." }}
      >
        {() => (
          <>
            {shown.length > 0 && (
              <StatStrip>
                {shown.map(({ metric, answer }) => {
                  const result = answer?.data?.kind === "answer" ? answer.data.read : undefined
                  const [key] = Object.keys(result?.document.rows ?? {})
                  const value = key === undefined ? undefined : result?.numbers.get(`/rows/${member(key)}/value`)
                  const label = metricHeading(metric, "period", view)
                  return (
                    // A StatWell (DESIGN.md 10): the label in the rim, the figure and its caption in the inset well.
                    <section key={metric.name} data-slot="kpi" className="flex h-full min-w-0 flex-col rounded-(--radius-panel) border border-(--panel-border) bg-card p-(--panel-inset) shadow-(--panel-shadow)">
                      <h3 className="flex h-9 min-w-0 items-center px-3.5 text-body text-foreground" title={label}><span className="truncate">{label}</span></h3>
                      <div data-slot="kpi-well" className="flex flex-1 flex-col justify-between gap-1.5 rounded-(--radius-well) border border-(--well-border) bg-(--well) px-4 pt-3.5 pb-3">
                        {answer?.isPending && <Skeleton className="h-7 w-24" />}
                        {result && <p className="text-figure tabular-nums [overflow-wrap:anywhere]">{value === undefined ? "—" : groupDigits(value)}</p>}
                        {answer?.isError && (
                          <p className="text-body text-muted-foreground">
                            Couldn't read this number.{" "}
                            <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={() => void answer?.refetch()}>Retry</button>
                          </p>
                        )}
                        <p className="flex min-w-0 items-center gap-1.5 text-meta text-muted-foreground">
                          <span className="truncate">{conceptTitle(metric.objectType)}</span>
                          {result && <><span aria-hidden className="text-subtle-foreground">·</span><span title={FRESHNESS[result.document.freshness].meaning}>{FRESHNESS[result.document.freshness].label}</span></>}
                          <Link to="/metrics" className="ms-auto inline-flex shrink-0 items-center gap-0.5 text-foreground underline-offset-4 hover:underline">Why<ArrowRight aria-hidden className="size-3.5 rtl:rotate-180" /></Link>
                        </p>
                      </div>
                    </section>
                  )
                })}
              </StatStrip>
            )}
            {refused.length > 0 && (
              <ul className="space-y-0.5 text-meta text-muted-foreground" aria-label="Key numbers not shown">
                {refused.map(({ metric, reason }) => (
                  <li key={metric.name} data-refused-number={metric.name}>
                    {conceptTitle(metric.objectType)} · {metricHeading(metric, "period", view)} (not shown). {reason}
                  </li>
                ))}
              </ul>
            )}
          </>
        )}
      </LoadState>
    </section>
  )
}
