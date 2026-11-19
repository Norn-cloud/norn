import { useQuery, useQueryClient } from "@tanstack/react-query"
import { useNavigate, useSearch } from "@tanstack/react-router"
import { Inbox, RefreshCw, Sparkles } from "lucide-react"
import { useEffect, useMemo, useState } from "react"
import { EmptyState } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { cn } from "@/lib/utils"
import type { CellResult } from "../lib/cell-read.ts"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import { useLiveRefresh } from "../lib/live-reads.ts"
import { badgeText, cellNow, countDecisions, DECISIONS_UNAVAILABLE, flowTitle, listDecisions, remaining, stepTitle, type Decision, type DecisionsPage } from "../lib/decisions.ts"
import { Countdown, DecisionSheet, RiskChip } from "./decision-sheet.tsx"

type Read<T> = { readonly result: CellResult<T>; readonly receivedAt: number }

const listKey = ["decisions", "pending"] as const

/** The cell's "now", ticking each second: the page's `serverNowMs` plus the time since it arrived. */
function useCellClock(serverNowMs: number | undefined, receivedAt: number | undefined): number | undefined {
  const [tick, setTick] = useState(() => Date.now())
  useEffect(() => {
    if (serverNowMs === undefined) return
    const timer = setInterval(() => setTick(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [serverNowMs])
  return serverNowMs === undefined || receivedAt === undefined ? undefined : cellNow(serverNowMs, receivedAt, tick)
}

/** The shell's badge: how many decisions wait on this actor, as the cell counts them ("500+" when capped). */
export function useDecisionBadge(): string | undefined {
  const count = useQuery({ queryKey: ["decisions", "count"], queryFn: () => countDecisions(), retry: false })
  // The cell says when the count changes (UI-LIVE-1); nothing polls.
  useLiveRefresh("/v1/decisions?state=pending&limit=0", ["decisions", "count"])
  return count.data?.kind === "ok" && count.data.value.total > 0 ? badgeText(count.data.value) : undefined
}

/** `/decisions`: what waits on you, soonest deadline first, in the cell's order. */
export function DecisionsPage() {
  const client = useQueryClient()
  const navigate = useNavigate()
  const search = useSearch({ strict: false }) as { open?: string }
  const read = useQuery<Read<DecisionsPage>>({
    queryKey: listKey,
    queryFn: async () => ({ result: await listDecisions(), receivedAt: Date.now() }),
    retry: false,
  })
  // A decision arriving, decided elsewhere or withdrawn reaches the list when the cell commits it.
  useLiveRefresh("/v1/decisions?state=pending", listKey)
  const page = read.data?.result.kind === "ok" ? read.data.result.value : undefined
  const now = useCellClock(page?.serverNowMs, read.data?.receivedAt)
  // A decision whose deadline passes leaves the list at once; the cell's next read confirms it.
  const open = useMemo(() => (page?.items ?? []).filter((d) => now === undefined || remaining(d.expiresAtMs, now) !== null), [page, now])
  const lapsed = (page?.items.length ?? 0) - open.length
  useEffect(() => { if (lapsed > 0) void client.invalidateQueries({ queryKey: ["decisions"] }) }, [lapsed, client])
  // The opened decision stays open after it leaves the list (decided, resolved elsewhere, expired), so its outcome can be read.
  const fromPage = page?.items.find((d) => d.id === search.open)
  const [held, setHeld] = useState<Decision | null>(null)
  useEffect(() => { if (fromPage !== undefined) setHeld(fromPage) }, [fromPage])
  const selected: Decision | null = search.open === undefined ? null : fromPage ?? (held?.id === search.open ? held : null)
  const missing = search.open !== undefined && page !== undefined && selected === null
  const choose = (id: string | undefined) => void navigate({ to: "/decisions", search: id === undefined ? {} : { open: id } as never, replace: true })
  const refresh = () => void client.invalidateQueries({ queryKey: ["decisions"] })

  const body = (() => {
    if (read.isPending) return <div className="grid gap-2" aria-label="Reading decisions">{[0, 1, 2].map((i) => <Skeleton key={i} className="h-20 rounded-node" />)}</div>
    if (read.isError) return (
      <div role="alert" className="flex flex-wrap items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
        <span>{read.error instanceof Error ? read.error.message : "The decisions could not be read."}</span><span className="flex-1" />
        <Button variant="outline" size="sm" onClick={() => void read.refetch()}>Retry</Button>
      </div>
    )
    const result = read.data?.result
    if (result?.kind === "no-cell") return <EmptyState icon={Inbox} title="No data cell">{NO_DATA_CELL}</EmptyState>
    if (result?.kind === "unavailable") return <EmptyState icon={Inbox} title="Not served yet">{DECISIONS_UNAVAILABLE}</EmptyState>
    if (result?.kind === "refused") return <p role="alert" className="text-sm text-destructive">{result.refusal.message}</p>
    if (open.length === 0) return <EmptyState icon={Inbox} title="Nothing waits on you">When a flow asks you to decide, it shows here.</EmptyState>
    return (
      <ol className="grid gap-2" aria-label="Decisions waiting on you">
        {open.map((d) => (
          <li key={d.id}>
            <button type="button" onClick={() => choose(d.id)}
              className={cn("grid w-full gap-1.5 rounded-node border border-node-border bg-card p-4 text-start shadow-[var(--node-highlight),var(--elevation-node)] transition-shadow hover:shadow-[var(--node-highlight),var(--elevation-node-hover)] focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none",
                selected?.id === d.id && "ring-2 ring-primary/40")}>
              <span className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
                <span>{flowTitle(d)} · {stepTitle(d)}</span><span className="flex-1" />
                <Countdown expiresAtMs={d.expiresAtMs} now={now} />
              </span>
              <span className="line-clamp-2 text-sm font-medium">{d.question}</span>
              <span className="flex flex-wrap items-center gap-2">
                <RiskChip risk={d.risk} />
                <span className="text-meta text-muted-foreground">{d.options.length} options</span>
                {d.agentPick !== null && <span className="inline-flex items-center gap-1 text-meta text-muted-foreground"><Sparkles className="size-3" aria-hidden />Suggested: {d.options.find((o) => o.id === d.agentPick)?.label ?? d.agentPick}</span>}
              </span>
            </button>
          </li>
        ))}
      </ol>
    )
  })()

  return (
    <div className="grid max-w-4xl gap-4">
      <header className="flex flex-wrap items-baseline gap-3">
        <h1 className="text-title font-semibold tracking-tight">Decisions</h1>
        {page && <span className="text-meta tabular-nums text-muted-foreground" aria-live="polite">{page.total === 0 ? "None waiting" : `${badgeText(page)} waiting on you`}</span>}
        <span className="flex-1" />
        <Button variant="ghost" size="sm" onClick={refresh} disabled={read.isFetching}><RefreshCw className={cn(read.isFetching && "animate-spin")} /> Refresh</Button>
      </header>
      {missing && <p role="status" className="rounded-lg border border-node-border bg-muted/50 px-3 py-2 text-sm">That decision isn't waiting on you: it was resolved, it expired, or you're not one of its deciders.</p>}
      {body}
      <DecisionSheet decision={selected} now={now} onClose={() => choose(undefined)} onSettled={refresh} />
    </div>
  )
}
