import { useQuery, useQueryClient } from "@tanstack/react-query"
import { Dot, type DotTone } from "@/components/system/status-dot"
import { Link, useNavigate, useSearch } from "@tanstack/react-router"
import { Inbox, Play, RefreshCw, ShieldAlert, Sparkles, Workflow } from "lucide-react"
import { useEffect, useMemo, useState } from "react"
import { LoadState } from "@/components/load-state"
import { PageHeader } from "@/components/panel"
import { EmptyState } from "@/components/system/panel"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import type { CellResult, Refusal as CellReadRefusal } from "../lib/cell-read.ts"
import { CellUnavailable, NO_DATA_CELL } from "../lib/definitions.ts"
import { CellRefusal } from "../lib/live-transport.ts"
import { useLiveRefresh } from "../lib/live-reads.ts"
import { badgeText, cellNow, DECISIONS_UNAVAILABLE, flowTitle, listDecisions, listRequested, parkedFrom, remaining, requestedAction, requestedPath, requestedWords, stepTitle, type Decision, type DecisionsPage, type RequestedDecision, type RequestedPage } from "../lib/decisions.ts"
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

export { useDecisionBadge } from "../lib/nav-menus.ts"

function inboxRefusalError(refusal: CellReadRefusal): Error {
  const body = refusal.body
  const explanation = typeof body["explanation"] === "string" ? body["explanation"] : undefined
  const incidentId = typeof body["incidentId"] === "string" ? body["incidentId"] : undefined
  const retryable = typeof body["retryable"] === "boolean" ? body["retryable"] : undefined
  const category = typeof body["category"] === "string" ? body["category"] : undefined
  const decidedAt = typeof body["decidedAt"] === "string" ? body["decidedAt"] : undefined
  if (refusal.status >= 500 || retryable === false) {
    return new CellUnavailable(refusal.status, refusal.message, { code: refusal.code, explanation, incidentId, retryable, category, decidedAt })
  }
  return new CellRefusal(refusal.status, refusal.code, refusal.message, { explanation, incidentId, retryable, category, decidedAt })
}

/**
 * A requested call's status dot: held while it waits; live once approved and run (or approved with
 * nothing left to run); the accent while an approval waits for the reader to run it; bad when the
 * decider rejected it or the cell refused the approved call; quiet when it expired or was withdrawn.
 */
export function requestDot(item: Pick<RequestedDecision, "status" | "canRun" | "call">): DotTone {
  switch (item.status) {
    case "pending": return "held"
    case "rejected": return "bad"
    case "expired":
    case "withdrawn": return "quiet"
    case "approved": {
      const state = item.call.state?.state
      if (state === "refused") return "bad"
      if (state === "committed") return "live"
      return item.canRun ? "accent" : "live"
    }
  }
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
    if (read.isPending) return <LoadState status="pending" bone="decisions-list" skeleton="list" loadingLabel="Reading decisions" onRetry={refresh}>{() => null}</LoadState>
    if (read.isError) return <LoadState status="error" error={read.error} isFetching={read.isFetching} onRetry={() => void read.refetch()}>{() => null}</LoadState>
    const result = read.data?.result
    if (result?.kind === "no-cell") return <EmptyState icon={Inbox} title="No data cell">{NO_DATA_CELL}</EmptyState>
    if (result?.kind === "unavailable") return <EmptyState icon={Inbox} title="Not served yet">{DECISIONS_UNAVAILABLE}</EmptyState>
    if (result?.kind === "refused") return <LoadState status="error" error={inboxRefusalError(result.refusal)} isFetching={read.isFetching} onRetry={() => void read.refetch()}>{() => null}</LoadState>
    if (open.length === 0) return <EmptyState icon={Inbox} title="Nothing waits on you" next={<Button asChild size="sm" variant="outline"><Link to="/workflows"><Workflow />View workflows</Link></Button>}>When a flow asks you to decide, it shows here.</EmptyState>
    return (
      // One list in a ShellCard well, not a stack of cards: the question leads, its deadline sits at the end, the rest is one quiet line.
      <div data-slot="panel" className="rounded-(--radius-panel) border border-(--panel-border) bg-card shadow-(--panel-shadow)">
      <ol data-slot="well" className="grid gap-px p-1.5" aria-label="Decisions waiting on you">
        {open.map((d) => (
          <li key={d.id}>
            <button type="button" onClick={() => choose(d.id)} aria-current={selected?.id === d.id ? "true" : undefined}
              className="grid min-h-12 w-full gap-1 rounded-(--radius-row) px-3 py-2.5 text-start transition-colors outline-none hover:bg-(--well-hover) focus-visible:outline-2 focus-visible:outline-solid focus-visible:-outline-offset-2 focus-visible:outline-ring aria-current:bg-card aria-current:ring-1 aria-current:ring-panel-border motion-reduce:transition-none">
              <span className="flex items-baseline gap-3">
                <span className="line-clamp-2 min-w-0 flex-1 text-body">{d.question}</span>
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
      </div>
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
      {missing && <p role="status" className="rounded-lg border bg-muted/50 px-3 py-2 text-body">That decision isn't waiting on you: it was resolved, it expired, or you're not one of its deciders.</p>}
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
      <h2 id="requested-heading" className="text-heading">Your requests</h2>
      <div data-slot="panel" className="rounded-(--radius-panel) border border-(--panel-border) bg-card shadow-(--panel-shadow)">
      <ol data-slot="well" className="grid gap-px p-1.5" aria-label="Your requests">
        {items.map((item) => (
          <li key={item.id} className="flex min-h-12 flex-wrap items-center gap-3 rounded-(--radius-row) px-3 py-2.5">
            {/* Dot-first (DESIGN.md 4.3, 12): one dot per outcome; the words below say which. */}
            <Dot tone={requestDot(item)} data-status={item.status}>
              <button type="button" onClick={() => open(item)} className="grid min-w-0 flex-1 gap-1 rounded-(--radius-row) text-start outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring">
                <span className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
                  <span>{requestedAction(actions, item).title}</span>
                  {item.status === "pending" && <><span className="flex-1" /><Countdown expiresAtMs={item.expiresAtMs} now={now} /></>}
                </span>
                <span className="line-clamp-2 text-body">{item.question}</span>
                <span className="text-meta text-muted-foreground">{requestedWords(item)}</span>
              </button>
            </Dot>
            <RiskChip risk={item.risk} />
            {item.canRun && <Button type="button" size="sm" onClick={() => run(item)}><Play /> Approved, run it</Button>}
          </li>
        ))}
      </ol>
      </div>
    </section>}
    <ActionSheet open={acting} onClose={() => setActing(null)} />
    </>
  )
}
