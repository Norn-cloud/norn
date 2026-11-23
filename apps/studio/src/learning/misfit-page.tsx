import { useQuery, useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { BarChart3, ExternalLink, FileDiff, RefreshCw, Scale, TrendingUp } from "lucide-react"
import { useState } from "react"
import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts"
import { ActionSheet, useActions, type OpenAction } from "../actions/action-sheet.tsx"
import { RefusedNote } from "../answer-view.tsx"
import { Chip } from "@/components/chip"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { EmptyState, PageHeader, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Button } from "@/components/ui/button"
import { ChartContainer, ChartLegend, ChartLegendContent, ChartTooltip, ChartTooltipContent, type ChartConfig } from "@/components/ui/chart"
import { Skeleton } from "@/components/ui/skeleton"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { cn } from "@/lib/utils"
import { useView } from "@/lib/view"
import { SUPPRESSED } from "../lib/answer.ts"
import { humanize } from "../lib/cell-read.ts"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import { FRESHNESS, stamp } from "../lib/format.ts"
import { writesWhole } from "../lib/actions.ts"
import { VERDICTS, type Verdict } from "../lib/learning/episode.ts"
import {
  MISFIT_UNAVAILABLE, TRIAGE_TREND_UNAVAILABLE, readMisfitByLaw, readTriageOverTime,
  type AnswerMeta, type RateFigure,
} from "../lib/learning/misfit.ts"
import { GAP_PROPOSAL_TYPE } from "../lib/learning/proposals.ts"
import { useCatalog } from "../lib/use-catalog.ts"
import { VERDICT_LABEL } from "./episode-page.tsx"

const MISFIT_CHART_CONFIG: ChartConfig = {
  overrideRate: { label: "Override rate", color: "var(--tone-amber)" },
  exceptionRate: { label: "Exception rate", color: "var(--tone-blue)" },
}

const TRIAGE_CHART_CONFIG: ChartConfig = {
  exception: { label: VERDICT_LABEL.exception, color: "var(--tone-blue)" },
  noise: { label: VERDICT_LABEL.noise, color: "var(--tone-gray)" },
  pattern: { label: VERDICT_LABEL.pattern, color: "var(--tone-green)" },
  gap: { label: VERDICT_LABEL.gap, color: "var(--tone-amber)" },
}

function MetaChips({ meta }: { readonly meta: AnswerMeta }) {
  const freshness = FRESHNESS[meta.freshness]
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-xs">
      <Chip tone={meta.freshness === "exact" || meta.freshness === "fresh" ? "green" : "amber"} dot title={freshness.meaning}>{freshness.label}</Chip>
      {!meta.complete && <Chip tone="amber">Incomplete coverage</Chip>}
      {meta.exclusionCount > 0 && <Chip tone="amber">{meta.exclusionCount} quarantined excluded</Chip>}
      <span className="text-muted-foreground tabular-nums">commit {meta.knownAsOf} · {stamp(meta.nowMs)}</span>
    </div>
  )
}

function RateCell({ figure }: { readonly figure: RateFigure | null }) {
  if (figure === null) return <span className="text-muted-foreground">—</span>
  if (figure.suppressed) return <Chip tone="gray" title={SUPPRESSED}>Suppressed</Chip>
  if (figure.percent === null) return <span className="text-muted-foreground">—</span>
  return (
    <span className="inline-flex items-baseline justify-end gap-2 tabular-nums">
      <span className="font-medium">{figure.percent}</span>
      {figure.numerator !== null && figure.denominator !== null && (
        <span className="text-xs text-muted-foreground">({figure.numerator}/{figure.denominator})</span>
      )}
    </span>
  )
}

/**
 * `/learning/misfit`: the Misfit board and learning charts island (#1220 L-4).
 *
 * Reads the two pack questions through the generic question route:
 * - `learning.misfit_by_law`: override and exception rate per law over a window;
 * - `learning.triage_over_time`: verdict counts per week.
 *
 * Links every law to `/learning/episodes?law=<lawId>` and offers drafting a gap proposal only when
 * the cell's action catalog serves a create action for `learning.GapProposal`.
 */
export function MisfitPage() {
  const { view } = useView()
  const technical = view === "technical"
  const client = useQueryClient()
  const { catalog } = useCatalog()
  const [acting, setActing] = useState<OpenAction | null>(null)

  const misfitRead = useQuery({ queryKey: ["learning", "misfit-by-law"], queryFn: () => readMisfitByLaw(), retry: false })
  const triageRead = useQuery({ queryKey: ["learning", "triage-over-time"], queryFn: () => readTriageOverTime(), retry: false })
  const proposalCatalog = useActions(GAP_PROPOSAL_TYPE)
  const draftAction = proposalCatalog.data?.kind === "catalog"
    ? proposalCatalog.data.items.find((a) => a.actionId === "learning.draft_gap_proposal") ?? proposalCatalog.data.items.find(writesWhole)
    : undefined

  const lawTitle = (lawId: string): string => {
    const found = catalog?.laws.find((l) => l.id === lawId)
    return found?.title ?? humanize(lawId)
  }

  const refresh = () => {
    void client.invalidateQueries({ queryKey: ["learning", "misfit-by-law"] })
    void client.invalidateQueries({ queryKey: ["learning", "triage-over-time"] })
  }

  const misfit = misfitRead.data?.kind === "ok" ? misfitRead.data.data : undefined
  const triage = triageRead.data?.kind === "ok" ? triageRead.data.data : undefined

  const topOverride = misfit?.rows.find((r) => r.override !== null && !r.override.suppressed && r.override.percent !== null)
  const topException = misfit?.rows
    .filter((r) => r.exception !== null && !r.exception.suppressed && r.exception.plotPercent !== null)
    .sort((a, b) => (b.exception?.plotPercent ?? -1) - (a.exception?.plotPercent ?? -1))[0]
  const totalTriaged = triage && !triage.weeks.some((w) => w.anySuppressed)
    ? triage.weeks.reduce((sum, w) => sum + (w.total ?? 0), 0)
    : null

  const misfitChartData = (misfit?.rows ?? [])
    .filter((r) => (r.override?.plotPercent ?? null) !== null || (r.exception?.plotPercent ?? null) !== null)
    .map((r) => ({
      lawId: r.lawId,
      law: lawTitle(r.lawId),
      overrideRate: r.override?.plotPercent ?? null,
      exceptionRate: r.exception?.plotPercent ?? null,
      overrideExact: r.override?.percent ?? "—",
      exceptionExact: r.exception?.percent ?? "—",
    }))

  const triageChartData = (triage?.weeks ?? []).map((w) => ({
    week: w.label,
    weekStart: w.weekStart,
    exception: w.counts.exception.plot,
    noise: w.counts.noise.plot,
    pattern: w.counts.pattern.plot,
    gap: w.counts.gap.plot,
  }))

  return (
    <div className="grid max-w-6xl gap-5">
      <PageHeader
        title="Misfit board"
        description="Override and exception rates per law alongside weekly triage verdicts, answered directly from the learning pack's questions."
        actions={
          <Button type="button" variant="ghost" size="sm" onClick={refresh} disabled={misfitRead.isFetching || triageRead.isFetching}>
            <RefreshCw className={cn((misfitRead.isFetching || triageRead.isFetching) && "animate-spin")} /> Refresh
          </Button>
        }
      />

      {(misfit !== undefined || triage !== undefined) && (
        <StatStrip>
          <KpiTile icon={Scale} label="Laws observed" value={misfit ? misfit.rows.length : "—"} sub="From learning.misfit_by_law" />
          <KpiTile icon={BarChart3} label="Highest override rate" value={topOverride?.override?.percent ?? "—"} sub={topOverride ? lawTitle(topOverride.lawId) : "No unsuppressed law"} />
          <KpiTile icon={BarChart3} label="Highest exception rate" value={topException?.exception?.percent ?? "—"} sub={topException ? lawTitle(topException.lawId) : "No unsuppressed law"} />
          <KpiTile icon={TrendingUp} label="Triaged in window" value={totalTriaged === null ? (triage ? "Partial" : "—") : totalTriaged} sub={triage ? `${triage.weeks.length} weeks` : "From learning.triage_over_time"} />
        </StatStrip>
      )}

      {/* ---- Misfit by law: chart + ranked board ---- */}
      <Panel icon={Scale} label="Misfit by law" action={misfit ? <MetaChips meta={misfit.meta} /> : "learning.misfit_by_law"}>
        {misfitRead.isPending ? (
          <Well className="grid gap-3 p-4" aria-label="Reading misfit by law"><Skeleton className="h-56 rounded-lg" /><Skeleton className="h-28 rounded-lg" /></Well>
        ) : misfitRead.isError ? (
          <Well className="p-4">
            <div role="alert" className="flex items-center gap-3 text-sm text-destructive">
              <span>{misfitRead.error instanceof Error ? misfitRead.error.message : "Could not read learning.misfit_by_law."}</span>
              <span className="flex-1" />
              <Button variant="outline" size="sm" onClick={() => void misfitRead.refetch()}>Retry</Button>
            </div>
          </Well>
        ) : misfitRead.data?.kind === "no-cell" ? (
          <Well><EmptyState icon={Scale} title="No data cell">{NO_DATA_CELL}</EmptyState></Well>
        ) : misfitRead.data?.kind === "unavailable" ? (
          <Well><EmptyState icon={Scale} title="Not served yet">{MISFIT_UNAVAILABLE}</EmptyState></Well>
        ) : misfitRead.data?.kind === "refused" ? (
          <Well className="p-4"><RefusedNote refusal={misfitRead.data.refusal} /></Well>
        ) : misfitRead.data?.kind === "malformed" ? (
          <Well><EmptyState icon={Scale} title="Can't read misfit answer">The cell sent a misfit answer Studio can't decode ({misfitRead.data.problem}).</EmptyState></Well>
        ) : misfit === undefined || misfit.rows.length === 0 ? (
          <Well><EmptyState icon={Scale} title="No misfit signal in this window">No episodes matched any law in the active window.</EmptyState></Well>
        ) : (
          <>
            {misfitChartData.length > 0 && (
              <Well className="p-4">
                <div className="mb-2 text-meta text-muted-foreground">Override and exception rates per law (%)</div>
                <ChartContainer config={MISFIT_CHART_CONFIG} className="aspect-auto h-64 w-full" aria-label="Misfit by law chart">
                  <BarChart data={misfitChartData} margin={{ left: 0, right: 12, top: 8, bottom: 8 }}>
                    <CartesianGrid vertical={false} stroke="var(--border)" />
                    <XAxis dataKey="law" tickLine={false} axisLine={false} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} />
                    <YAxis domain={[0, 100]} tickLine={false} axisLine={false} width={44} tickFormatter={(v: number) => `${v}%`} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} />
                    <ChartTooltip content={<ChartTooltipContent formatter={(value, name) => (
                      <div className="flex w-full justify-between gap-4">
                        <span className="text-muted-foreground">{name === "overrideRate" ? "Override rate" : "Exception rate"}</span>
                        <span className="font-mono tabular-nums">{String(value)}%</span>
                      </div>
                    )} />} />
                    <ChartLegend content={<ChartLegendContent />} />
                    <Bar dataKey="overrideRate" fill="var(--color-overrideRate)" radius={[4, 4, 0, 0]} isAnimationActive={false} />
                    <Bar dataKey="exceptionRate" fill="var(--color-exceptionRate)" radius={[4, 4, 0, 0]} isAnimationActive={false} />
                  </BarChart>
                </ChartContainer>
              </Well>
            )}
            <TableCard>
              <Table aria-label="Misfit by law">
                <TableHeader>
                  <TableRow>
                    <TableHead>Law</TableHead>
                    <TableHead className="text-right">Override rate</TableHead>
                    <TableHead className="text-right">Exception rate</TableHead>
                    <TableHead className="text-right">Episodes</TableHead>
                    <TableHead className="text-right">Actions</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {misfit.rows.map((row) => (
                    <TableRow key={row.lawId} data-misfit-law={row.lawId}>
                      <TableCell className="whitespace-normal">
                        <div className="font-medium">{lawTitle(row.lawId)}</div>
                        {technical && <code className="font-mono text-xs text-muted-foreground">{row.lawId}</code>}
                      </TableCell>
                      <TableCell className="text-right"><RateCell figure={row.override} /></TableCell>
                      <TableCell className="text-right"><RateCell figure={row.exception} /></TableCell>
                      <TableCell className="text-right tabular-nums text-muted-foreground">
                        {row.episodeCount === null ? "—" : row.episodeCount}
                      </TableCell>
                      <TableCell className="text-right">
                        <div className="flex items-center justify-end gap-1.5">
                          <Button asChild type="button" variant="ghost" size="xs">
                            <Link to="/learning/episodes" search={{ law: row.lawId } as never}>
                              <ExternalLink className="size-3" /> Episodes
                            </Link>
                          </Button>
                          {draftAction !== undefined && (
                            <Button type="button" variant="outline" size="xs"
                              onClick={() => setActing({ action: draftAction, prefill: { target: row.lawId } })}>
                              <FileDiff className="size-3" /> {draftAction.title}
                            </Button>
                          )}
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </TableCard>
          </>
        )}
      </Panel>

      {/* ---- Triage over time: stacked chart + weekly exact table ---- */}
      <Panel icon={TrendingUp} label="Triage over time" action={triage ? <MetaChips meta={triage.meta} /> : "learning.triage_over_time"}>
        {triageRead.isPending ? (
          <Well className="grid gap-3 p-4" aria-label="Reading triage over time"><Skeleton className="h-56 rounded-lg" /><Skeleton className="h-24 rounded-lg" /></Well>
        ) : triageRead.isError ? (
          <Well className="p-4">
            <div role="alert" className="flex items-center gap-3 text-sm text-destructive">
              <span>{triageRead.error instanceof Error ? triageRead.error.message : "Could not read learning.triage_over_time."}</span>
              <span className="flex-1" />
              <Button variant="outline" size="sm" onClick={() => void triageRead.refetch()}>Retry</Button>
            </div>
          </Well>
        ) : triageRead.data?.kind === "no-cell" ? (
          <Well><EmptyState icon={TrendingUp} title="No data cell">{NO_DATA_CELL}</EmptyState></Well>
        ) : triageRead.data?.kind === "unavailable" ? (
          <Well><EmptyState icon={TrendingUp} title="Not served yet">{TRIAGE_TREND_UNAVAILABLE}</EmptyState></Well>
        ) : triageRead.data?.kind === "refused" ? (
          <Well className="p-4"><RefusedNote refusal={triageRead.data.refusal} /></Well>
        ) : triageRead.data?.kind === "malformed" ? (
          <Well><EmptyState icon={TrendingUp} title="Can't read triage trend">The cell sent a triage trend Studio can't decode ({triageRead.data.problem}).</EmptyState></Well>
        ) : triage === undefined || triage.weeks.length === 0 ? (
          <Well><EmptyState icon={TrendingUp} title="No triage verdicts in this window">Verdict counts per week appear here once episodes are triaged.</EmptyState></Well>
        ) : (
          <>
            <Well className="p-4">
              <div className="mb-2 text-meta text-muted-foreground">Verdict counts per week</div>
              <ChartContainer config={TRIAGE_CHART_CONFIG} className="aspect-auto h-64 w-full" aria-label="Triage verdicts over time chart">
                <BarChart data={triageChartData} margin={{ left: 0, right: 12, top: 8, bottom: 8 }}>
                  <CartesianGrid vertical={false} stroke="var(--border)" />
                  <XAxis dataKey="week" tickLine={false} axisLine={false} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} />
                  <YAxis allowDecimals={false} tickLine={false} axisLine={false} width={36} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} />
                  <ChartTooltip content={<ChartTooltipContent />} />
                  <ChartLegend content={<ChartLegendContent />} />
                  {VERDICTS.map((v: Verdict) => (
                    <Bar key={v} dataKey={v} stackId="verdicts" fill={`var(--color-${v})`} isAnimationActive={false} />
                  ))}
                </BarChart>
              </ChartContainer>
            </Well>
            <TableCard>
              <Table aria-label="Triage verdicts per week">
                <TableHeader>
                  <TableRow>
                    <TableHead>Week</TableHead>
                    {VERDICTS.map((v) => <TableHead key={v} className="text-right">{VERDICT_LABEL[v]}</TableHead>)}
                    <TableHead className="text-right">Total</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {triage.weeks.map((w) => (
                    <TableRow key={w.weekStart} data-triage-week={w.weekStart}>
                      <TableCell className="font-medium">{w.label}</TableCell>
                      {VERDICTS.map((v) => (
                        <TableCell key={v} className="text-right tabular-nums">
                          {w.counts[v].suppressed ? <Chip tone="gray" title={SUPPRESSED}>Suppressed</Chip> : (w.counts[v].exact ?? "0")}
                        </TableCell>
                      ))}
                      <TableCell className="text-right font-medium tabular-nums">
                        {w.total === null ? <Chip tone="gray" title="One or more verdict buckets in this week were suppressed.">Partial</Chip> : w.total}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </TableCard>
          </>
        )}
      </Panel>

      <ActionSheet open={acting} onClose={() => setActing(null)} onChanged={() => void client.invalidateQueries({ queryKey: ["learning", "proposals"] })} />
    </div>
  )
}
