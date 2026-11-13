import { useMutation, useQuery } from "@tanstack/react-query"
import { Link, useNavigate, useParams } from "@tanstack/react-router"
import { CheckCircle2, Circle, Hourglass, Loader2, MinusCircle, Play, Radio, Workflow, XCircle, type LucideIcon } from "lucide-react"
import { useRef } from "react"
import { CopyId } from "@/components/copy-id"
import { EmptyState } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { useView } from "@/lib/view"
import { cn } from "@/lib/utils"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import { useLiveRefresh } from "../lib/live-reads.ts"
import { getRun, newRunId, phaseOf, refreshEvery, runTitle, RUNS_UNAVAILABLE, startRun, stepTitle, type Phase, type Run, type RunStep, type StepStatus } from "../lib/runs.ts"

const STATUS: Readonly<Record<StepStatus, { readonly icon: LucideIcon; readonly label: string; readonly tone: string }>> = {
  pending: { icon: Circle, label: "Not started", tone: "text-muted-foreground" },
  running: { icon: Loader2, label: "Running", tone: "text-[var(--tint-tool)]" },
  waiting: { icon: Hourglass, label: "Waiting on a person", tone: "text-[var(--tint-ask)]" },
  done: { icon: CheckCircle2, label: "Done", tone: "text-[var(--tint-outcome)]" },
  failed: { icon: XCircle, label: "Failed", tone: "text-destructive" },
  skipped: { icon: MinusCircle, label: "Skipped", tone: "text-muted-foreground" },
}

const PHASE: Readonly<Record<Phase, string>> = { running: "Running", waiting: "Waiting on a decision", succeeded: "Succeeded", failed: "Failed", ended: "Ended", active: "In progress" }

const KIND: Readonly<Record<RunStep["kind"], string>> = { perform: "Step", decide: "Ask a person", route: "Route", wait: "Wait", succeed: "Outcome", fail: "Outcome" }

/**
 * The button that starts a run of an installed flow and opens it: the automation canvas's Run, and
 * "Run again" here. The cell decides whether this actor may start it; a refusal reads in place.
 */
export function RunButton({ flow, label = "Run", variant = "default" }: { readonly flow: string; readonly label?: string; readonly variant?: "default" | "outline" }) {
  const navigate = useNavigate()
  const start = useMutation({
    mutationFn: async () => startRun(flow, newRunId()),
    onSuccess: (result) => { if (result.kind === "ok") void navigate({ to: "/runs/$id", params: { id: result.value.id } }) },
  })
  const problem = start.isError ? (start.error instanceof Error ? start.error.message : "The cell did not answer.")
    : start.data?.kind === "refused" ? start.data.refusal.message
      : start.data?.kind === "unavailable" ? RUNS_UNAVAILABLE : start.data?.kind === "no-cell" ? NO_DATA_CELL : undefined
  return (
    <span className="inline-flex flex-wrap items-center gap-2">
      <Button size="sm" variant={variant} className={cn(variant === "outline" && "border-node-border")} disabled={start.isPending} onClick={() => start.mutate()}>
        {start.isPending ? <Loader2 className="animate-spin" /> : <Play />} {label}
      </Button>
      {problem !== undefined && <span role="alert" className="text-meta text-destructive">{problem}</span>}
    </span>
  )
}

const time = (ms: number | null) => ms === null ? null : new Date(ms)
const took = (step: RunStep) => step.startedAtMs !== null && step.endedAtMs !== null ? `${Math.max(0, Math.round((step.endedAtMs - step.startedAtMs) / 100) / 10)} s` : null

/** `/runs/$id`: a run's steps as they happen, read again while it works or waits; receipts under Details. */
export function RunPage() {
  const { id = "" } = useParams({ strict: false })
  const { view } = useView()
  const technical = view === "technical"
  const etag = useRef<string | null>(null)
  const last = useRef<Run | null>(null)
  const read = useQuery({
    queryKey: ["run", id],
    queryFn: async () => {
      const result = await getRun(id, etag.current)
      if (result.kind === "ok") { etag.current = result.etag; last.current = result.value }
      return { result, at: Date.now() }
    },
    retry: false,
  })
  const result = read.data?.result
  // A 304 keeps the run already read.
  const run = result?.kind === "ok" ? result.value : result?.kind === "not-modified" ? last.current : null
  // While the run works or waits, each commit that changes it is read once (UI-LIVE-1); an ended run is not watched.
  useLiveRefresh(run !== null && refreshEvery(phaseOf(run)) !== false ? `/v1/runs/${encodeURIComponent(id)}` : null, ["run", id])

  if (read.isPending) return <div className="grid max-w-4xl gap-3" aria-label="Reading the run"><Skeleton className="h-8 w-64" />{[0, 1, 2, 3].map((i) => <Skeleton key={i} className="h-14 rounded-node" />)}</div>
  if (read.isError) return (
    <div role="alert" className="flex max-w-4xl flex-wrap items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
      <span>{read.error instanceof Error ? read.error.message : "The run could not be read."}</span><span className="flex-1" />
      <Button variant="outline" size="sm" onClick={() => void read.refetch()}>Retry</Button>
    </div>
  )
  if (result?.kind === "no-cell") return <EmptyState icon={Workflow} title="No data cell">{NO_DATA_CELL}</EmptyState>
  if (result?.kind === "unavailable") return <EmptyState icon={Workflow} title="Not served yet">{RUNS_UNAVAILABLE}</EmptyState>
  if (result?.kind === "refused") return <EmptyState icon={Workflow} title="No such run">{result.refusal.message}</EmptyState>
  if (run === null) return null

  const phase = phaseOf(run)
  const live = refreshEvery(phase) !== false
  const flowId = run.flow.ruleId.replace(/\.flow\.rule$/, "")
  return (
    <div className="grid max-w-4xl gap-4">
      <header className="grid gap-2">
        <div className="flex flex-wrap items-center gap-3">
          <h1 className="text-title font-semibold tracking-tight">{runTitle(run)}</h1>
          <span className={cn("rounded-md px-2 py-0.5 text-xs", phase === "failed" ? "bg-destructive/10 text-destructive" : phase === "succeeded" ? "bg-[color-mix(in_oklab,var(--tint-outcome)_14%,transparent)] text-[color-mix(in_oklab,var(--tint-outcome)_80%,var(--foreground))]" : "bg-muted text-foreground")}>{PHASE[phase]}</span>
          {live && <span className="inline-flex items-center gap-1 text-meta text-muted-foreground" aria-live="polite"><Radio className="size-3 animate-pulse" aria-hidden />Live</span>}
          <span className="flex-1" />
          <RunButton flow={flowId} label="Run again" variant="outline" />
        </div>
        <p className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
          <span>Run</span><CopyId value={run.id} />
          <span>· started {new Date(run.createdAtMs).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}</span>
          {technical && <span>· {run.flow.ruleId} v{run.flow.version}{run.steps !== undefined && ` · generation ${run.generation}`} · version {run.version}</span>}
        </p>
      </header>
      {run.steps === undefined && (
        <section aria-label="Run" className="grid gap-3 rounded-node border border-node-border bg-card p-4 shadow-[var(--node-highlight),var(--elevation-node)]">
          <dl className="grid grid-cols-[minmax(7rem,auto)_1fr] gap-x-4 gap-y-2 text-sm">
            <dt className="text-muted-foreground">State</dt><dd>{run.state === null ? <span className="text-muted-foreground">Ended (the run object is gone)</span> : <code className="text-xs">{run.state}</code>}</dd>
            <dt className="text-muted-foreground">Flow</dt><dd>{runTitle(run)} <span className="text-meta text-muted-foreground">v{run.flow.version}</span></dd>
            <dt className="text-muted-foreground">Generation</dt><dd className="tabular-nums">{run.generation}</dd>
            <dt className="text-muted-foreground">Updated</dt><dd>{new Date(run.updatedAtMs).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" })}</dd>
          </dl>
          <p className="rounded-lg border border-dashed border-node-border px-3 py-2 text-meta text-muted-foreground">Step details are coming soon: this cell doesn't send a run's steps yet.</p>
        </section>
      )}
      {(run.decision ?? null) !== null && (
        <div className="flex flex-wrap items-center gap-3 rounded-node border border-[color-mix(in_oklab,var(--tint-ask)_35%,transparent)] bg-[color-mix(in_oklab,var(--tint-ask)_8%,var(--card))] px-4 py-3 text-sm">
          <Hourglass className="size-4 text-[var(--tint-ask)]" aria-hidden />
          <span>This run waits on a person's decision.</span><span className="flex-1" />
          <Link to="/decisions" search={{ open: run.decision!.id } as never} className="font-medium underline-offset-4 hover:underline">Open decision</Link>
        </div>
      )}
      {run.steps !== undefined && <ol aria-label="Steps" className="grid gap-2">
        {run.steps.map((step, index) => {
          const s = STATUS[step.status]
          const Icon = s.icon
          const started = time(step.startedAtMs)
          return (
            <li key={step.id} aria-current={step.status === "running" || step.status === "waiting" ? "step" : undefined}
              className={cn("grid gap-2 rounded-node border border-node-border bg-card p-4 shadow-[var(--node-highlight),var(--elevation-node)]", step.status === "pending" || step.status === "skipped" ? "opacity-70" : undefined)}>
              <div className="flex flex-wrap items-center gap-2">
                <Icon className={cn("size-4 shrink-0", s.tone, step.status === "running" && "animate-spin")} aria-hidden />
                <span className="text-meta text-muted-foreground tabular-nums">{index + 1}</span>
                <span className="font-medium">{stepTitle(step)}</span>
                <span className="rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">{KIND[step.kind]}</span>
                <span className="flex-1" />
                <span className={cn("text-meta", s.tone)}>{s.label}</span>
                {took(step) && <span className="text-meta text-muted-foreground tabular-nums">· {took(step)}</span>}
              </div>
              {step.error && <p role="alert" className="text-sm text-destructive">{step.error.message}{technical && <code className="ms-2 text-xs">{step.error.code}</code>}</p>}
              {step.receipts.length > 0 && (
                <details open={technical} className="text-sm">
                  <summary className="cursor-pointer text-meta text-muted-foreground select-none hover:text-foreground">
                    {step.receipts.some((r) => r.executor) ? `Done by ${step.receipts.find((r) => r.executor)?.executor}` : "Details"}{started ? ` · ${started.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}` : ""}
                  </summary>
                  <ul className="mt-2 grid gap-2">
                    {step.receipts.map((r) => {
                      const at = new Date(r.recordedAtMs)
                      return (
                        <li key={r.receiptHash} className="grid grid-cols-[minmax(7rem,auto)_1fr] gap-x-4 gap-y-1 rounded-lg border border-node-border p-3">
                          <span className="text-muted-foreground">Receipt</span><span className="min-w-0"><CopyId value={r.receiptHash} short={16} /></span>
                          <span className="text-muted-foreground">Kind</span><span>{r.kind === "decision" ? "A person's decision" : r.kind === "function" ? "A function's result" : "A commit"}</span>
                          {r.executor && <><span className="text-muted-foreground">By</span><span>{r.executor}</span></>}
                          <span className="text-muted-foreground">Commit</span><span className="tabular-nums">{r.commitSeq}</span>
                          <span className="text-muted-foreground">Recorded</span><time dateTime={at.toISOString()} className="whitespace-nowrap">{at.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" })}</time>
                        </li>
                      )
                    })}
                  </ul>
                </details>
              )}
            </li>
          )
        })}
      </ol>}
    </div>
  )
}
