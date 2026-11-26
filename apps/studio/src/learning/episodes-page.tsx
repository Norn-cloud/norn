import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query"
import { Dot } from "@/components/system/status-dot"
import { Link } from "@tanstack/react-router"
import { CheckCircle2, CircleDashed, Clock, Filter, GitCompareArrows, Hourglass, RefreshCw, ShieldAlert, X } from "lucide-react"
import { useEffect, useMemo, useState } from "react"
import { Chip, type Tone } from "@/components/system/chip"
import { PageHeader } from "@/components/panel"
import { EmptyState, Panel, Well } from "@/components/system/panel"
import { TableCard } from "@/components/system/table-card"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { cn } from "@/lib/utils"
import { useView } from "@/lib/view"
import { humanize } from "../lib/cell-read.ts"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import {
  AGREEMENTS, EPISODES_UNAVAILABLE,
  episodeFilterPredicate, episodeFiltersFromServed, listEpisodes,
  type Agreement, type Episode, type EpisodeFilters, type OutcomeState, type Verdict,
} from "../lib/learning/episode.ts"
import { DATA_CHANGED, LIMITS, recordsQueryParams, SERVER_QUERY_UNAVAILABLE, totalText, type RecordsQuery } from "../lib/records-query.ts"
import { AGREEMENT_STYLE, VERDICT_LABEL } from "./episode-page.tsx"
import { LearningGate, useBoundLearningRead } from "./gate.tsx"

const AGREEMENT_TONE: Readonly<Record<Agreement, Tone>> = {
  accepted: "green",
  edited: "violet",
  overridden: "amber",
  none: "gray",
}

const OUTCOME_TONE: Readonly<Record<OutcomeState, Tone>> = {
  recorded: "green",
  open: "gray",
  overdue: "red",
}

const VERDICT_TONE: Readonly<Record<Verdict, Tone>> = {
  exception: "blue",
  noise: "gray",
  pattern: "green",
  gap: "amber",
}

const selectClass = "h-8 max-sm:min-h-[44px] rounded-md border border-input bg-background px-2.5 text-body outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
const when = (ms: number) => new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })

function rulesSummary(episode: Episode): { readonly text: string; readonly tone: Tone } {
  const waived = episode.ruleOutcomes.filter((r) => r.outcome === "blocked" && r.waiver !== null).length
  const blocked = episode.ruleOutcomes.filter((r) => r.outcome === "blocked" && r.waiver === null).length
  const wouldBlock = episode.ruleOutcomes.filter((r) => r.outcome === "would_block").length
  if (blocked > 0) return { text: `${blocked} blocked`, tone: "red" }
  if (waived > 0) return { text: `${waived} waived`, tone: "amber" }
  if (wouldBlock > 0) return { text: `${wouldBlock} would block`, tone: "amber" }
  if (episode.ruleOutcomes.length === 0) return { text: "No rules", tone: "gray" }
  return { text: `${episode.ruleOutcomes.length} allowed`, tone: "green" }
}

/**
 * `/learning/episodes`: server-driven list of `learning.Episode` projections (#1220 L-2).
 *
 * Gated behind S1 capability discovery (`learning.Episode` advertised by the scoped cell).
 * Sends only filters accepted by the real generic object query route (`agreement` today;
 * see `crates/norn-api/src/object_query.rs:291`). The other four #1208 filters (outcome state,
 * law id, producer manifest pin, and triage verdict / untriaged) are hidden until the backend
 * follow-up exposes flat filterable properties or nested query grammar with route-produced
 * fixtures. Studio never filters client-side and reflects only what the server echoes back.
 */
export function EpisodesPage() {
  return (
    <LearningGate surface="episodes" icon={GitCompareArrows} title="Episodes">
      <EpisodesPageContent />
    </LearningGate>
  )
}

function EpisodesPageContent() {
  const { view } = useView()
  const technical = view === "technical"
  const client = useQueryClient()
  const bound = useBoundLearningRead()

  const [filters, setFilters] = useState<EpisodeFilters>({})
  const [cursors, setCursors] = useState<readonly (string | undefined)[]>([undefined])
  const [pageIndex, setPageIndex] = useState(0)
  const [notice, setNotice] = useState<string | null>(null)

  const predicate = useMemo(() => episodeFilterPredicate(filters), [filters])
  const query: RecordsQuery = {
    sort: [],
    filter: predicate,
    limit: LIMITS.pageSize,
    ...(cursors[pageIndex] === undefined ? {} : { after: cursors[pageIndex] }),
  }
  const paramString = recordsQueryParams(query).toString()

  const read = useQuery({
    queryKey: ["learning", "episodes", bound.scopeKey, paramString],
    queryFn: ({ signal }) => listEpisodes(query, undefined, {
      ...(bound.scope === null ? {} : { scope: bound.scope }),
      signal,
      onScopeMismatch: bound.onScopeMismatch,
    }),
    placeholderData: keepPreviousData,
    retry: false,
  })

  const data = read.data

  useEffect(() => {
    if (data?.kind !== "restart") return
    void client.removeQueries({ queryKey: ["learning", "episodes"] })
    setCursors([undefined])
    setPageIndex(0)
    setNotice(DATA_CHANGED)
  }, [data, client])

  const applyFilters = (next: EpisodeFilters) => {
    setFilters(next)
    setCursors([undefined])
    setPageIndex(0)
    setNotice(null)
  }

  const clearAll = () => applyFilters({})

  const page = data?.kind === "page" ? data : undefined
  const echoed = useMemo(() => (page ? episodeFiltersFromServed(page.served) : null), [page])
  const hasFilters = predicate !== null
  const refresh = () => { setNotice(null); void client.invalidateQueries({ queryKey: ["learning", "episodes"] }) }

  const body = (() => {
    if (read.isPending) {
      return (
        <div className="grid gap-2" aria-label="Reading episodes">
          {[0, 1, 2, 3].map((i) => <Skeleton key={i} className="h-14 rounded-lg" />)}
        </div>
      )
    }
    if (read.isError) {
      return (
        <div role="alert" className="flex flex-wrap items-center gap-3 rounded-(--radius-well) border border-(--panel-border) bg-card px-3 py-2 text-body text-foreground">
          <Dot tone="bad"><span>{read.error instanceof Error ? read.error.message : "Episodes could not be read."}</span></Dot>
          <span className="flex-1" />
          <Button variant="outline" size="sm" onClick={() => void read.refetch()}>Retry</Button>
        </div>
      )
    }
    if (data?.kind === "no-cell") return <EmptyState icon={GitCompareArrows} title="No data cell">{NO_DATA_CELL}</EmptyState>
    if (data?.kind === "unavailable") return <EmptyState icon={GitCompareArrows} title="Not served yet">{EPISODES_UNAVAILABLE}</EmptyState>
    if (data?.kind === "refused") {
      return (
        <div role="alert" className="grid gap-1 rounded-(--radius-well) border border-(--panel-border) bg-card px-4 py-3 text-body text-foreground">
          <p className="flex items-center gap-2"><Dot tone="bad">{data.message}</Dot></p>
          {data.diagnostics.length > 0 && (
            <ul className="list-disc ps-5 text-meta">
              {data.diagnostics.map((d, index) => <li key={`${d.code}:${index}`}>{d.message}{d.pointer ? ` (${d.pointer})` : ""}</li>)}
            </ul>
          )}
        </div>
      )
    }
    if (page === undefined) return null
    if (page.rows.length === 0) {
      return (
        <EmptyState icon={GitCompareArrows} title={hasFilters ? "No matching episodes" : "No episodes yet"}
          next={hasFilters ? <Button type="button" variant="outline" size="sm" onClick={clearAll}>Clear filters</Button> : undefined}>
          {hasFilters ? "No decision episode matched the active server filters." : "When a governed decision is recorded, its episode projection appears here."}
        </EmptyState>
      )
    }
    return (
      <Panel icon={GitCompareArrows} label="Decision episodes" action={page.total ? totalText(page.total) : `${page.rows.length} shown`}>
        <TableCard>
          <Table aria-label="Decision episodes">
            <TableHeader>
              <TableRow>
                <TableHead>Decision</TableHead>
                <TableHead>Agreement</TableHead>
                <TableHead>Rules</TableHead>
                <TableHead>Outcome & standing</TableHead>
                <TableHead>Triage</TableHead>
                <TableHead className="text-end">Decided</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {page.rows.map((row) => {
                if (row.kind === "withheld") {
                  return (
                    <TableRow key={row.id} data-withheld="true">
                      <TableCell className="font-mono text-meta text-muted-foreground">{row.id}</TableCell>
                      <TableCell colSpan={5} className="text-muted-foreground">Withheld by cell policy</TableCell>
                    </TableRow>
                  )
                }
                if (row.kind === "malformed") {
                  return (
                    <TableRow key={row.id} data-malformed="true">
                      <TableCell className="font-mono text-meta">{row.id}</TableCell>
                      <TableCell colSpan={5} className="text-foreground">
                        <span className="inline-flex items-center gap-1.5"><Dot tone="bad">Malformed episode ({row.problem})</Dot></span>
                      </TableCell>
                    </TableRow>
                  )
                }
                const e = row.item
                const flow = e.run.flow.title ?? humanize(e.run.flow.ruleId.replace(/\.flow\.rule$/, ""))
                const step = e.run.step.label ?? humanize(e.run.step.id)
                const rules = rulesSummary(e)
                const outcomeIcon = e.outcome.state === "recorded" ? CheckCircle2 : e.outcome.state === "overdue" ? Clock : Hourglass
                return (
                  <TableRow key={e.id} data-episode-row={e.id}>
                    <TableCell className="max-w-md whitespace-normal">
                      <div className="grid gap-0.5 py-1">
                        <Link to="/episodes/$id" params={{ id: e.id }} className="text-foreground underline-offset-4 hover:underline">
                          {e.decision.question}
                        </Link>
                        <span className="text-meta text-muted-foreground">
                          {flow} · {step}{technical && <> · <code className="font-mono">{e.id}</code></>}
                        </span>
                      </div>
                    </TableCell>
                    <TableCell>
                      <Chip tone={AGREEMENT_TONE[e.agreement]}>{AGREEMENT_STYLE[e.agreement].short}</Chip>
                    </TableCell>
                    <TableCell>
                      <Chip tone={rules.tone} icon={rules.tone === "amber" || rules.tone === "red" ? ShieldAlert : undefined}>{rules.text}</Chip>
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-wrap items-center gap-1.5">
                        <Chip tone={OUTCOME_TONE[e.outcome.state]} icon={outcomeIcon}>
                          {e.outcome.state === "recorded" ? "Recorded" : e.outcome.state === "overdue" ? "Overdue" : "Open"}
                        </Chip>
                        <span className="text-meta text-muted-foreground">{e.standing === "measured" ? "Measured" : "Context"}</span>
                      </div>
                    </TableCell>
                    <TableCell>
                      {e.triage === null
                        ? <span className="inline-flex items-center gap-1 text-meta text-muted-foreground"><CircleDashed className="size-3" aria-hidden />Not triaged</span>
                        : <Chip tone={VERDICT_TONE[e.triage.verdict]}>{VERDICT_LABEL[e.triage.verdict]}</Chip>}
                    </TableCell>
                    <TableCell className="text-end text-muted-foreground">
                      <div>{e.decision.decidedBy}</div>
                      <div className="text-meta">{when(e.decision.recordedAtMs)}</div>
                    </TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </TableCard>
        <div className="flex flex-wrap items-center justify-between gap-2 border-t px-4 py-2.5 text-meta text-muted-foreground">
          <span>{page.quarantineCount > 0 ? `${page.quarantineCount} quarantined` : "Read-only ledger projection"}</span>
          <nav className="flex items-center gap-2" aria-label="Episodes pages">
            <Button type="button" variant="outline" size="sm" disabled={pageIndex === 0} onClick={() => setPageIndex(pageIndex - 1)}>Previous</Button>
            <span className="tabular-nums">Page {pageIndex + 1}</span>
            <Button type="button" variant="outline" size="sm" disabled={page.next === undefined || read.isFetching}
              onClick={() => { if (page.next === undefined) return; setCursors([...cursors.slice(0, pageIndex + 1), page.next]); setPageIndex(pageIndex + 1) }}>
              Next
            </Button>
          </nav>
        </div>
      </Panel>
    )
  })()

  return (
    <div className="grid max-w-6xl gap-5">
      <PageHeader
        title="Episodes"
        description="Every governed decision joined with its suggestion, rules, recorded outcome and triage verdict."
        meta={page?.total ? <span className="tabular-nums">{totalText(page.total)}</span> : undefined}
        actions={<Button type="button" variant="ghost" size="sm" onClick={refresh} disabled={read.isFetching}><RefreshCw className={cn(read.isFetching && "animate-spin")} /> Refresh</Button>}
      />

      {data?.kind !== "no-cell" && data?.kind !== "unavailable" && (
        <Panel icon={Filter} label="Server filters" action={hasFilters ? <Button type="button" variant="ghost" size="xs" onClick={clearAll}><X /> Clear filters</Button> : "Evaluated by the cell"}>
          <Well className="grid gap-3 p-4">
            {/*
              Backend follow-up (#1220 / #1208): `crates/norn-api/src/object_query.rs` (`is_name`)
              currently only admits single flat `[a-z][a-z0-9_]*` property names. Outcome state,
              triage verdict, law id, and producer manifest pin filters stay hidden here until the
              backend learning-pack lane exposes flat filterable properties or nested query grammar
              with route-produced fixtures.
            */}
            <div className="grid gap-3 sm:max-w-xs">
              <label className="grid gap-1 text-meta text-muted-foreground">
                <span>Agreement</span>
                <select aria-label="Filter by agreement" className={selectClass} value={filters.agreement ?? ""}
                  onChange={(e) => applyFilters({ ...filters, agreement: e.target.value === "" ? undefined : e.target.value as Agreement })}>
                  <option value="">All agreements</option>
                  {AGREEMENTS.map((a) => <option key={a} value={a}>{AGREEMENT_STYLE[a].short}</option>)}
                </select>
              </label>
            </div>

            {page !== undefined && page.served === null && (
              <p className="text-meta text-muted-foreground">{SERVER_QUERY_UNAVAILABLE}</p>
            )}
            {echoed !== null && echoed.agreement !== undefined && (
              <div className="flex flex-wrap items-center gap-1.5" aria-label="Applied server filters">
                <span className="text-meta text-muted-foreground">Applied by cell:</span>
                <Chip tone="blue">agreement: {echoed.agreement}</Chip>
              </div>
            )}
          </Well>
        </Panel>
      )}

      {notice && <p role="status" className="rounded-lg border bg-muted/50 px-3 py-2 text-body">{notice}</p>}
      {body}
    </div>
  )
}
