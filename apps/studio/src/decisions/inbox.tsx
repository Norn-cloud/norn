import { useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate, useSearch } from "@tanstack/react-router"
import { CheckCircle2, Hourglass, Inbox, Play, RefreshCw, ShieldAlert, Sparkles } from "lucide-react"
import { useEffect, useMemo, useState } from "react"
import { EmptyState, PageHeader } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { cn } from "@/lib/utils"
import type { CellResult } from "../lib/cell-read.ts"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import { useLiveRefresh } from "../lib/live-reads.ts"
import { badgeText, cellNow, countDecisions, DECISIONS_UNAVAILABLE, flowTitle, listDecisions, listRequested, parkedFrom, remaining, requestedAction, requestedPath, requestedWords, stepTitle, type Decision, type DecisionsPage, type RequestedDecision, type RequestedPage } from "../lib/decisions.ts"
import { listActions } from "../lib/actions.ts"
import { ActionSheet, type OpenAction } from "../actions/action-sheet.tsx"
import { heldCountText, useHeldBack } from "../laws.tsx"
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
  const { section: heldSection } = useHeldBack()
  const heldCount = heldSection && (heldSection.count > 0 || heldSection.approximate) ? heldCountText(heldSection.count, heldSection.approximate) : undefined

  const body = (() => {
    if (read.isPending) return <div className="grid gap-2" aria-label="Reading decisions">{[0, 1, 2].map((i) => <Skeleton key={i} className="h-16 rounded-lg" />)}</div>
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
      // One ruled list, not a stack of cards: the question leads, its deadline sits at the end, the rest is one quiet line.
      <ol className="divide-y overflow-hidden rounded-lg border bg-card" aria-label="Decisions waiting on you">
        {open.map((d) => (
          <li key={d.id}>
            <button type="button" onClick={() => choose(d.id)} aria-current={selected?.id === d.id ? "true" : undefined}
              className="grid w-full gap-1 px-4 py-3 text-start transition-colors hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset aria-current:bg-muted">
              <span className="flex items-baseline gap-3">
                <span className="line-clamp-2 min-w-0 flex-1 text-sm font-medium">{d.question}</span>
                <span className="shrink-0 text-meta"><Countdown expiresAtMs={d.expiresAtMs} now={now} /></span>
              </span>
              <span className="flex flex-wrap items-center gap-x-2 gap-y-1 text-meta text-muted-foreground">
                <RiskChip risk={d.risk} />
                <span>{flowTitle(d)} · {stepTitle(d)}</span>
                <span aria-hidden>·</span><span>{d.options.length} options</span>
                {d.agentPick !== null && <><span aria-hidden>·</span><span className="inline-flex items-center gap-1"><Sparkles className="size-3" aria-hidden />Suggested: {d.options.find((o) => o.id === d.agentPick)?.label ?? d.agentPick}</span></>}
              </span>
            </button>
          </li>
        ))}
      </ol>
    )
  })()

  return (
    <div className="grid max-w-4xl gap-6">
      <PageHeader title="Inbox" description="Decisions your flows wait on you to make, soonest deadline first."
        meta={page && <span className="tabular-nums" aria-live="polite">{page.total === 0 ? "None waiting" : `${badgeText(page)} waiting on you`}</span>}
        actions={<>
          <Button asChild variant="outline" size="sm">
            <Link to="/rules" search={{ view: "held" } as never}>
              <ShieldAlert aria-hidden />
              Held by rules
              {heldCount !== undefined && <span className="text-muted-foreground tabular-nums">{heldCount}</span>}
            </Link>
          </Button>
          <Button variant="ghost" size="sm" onClick={refresh} disabled={read.isFetching}><RefreshCw className={cn(read.isFetching && "animate-spin")} /> Refresh</Button>
        </>} />
      {missing && <p role="status" className="rounded-lg border bg-muted/50 px-3 py-2 text-sm">That decision isn't waiting on you: it was resolved, it expired, or you're not one of its deciders.</p>}
      {body}
      <RequestedCalls />
      <DecisionSheet decision={selected} now={now} onClose={() => choose(undefined)} onSettled={refresh} />
    </div>
  )
}

const requestedKey = ["decisions", "requested"] as const

/**
 * The calls you sent that wait on an approval, as the cell lists them for their requester
 * (APPROVAL-REQUESTED-1): pending a decider, or approved and not yet run. "Approved, run it" appears
 * only when the cell offers it (`canRun`); it opens the call's sheet, which sends exactly the call the
 * cell holds and shows the receipt, or the cell's refusal.
 */
function RequestedCalls() {
  const read = useQuery<Read<RequestedPage>>({ queryKey: requestedKey, queryFn: async () => ({ result: await listRequested(), receivedAt: Date.now() }), retry: false })
  useLiveRefresh(requestedPath(), requestedKey)
  const catalog = useQuery({ queryKey: ["actions", "all", null], queryFn: () => listActions(undefined), retry: false, staleTime: 60_000 })
  const page = read.data?.result.kind === "ok" ? read.data.result.value : undefined
  const now = useCellClock(page?.serverNowMs, read.data?.receivedAt)
  const [acting, setActing] = useState<OpenAction | null>(null)
  const items = page?.items ?? []
  const actions = catalog.data?.kind === "catalog" ? catalog.data.items : []
  const run = (item: RequestedDecision) => setActing({ action: requestedAction(actions, item), parked: parkedFrom(item), runNow: true })
  const open = (item: RequestedDecision) => setActing({ action: requestedAction(actions, item), parked: parkedFrom(item) })
  // The sheet stays where it is when the list empties under it (the call it ran left the list).
  return (
    <>
    {items.length > 0 && <section aria-labelledby="requested-heading" className="grid gap-2">
      <h2 id="requested-heading" className="text-section font-medium">Your requests</h2>
      <ol className="divide-y overflow-hidden rounded-lg border bg-card" aria-label="Your requests">
        {items.map((item) => (
          <li key={item.id} className="flex flex-wrap items-center gap-3 px-4 py-3">
            {item.status === "approved"
              ? <CheckCircle2 className="size-4 shrink-0 text-[var(--tint-outcome)]" aria-hidden />
              : <Hourglass className="size-4 shrink-0 text-muted-foreground" aria-hidden />}
            <button type="button" onClick={() => open(item)} className="grid min-w-0 flex-1 gap-1 text-start focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none rounded-sm">
              <span className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
                <span>{requestedAction(actions, item).title}</span>
                {item.status === "pending" && <><span className="flex-1" /><Countdown expiresAtMs={item.expiresAtMs} now={now} /></>}
              </span>
              <span className="line-clamp-2 text-sm font-medium">{item.question}</span>
              <span className="text-meta text-muted-foreground">{requestedWords(item)}</span>
            </button>
            <RiskChip risk={item.risk} />
            {item.canRun && <Button type="button" size="sm" onClick={() => run(item)}><Play /> Approved, run it</Button>}
          </li>
        ))}
      </ol>
    </section>}
    <ActionSheet open={acting} onClose={() => setActing(null)} />
    </>
  )
}
