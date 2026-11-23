import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate, useSearch } from "@tanstack/react-router"
import { AlertTriangle, CheckCircle2, CircleDashed, Clock, Filter, GitCompareArrows, Hourglass, RefreshCw, ShieldAlert, X } from "lucide-react"
import { useEffect, useMemo, useState } from "react"
import { Chip, type Tone } from "@/components/chip"
import { EmptyState, PageHeader, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Skeleton } from "@/components/ui/skeleton"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { cn } from "@/lib/utils"
import { useView } from "@/lib/view"
import { humanize } from "../lib/cell-read.ts"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import {
  AGREEMENTS, EPISODES_UNAVAILABLE, OUTCOME_STATES, VERDICTS,
  episodeFilterPredicate, episodeFiltersFromServed, listEpisodes,
  type Agreement, type Episode, type EpisodeFilters, type OutcomeState, type Verdict,
} from "../lib/learning/episode.ts"
import { DATA_CHANGED, LIMITS, recordsQueryParams, SERVER_QUERY_UNAVAILABLE, totalText, type RecordsQuery } from "../lib/records-query.ts"
import { AGREEMENT_STYLE, VERDICT_LABEL } from "./episode-page.tsx"

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

const selectClass = "h-8 rounded-md border border-input bg-background px-2.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
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
 * All five filters (`agreement`, `outcome.state`, `ruleOutcomes.lawId`,
 * `recommendation.producer.manifestPin`, `triage.verdict` including "not triaged") are sent as
 * server query clauses to `GET /v1/objects/learning.Episode`. Studio never filters client-side
 * and reflects only what the server echoes back.
 */
export function EpisodesPage() {
  const { view } = useView()
  const technical = view === "technical"
  const client = useQueryClient()
  const navigate = useNavigate()
  const search = useSearch({ strict: false }) as { law?: unknown }
  const initialLaw = typeof search.law === "string" && search.law.trim() !== "" ? search.law.trim() : undefined

  const [filters, setFilters] = useState<EpisodeFilters>(() => (initialLaw ? { lawId: initialLaw } : {}))
  const [lawDraft, setLawDraft] = useState(initialLaw ?? "")
  const [pinDraft, setPinDraft] = useState("")
  const [cursors, setCursors] = useState<readonly (string | undefined)[]>([undefined])
  const [pageIndex, setPageIndex] = useState(0)
  const [notice, setNotice] = useState<string | null>(null)

  // Sync URL `?law=` changes (for example when arriving from the Misfit board).
  useEffect(() => {
    if (initialLaw !== undefined && initialLaw !== filters.lawId) {
      setFilters((current) => ({ ...current, lawId: initialLaw }))
      setLawDraft(initialLaw)
      setCursors([undefined])
      setPageIndex(0)
    }
  }, [initialLaw])

  const predicate = useMemo(() => episodeFilterPredicate(filters), [filters])
  const query: RecordsQuery = {
    sort: [],
    filter: predicate,
    limit: LIMITS.pageSize,
    ...(cursors[pageIndex] === undefined ? {} : { after: cursors[pageIndex] }),
  }
  const paramString = recordsQueryParams(query).toString()

  const read = useQuery({
    queryKey: ["learning", "episodes", paramString],
    queryFn: () => listEpisodes(query),
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
    if (next.lawId === undefined && initialLaw !== undefined) {
      void navigate({ to: "/learning/episodes", search: {} as never, replace: true })
    }
  }

  const clearAll = () => {
    setLawDraft("")
    setPinDraft("")
    applyFilters({})
  }

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
        <div role="alert" className="flex flex-wrap items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
          <span>{read.error instanceof Error ? read.error.message : "Episodes could not be read."}</span>
          <span className="flex-1" />
          <Button variant="outline" size="sm" onClick={() => void read.refetch()}>Retry</Button>
        </div>
      )
    }
    if (data?.kind === "no-cell") return <EmptyState icon={GitCompareArrows} title="No data cell">{NO_DATA_CELL}</EmptyState>
    if (data?.kind === "unavailable") return <EmptyState icon={GitCompareArrows} title="Not served yet">{EPISODES_UNAVAILABLE}</EmptyState>
    if (data?.kind === "refused") {
      return (
        <div role="alert" className="grid gap-1 rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive">
          <p className="font-medium">{data.message}</p>
          {data.diagnostics.length > 0 && (
            <ul className="list-disc pl-5 text-meta">
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
                <TableHead className="text-right">Decided</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {page.rows.map((row) => {
                if (row.kind === "withheld") {
                  return (
                    <TableRow key={row.id} data-withheld="true">
                      <TableCell className="font-mono text-xs text-muted-foreground">{row.id}</TableCell>
                      <TableCell colSpan={5} className="text-muted-foreground">Withheld by cell policy</TableCell>
                    </TableRow>
                  )
                }
                if (row.kind === "malformed") {
                  return (
                    <TableRow key={row.id} data-malformed="true">
                      <TableCell className="font-mono text-xs">{row.id}</TableCell>
                      <TableCell colSpan={5} className="text-destructive">
                        <span className="inline-flex items-center gap-1.5"><AlertTriangle className="size-3.5" aria-hidden />Malformed episode ({row.problem})</span>
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
                        <Link to="/episodes/$id" params={{ id: e.id }} className="font-medium text-foreground underline-offset-4 hover:underline">
                          {e.decision.question}
                        </Link>
                        <span className="text-xs text-muted-foreground">
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
                        <span className="text-xs text-muted-foreground">{e.standing === "measured" ? "Measured" : "Context"}</span>
                      </div>
                    </TableCell>
                    <TableCell>
                      {e.triage === null
                        ? <span className="inline-flex items-center gap-1 text-xs text-muted-foreground"><CircleDashed className="size-3" aria-hidden />Not triaged</span>
                        : <Chip tone={VERDICT_TONE[e.triage.verdict]}>{VERDICT_LABEL[e.triage.verdict]}</Chip>}
                    </TableCell>
                    <TableCell className="text-right text-muted-foreground">
                      <div>{e.decision.decidedBy}</div>
                      <div className="text-xs">{when(e.decision.recordedAtMs)}</div>
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
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
              <label className="grid gap-1 text-xs text-muted-foreground">
                <span>Agreement</span>
                <select aria-label="Filter by agreement" className={selectClass} value={filters.agreement ?? ""}
                  onChange={(e) => applyFilters({ ...filters, agreement: e.target.value === "" ? undefined : e.target.value as Agreement })}>
                  <option value="">All agreements</option>
                  {AGREEMENTS.map((a) => <option key={a} value={a}>{AGREEMENT_STYLE[a].short}</option>)}
                </select>
              </label>

              <label className="grid gap-1 text-xs text-muted-foreground">
                <span>Outcome</span>
                <select aria-label="Filter by outcome" className={selectClass} value={filters.outcomeState ?? ""}
                  onChange={(e) => applyFilters({ ...filters, outcomeState: e.target.value === "" ? undefined : e.target.value as OutcomeState })}>
                  <option value="">All outcomes</option>
                  {OUTCOME_STATES.map((s) => (
                    <option key={s} value={s}>{s === "recorded" ? "Recorded" : s === "open" ? "Not measured yet" : "Not measured in time"}</option>
                  ))}
                </select>
              </label>

              <label className="grid gap-1 text-xs text-muted-foreground">
                <span>Triage verdict</span>
                <select aria-label="Filter by triage verdict" className={selectClass} value={filters.verdict ?? ""}
                  onChange={(e) => applyFilters({ ...filters, verdict: e.target.value === "" ? undefined : e.target.value as Verdict | "untriaged" })}>
                  <option value="">All verdicts</option>
                  <option value="untriaged">Not triaged</option>
                  {VERDICTS.map((v) => <option key={v} value={v}>{VERDICT_LABEL[v]}</option>)}
                </select>
              </label>

              <form className="grid gap-1 text-xs text-muted-foreground" onSubmit={(e) => { e.preventDefault(); applyFilters({ ...filters, lawId: lawDraft.trim() || undefined }) }}>
                <label htmlFor="filter-law-id">Law id</label>
                <div className="flex gap-1">
                  <Input id="filter-law-id" className="h-8 text-sm" placeholder="e.g. demo.credit.hold_over_limit" value={lawDraft} onChange={(e) => setLawDraft(e.target.value)} />
                  <Button type="submit" variant="outline" size="sm" className="h-8 px-2.5">Apply</Button>
                </div>
              </form>

              <form className="grid gap-1 text-xs text-muted-foreground" onSubmit={(e) => { e.preventDefault(); applyFilters({ ...filters, manifestPin: pinDraft.trim() || undefined }) }}>
                <label htmlFor="filter-manifest-pin">Producer pin</label>
                <div className="flex gap-1">
                  <Input id="filter-manifest-pin" className="h-8 font-mono text-xs" placeholder="sha256:…" value={pinDraft} onChange={(e) => setPinDraft(e.target.value)} />
                  <Button type="submit" variant="outline" size="sm" className="h-8 px-2.5">Apply</Button>
                </div>
              </form>
            </div>

            {page !== undefined && page.served === null && (
              <p className="text-meta text-muted-foreground">{SERVER_QUERY_UNAVAILABLE}</p>
            )}
            {echoed !== null && Object.values(echoed).some((v) => v !== undefined) && (
              <div className="flex flex-wrap items-center gap-1.5" aria-label="Applied server filters">
                <span className="text-xs text-muted-foreground">Applied by cell:</span>
                {echoed.agreement && <Chip tone="blue">agreement: {echoed.agreement}</Chip>}
                {echoed.outcomeState && <Chip tone="blue">outcome: {echoed.outcomeState}</Chip>}
                {echoed.verdict && <Chip tone="blue">triage: {echoed.verdict === "untriaged" ? "not triaged" : echoed.verdict}</Chip>}
                {echoed.lawId && <Chip tone="blue" mono>law: {echoed.lawId}</Chip>}
                {echoed.manifestPin && <Chip tone="blue" mono>pin: {echoed.manifestPin.slice(0, 18)}…</Chip>}
              </div>
            )}
          </Well>
        </Panel>
      )}

      {notice && <p role="status" className="rounded-lg border bg-muted/50 px-3 py-2 text-sm">{notice}</p>}
      {body}
    </div>
  )
}
