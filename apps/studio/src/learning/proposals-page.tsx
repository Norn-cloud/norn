import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { AlertTriangle, ArrowLeft, CheckCircle2, FileDiff, GitCommitHorizontal, RefreshCw, Scale, ShieldAlert, ShieldCheck, ShieldX } from "lucide-react"
import { useEffect, useMemo, useState } from "react"
import { Chip, type Tone } from "@/components/system/chip"
import { CopyId } from "@/components/copy-id"
import { KpiTile, StatStrip } from "@/components/system/kpi-tile"
import { PageHeader } from "@/components/panel"
import { EmptyState, Panel, Well } from "@/components/system/panel"
import { TableCard } from "@/components/system/table-card"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { cn } from "@/lib/utils"
import { useView } from "@/lib/view"
import { ActionSheet, type OpenAction } from "../actions/action-sheet.tsx"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import {
  GAP_PROPOSAL_TYPE, PROPOSAL_STATUSES, PROPOSALS_UNAVAILABLE,
  listGapProposals, proposalFilterPredicate, proposalStatusFromServed, readGapProposal,
  type GapProposal, type ProposalReplay, type ProposalStatus, type ReplayCase, type ReplayOutcome,
} from "../lib/learning/proposals.ts"
import { DATA_CHANGED, LIMITS, recordsQueryParams, totalText, type RecordsQuery } from "../lib/records-query.ts"
import type { ObjectRecord } from "../lib/transport.ts"
import { LearningGate, useBoundLearningRead } from "./gate.tsx"
import { RecordActions, TypeActions } from "./record-actions.tsx"

const STATUS_STYLE: Readonly<Record<ProposalStatus, { readonly label: string; readonly tone: Tone }>> = {
  draft: { label: "Draft", tone: "gray" },
  submitted: { label: "Submitted", tone: "blue" },
  admitted: { label: "Admitted", tone: "green" },
  refused: { label: "Admission refused", tone: "red" },
  ratified: { label: "Ratified", tone: "violet" },
  withdrawn: { label: "Withdrawn", tone: "gray" },
}

const REPLAY_OUTCOME_STYLE: Readonly<Record<ReplayOutcome, { readonly label: string; readonly tone: Tone }>> = {
  allowed: { label: "Allowed", tone: "green" },
  refused: { label: "Would refuse", tone: "red" },
  would_block: { label: "Would block", tone: "amber" },
  waived: { label: "Waived", tone: "amber" },
}

const when = (ms: number) => new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })
const selectClass = "h-8 rounded-md border border-input bg-background px-2.5 text-body outline-none focus-visible:ring-2 focus-visible:ring-ring"

/**
 * `/learning/proposals`: server-driven list of `learning.GapProposal` records (#1220 L-5).
 * Gated behind S1 capability discovery (`learning.GapProposal` advertised by the scoped cell).
 */
export function ProposalsPage() {
  return (
    <LearningGate surface="proposals" icon={FileDiff} title="Proposals">
      <ProposalsPageContent />
    </LearningGate>
  )
}

function ProposalsPageContent() {
  const { view } = useView()
  const technical = view === "technical"
  const client = useQueryClient()
  const bound = useBoundLearningRead()
  const [status, setStatus] = useState<ProposalStatus | undefined>(undefined)
  const [cursors, setCursors] = useState<readonly (string | undefined)[]>([undefined])
  const [pageIndex, setPageIndex] = useState(0)
  const [notice, setNotice] = useState<string | null>(null)

  const predicate = useMemo(() => proposalFilterPredicate(status), [status])
  const query: RecordsQuery = {
    sort: [],
    filter: predicate,
    limit: LIMITS.pageSize,
    ...(cursors[pageIndex] === undefined ? {} : { after: cursors[pageIndex] }),
  }
  const paramString = recordsQueryParams(query).toString()

  const read = useQuery({
    queryKey: ["learning", "proposals", bound.scopeKey, paramString],
    queryFn: ({ signal }) => listGapProposals(query, undefined, {
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
    void client.removeQueries({ queryKey: ["learning", "proposals"] })
    setCursors([undefined])
    setPageIndex(0)
    setNotice(DATA_CHANGED)
  }, [data, client])

  const refresh = () => { setNotice(null); void client.invalidateQueries({ queryKey: ["learning", "proposals"] }) }
  const page = data?.kind === "page" ? data : undefined
  const echoedStatus = page ? proposalStatusFromServed(page.served) : undefined

  return (
    <div className="grid max-w-6xl gap-5">
      <PageHeader
        title="Proposals"
        description="Definition diffs citing decision episodes, checked and replayed against curated cases before ratification."
        meta={page?.total ? <span className="tabular-nums">{totalText(page.total)}</span> : undefined}
        actions={
          <Button type="button" variant="ghost" size="sm" onClick={refresh} disabled={read.isFetching}>
            <RefreshCw className={cn(read.isFetching && "animate-spin")} /> Refresh
          </Button>
        }
        primary={<TypeActions objectKind={GAP_PROPOSAL_TYPE} primary onChanged={refresh} />}
      />

      {data?.kind !== "no-cell" && data?.kind !== "unavailable" && (
        <div className="flex flex-wrap items-center gap-3">
          <label className="flex items-center gap-2 text-meta text-muted-foreground">
            <span>Status</span>
            <select aria-label="Filter proposals by status" className={selectClass} value={status ?? ""}
              onChange={(e) => { setStatus(e.target.value === "" ? undefined : e.target.value as ProposalStatus); setCursors([undefined]); setPageIndex(0) }}>
              <option value="">All statuses</option>
              {PROPOSAL_STATUSES.map((s) => <option key={s} value={s}>{STATUS_STYLE[s].label}</option>)}
            </select>
          </label>
          {echoedStatus && <Chip tone="blue">Applied by cell: {STATUS_STYLE[echoedStatus].label}</Chip>}
        </div>
      )}

      {notice && <p role="status" className="rounded-lg border bg-muted/50 px-3 py-2 text-body">{notice}</p>}

      {read.isPending ? (
        <div className="grid gap-2" aria-label="Reading proposals">{[0, 1, 2].map((i) => <Skeleton key={i} className="h-14 rounded-lg" />)}</div>
      ) : read.isError ? (
        <div role="alert" className="flex items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive">
          <span>{read.error instanceof Error ? read.error.message : "Proposals could not be read."}</span>
          <span className="flex-1" />
          <Button variant="outline" size="sm" onClick={() => void read.refetch()}>Retry</Button>
        </div>
      ) : data?.kind === "scope-changed" ? null : data?.kind === "no-cell" ? (
        <EmptyState icon={FileDiff} title="No data cell">{NO_DATA_CELL}</EmptyState>
      ) : data?.kind === "unavailable" ? (
        <EmptyState icon={FileDiff} title="Not served yet">{PROPOSALS_UNAVAILABLE}</EmptyState>
      ) : data?.kind === "refused" ? (
        <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-3 text-body text-destructive">{data.message}</p>
      ) : page === undefined || page.rows.length === 0 ? (
        <EmptyState icon={FileDiff} title={status ? "No matching proposals" : "No gap proposals yet"}
          next={status ? <Button type="button" variant="outline" size="sm" onClick={() => setStatus(undefined)}>Show all</Button> : <TypeActions objectKind={GAP_PROPOSAL_TYPE} onChanged={refresh} />}>
          {status ? "No gap proposal matched this status filter." : "Draft a gap proposal from the Misfit board or here to change a definition with replay evidence."}
        </EmptyState>
      ) : (
        <Panel icon={FileDiff} label="Gap proposals" action={page.total ? totalText(page.total) : `${page.rows.length} shown`}>
          <TableCard>
            <Table aria-label="Gap proposals">
              <TableHeader>
                <TableRow>
                  <TableHead>Proposal</TableHead>
                  <TableHead>Target</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Replay impact</TableHead>
                  <TableHead className="text-end">Episodes</TableHead>
                  <TableHead className="text-end">Updated</TableHead>
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
                        <TableCell colSpan={5} className="text-destructive">
                          <span className="inline-flex items-center gap-1.5"><AlertTriangle className="size-3.5" aria-hidden />Malformed proposal ({row.problem})</span>
                        </TableCell>
                      </TableRow>
                    )
                  }
                  const p = row.item
                  const st = STATUS_STYLE[p.status]
                  return (
                    <TableRow key={p.id} data-proposal-row={p.id}>
                      <TableCell className="max-w-md whitespace-normal">
                        <div className="grid gap-0.5 py-1">
                          <Link to="/learning/proposals/$id" params={{ id: p.id }} className="text-foreground underline-offset-4 hover:underline">
                            {p.title}
                          </Link>
                          <span className="text-meta text-muted-foreground">{p.diff.summary}</span>
                        </div>
                      </TableCell>
                      <TableCell>
                        <div>{p.target.label ?? p.target.artifactId}</div>
                        {technical && <code className="font-mono text-meta text-muted-foreground">{p.target.artifactId}</code>}
                      </TableCell>
                      <TableCell><Chip tone={st.tone}>{st.label}</Chip></TableCell>
                      <TableCell>
                        {p.replay === null ? (
                          <span className="text-meta text-muted-foreground">Not replayed yet</span>
                        ) : (
                          <div className="flex flex-wrap items-center gap-1.5">
                            <Chip tone={p.replay.flippedCount > 0 ? "amber" : "green"}>{p.replay.flippedCount} flipped</Chip>
                            {p.replay.protectedFlippedCount > 0 && (
                              <Chip tone="red" icon={ShieldX}>{p.replay.protectedFlippedCount} protected</Chip>
                            )}
                          </div>
                        )}
                      </TableCell>
                      <TableCell className="text-end tabular-nums text-muted-foreground">{p.episodeIds.length}</TableCell>
                      <TableCell className="text-end text-muted-foreground">
                        <div>{p.author}</div>
                        <div className="text-meta">{when(p.updatedAtMs)}</div>
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </TableCard>
          <div className="flex items-center justify-end gap-2 border-t px-4 py-2.5 text-meta text-muted-foreground">
            <Button type="button" variant="outline" size="sm" disabled={pageIndex === 0} onClick={() => setPageIndex(pageIndex - 1)}>Previous</Button>
            <span className="tabular-nums">Page {pageIndex + 1}</span>
            <Button type="button" variant="outline" size="sm" disabled={page.next === undefined || read.isFetching}
              onClick={() => { if (page.next === undefined) return; setCursors([...cursors.slice(0, pageIndex + 1), page.next]); setPageIndex(pageIndex + 1) }}>
              Next
            </Button>
          </div>
        </Panel>
      )}
    </div>
  )
}

/**
 * `/learning/proposals/$id`: proposal review and replay table island (#1220 L-5).
 *
 * Gated behind S1 capability discovery (`learning.GapProposal` advertised by the scoped cell).
 * Shows the proposed definition diff, cited episodes, checker admission diagnostics, the replay
 * table of curated cases (highlighting past cases a tightened limit would have refused and any
 * flipped protected case that refuses admission), the Ruling receipt when ratified, and only the
 * governed actions the cell offers for this proposal in its current state.
 */
export function ProposalDetailPage() {
  const { id = "" } = useParams({ strict: false })
  return (
    <LearningGate surface="proposals" icon={FileDiff} title="Proposal">
      <ProposalDetailPageContent key={id} id={id} />
    </LearningGate>
  )
}

function ProposalDetailPageContent({ id }: { readonly id: string }) {
  const client = useQueryClient()
  const bound = useBoundLearningRead()
  const [acting, setActing] = useState<OpenAction | null>(null)
  const refresh = () => {
    void client.invalidateQueries({ queryKey: ["learning", "proposal"] })
    void client.invalidateQueries({ queryKey: ["learning", "proposals"] })
    void client.invalidateQueries({ queryKey: ["actions", GAP_PROPOSAL_TYPE, id] })
  }
  return (
    <>
      <ProposalDetail id={id} bound={bound} onOpen={setActing} />
      <ActionSheet open={acting} onClose={() => setActing(null)} onChanged={refresh} scopeBinding={bound} />
    </>
  )
}

function ProposalDetail({ id, bound, onOpen }: {
  readonly id: string
  readonly bound: ReturnType<typeof useBoundLearningRead>
  readonly onOpen: (action: OpenAction) => void
}) {
  const read = useQuery({
    queryKey: ["learning", "proposal", bound.scopeKey, id],
    queryFn: ({ signal }) => readGapProposal(id, undefined, {
      ...(bound.scope === null ? {} : { scope: bound.scope }),
      signal,
      onScopeMismatch: bound.onScopeMismatch,
    }),
    retry: false,
  })
  const result = read.data

  if (read.isPending) {
    return (
      <div className="mx-auto grid max-w-5xl gap-3" aria-label="Reading the proposal">
        <Skeleton className="h-8 w-80" />
        <Skeleton className="h-28 rounded-lg" />
        <Skeleton className="h-48 rounded-lg" />
      </div>
    )
  }
  if (read.isError) {
    return (
      <div role="alert" className="mx-auto flex max-w-5xl items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive">
        <span>{read.error instanceof Error ? read.error.message : "The proposal could not be read."}</span>
        <span className="flex-1" />
        <Button variant="outline" size="sm" onClick={() => void read.refetch()}>Retry</Button>
      </div>
    )
  }
  switch (result?.kind) {
    case "no-cell": return <EmptyState icon={FileDiff} title="No data cell">{NO_DATA_CELL}</EmptyState>
    case "unavailable": return <EmptyState icon={FileDiff} title="Not served yet">{PROPOSALS_UNAVAILABLE}</EmptyState>
    case "not-found": return <EmptyState icon={FileDiff} title="No such proposal">{result.message}</EmptyState>
    case "refused": return <EmptyState icon={FileDiff} title="Can't read this proposal">{result.message}</EmptyState>
    case "malformed": return <EmptyState icon={FileDiff} title="Can't show this proposal">The cell sent a proposal Studio can't read ({result.problem}). Nothing is guessed.</EmptyState>
    case "ok": return <ProposalReview proposal={result.item} record={result.record} onOpen={onOpen} />
    default: return null
  }
}

function ProposalReview({ proposal: p, record, onOpen }: {
  readonly proposal: GapProposal
  readonly record: ObjectRecord
  readonly onOpen: (action: OpenAction) => void
}) {
  const { view } = useView()
  const technical = view === "technical"
  const st = STATUS_STYLE[p.status]

  return (
    <div className="mx-auto grid max-w-5xl gap-5">
      <PageHeader
        title={p.title}
        description={p.summary}
        meta={
          <>
            <Chip tone={st.tone}>{st.label}</Chip>
            <Chip tone="gray" mono={technical}>{p.target.label ?? p.target.artifactId}</Chip>
          </>
        }
        actions={<Button asChild variant="ghost" size="sm"><Link to="/learning/proposals"><ArrowLeft /> All proposals</Link></Button>}
      />

      <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_18rem]">
        <div className="grid content-start gap-5">
          {/* ---- Definition diff ---- */}
          <Panel icon={FileDiff} label="Proposed definition diff" action={p.target.kind}>
            <Well className="grid gap-3 p-4">
              <p className="text-body">{p.diff.summary}</p>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="rounded-md border bg-muted/40 p-3">
                  <div className="text-meta text-muted-foreground">Current definition</div>
                  <div className="mt-1 font-mono text-body">{p.diff.before}</div>
                </div>
                <div className="rounded-md border border-tone-amber/30 bg-tone-amber/5 p-3">
                  <div className="text-meta text-muted-foreground">Proposed definition</div>
                  <div className="mt-1 font-mono text-body">{p.diff.after}</div>
                </div>
              </div>
              <pre className="overflow-x-auto rounded-md border bg-muted/30 p-3 font-mono text-meta leading-relaxed">{p.diff.patch}</pre>
            </Well>
          </Panel>

          {/* ---- Replay table island ---- */}
          <ReplayIsland replay={p.replay} technical={technical} />

          {/* ---- Checker admission & diagnostics ---- */}
          <Panel icon={Scale} label="Checker admission" action={
            <Chip tone={p.checker.status === "admitted" ? "green" : p.checker.status === "refused" ? "red" : "gray"}>
              {p.checker.status === "admitted" ? "Admitted" : p.checker.status === "refused" ? "Refused" : "Not run yet"}
            </Chip>
          }>
            <Well className="grid gap-2 p-4 text-body">
              {p.checker.diagnostics.length === 0 ? (
                <p className="text-muted-foreground">
                  {p.checker.status === "not_run" ? "Submit this proposal to run the checker and curated-case replay." : "No checker diagnostics."}
                </p>
              ) : (
                <ul className="grid gap-2" aria-label="Checker diagnostics">
                  {p.checker.diagnostics.map((d, index) => (
                    <li key={`${d.code}:${index}`} className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive">
                      <span>{d.message}</span>
                      {technical && <code className="ms-2 font-mono text-meta">{d.code}</code>}
                    </li>
                  ))}
                </ul>
              )}
            </Well>
          </Panel>
        </div>

        {/* ---- Sidebar: Actions, Ruling receipt, Cited episodes ---- */}
        <aside aria-label="Proposal details" className="grid content-start gap-3">
          <Panel label="Review actions">
            <Well className="grid gap-3 p-4">
              <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-meta">
                <dt className="text-muted-foreground">Author</dt><dd>{p.author}</dd>
                <dt className="text-muted-foreground">Updated</dt><dd>{when(p.updatedAtMs)}</dd>
                {technical && <><dt className="text-muted-foreground">Target</dt><dd className="min-w-0 truncate font-mono text-meta">{p.target.artifactId}</dd></>}
                {p.supersedesProposalId && (
                  <>
                    <dt className="text-muted-foreground">Supersedes</dt>
                    <dd><Link to="/learning/proposals/$id" params={{ id: p.supersedesProposalId }} className="font-mono text-meta underline-offset-4 hover:underline">{p.supersedesProposalId}</Link></dd>
                  </>
                )}
              </dl>
              <div className="border-t pt-3">
                <RecordActions
                  objectKind={GAP_PROPOSAL_TYPE}
                  record={record}
                  ariaLabel="Proposal actions"
                  emptyText="The cell offers you no actions on this proposal."
                  onOpen={onOpen}
                />
              </div>
            </Well>
          </Panel>

          {p.ruling !== null && (
            <Panel icon={CheckCircle2} label="Ruling">
              <Well className="grid gap-2 p-4 text-body">
                <dl className="grid gap-1.5">
                  <div>
                    <dt className="text-meta text-muted-foreground">Ratified by</dt>
                    <dd>{p.ruling.ratifiedBy}, {when(p.ruling.ratifiedAtMs)}</dd>
                  </div>
                  <div>
                    <dt className="text-meta text-muted-foreground">Receipt{technical ? ` · commit ${p.ruling.commitSeq}` : ""}</dt>
                    <dd><CopyId value={p.ruling.receiptHash} short={14} /></dd>
                  </div>
                  {p.ruling.supersededCaseIds.length > 0 && (
                    <div>
                      <dt className="text-meta text-muted-foreground">Superseded protected cases</dt>
                      <dd className="flex flex-wrap gap-1 pt-1">
                        {p.ruling.supersededCaseIds.map((caseId) => <Chip key={caseId} tone="violet" mono>{caseId}</Chip>)}
                      </dd>
                    </div>
                  )}
                </dl>
              </Well>
            </Panel>
          )}

          <Panel icon={GitCommitHorizontal} label="Cited episodes" action={`${p.episodeIds.length}`}>
            <Well className="p-4">
              {p.episodeIds.length === 0 ? (
                <p className="text-meta text-muted-foreground">No episodes cited.</p>
              ) : (
                <ul className="grid gap-1.5 text-body" aria-label="Cited episodes">
                  {p.episodeIds.map((epId) => (
                    <li key={epId}>
                      <Link to="/episodes/$id" params={{ id: epId }} className="font-mono text-meta underline-offset-4 hover:underline">
                        {epId}
                      </Link>
                    </li>
                  ))}
                </ul>
              )}
            </Well>
          </Panel>
        </aside>
      </div>
    </div>
  )
}

function ReplayIsland({ replay, technical }: { readonly replay: ProposalReplay | null; readonly technical: boolean }) {
  const [filter, setFilter] = useState<"all" | "flipped" | "protected">("all")

  if (replay === null) {
    return (
      <Panel icon={ShieldAlert} label="Curated-case replay">
        <Well>
          <EmptyState icon={ShieldAlert} title="Not replayed yet">
            Submit this proposal so the cell checks admission and replays curated cases against the proposed definition.
          </EmptyState>
        </Well>
      </Panel>
    )
  }

  // Sort flipped protected cases first, then other flipped cases, then unchanged cases.
  const ordered = [...replay.cases].sort((a, b) => {
    const rank = (c: ReplayCase) => (c.flipped && c.protected && !c.supersededByRuling ? 0 : c.flipped && c.protected ? 1 : c.flipped ? 2 : 3)
    return rank(a) - rank(b) || a.caseId.localeCompare(b.caseId)
  })
  const visible = ordered.filter((c) => {
    if (filter === "flipped") return c.flipped
    if (filter === "protected") return c.flipped && c.protected
    return true
  })

  return (
    <Panel
      icon={replay.admissionStatus === "refused" ? ShieldX : ShieldCheck}
      label="Curated-case replay"
      action={`${replay.evalSetName} v${replay.evalSetVersion}`}
    >
      <Well className="grid gap-4 p-4">
        {replay.admissionStatus === "refused" && (
          <div role="alert" data-replay-refusal="true" className="grid gap-1 rounded-lg border border-destructive/40 bg-destructive/5 px-4 py-3 text-body text-destructive">
            <p className="flex items-center gap-2">
              <ShieldX className="size-4 shrink-0" aria-hidden />
              Admission refused by replay
            </p>
            <p className="text-foreground/90">
              {replay.refusalReason ?? `${replay.protectedFlippedCount} protected ${replay.protectedFlippedCount === 1 ? "case" : "cases"} would change outcome under the proposed definition.`}
            </p>
          </div>
        )}

        <StatStrip>
          <KpiTile label="Replayed cases" value={replay.totalCases} sub={`${replay.evalSetName} v${replay.evalSetVersion}`} />
          <KpiTile label="Unchanged" value={replay.totalCases - replay.flippedCount} sub="Same outcome" />
          <KpiTile label="Would change" value={replay.flippedCount} sub="Outcome flips" />
          <KpiTile label="Protected flipped" value={replay.protectedFlippedCount} sub={replay.protectedFlippedCount > 0 ? "Blocks admission" : "None flipped"} />
        </StatStrip>

        <div className="flex flex-wrap items-center justify-between gap-2">
          <ToggleGroup type="single" value={filter} onValueChange={(next) => { if (next === "all" || next === "flipped" || next === "protected") setFilter(next) }} aria-label="Filter replayed cases">
            <ToggleGroupItem value="all" size="sm">All ({replay.cases.length})</ToggleGroupItem>
            <ToggleGroupItem value="flipped" size="sm">Flipped ({replay.flippedCount})</ToggleGroupItem>
            <ToggleGroupItem value="protected" size="sm">Protected flipped ({replay.protectedFlippedCount})</ToggleGroupItem>
          </ToggleGroup>
          {technical && <span className="font-mono text-meta text-muted-foreground">pin {replay.definitionPin.slice(0, 18)}…</span>}
        </div>
      </Well>

      {visible.length === 0 ? (
        <Well><EmptyState icon={ShieldCheck} title="No cases in this view">No replayed cases match this filter.</EmptyState></Well>
      ) : (
        <TableCard>
          <Table aria-label="Replayed cases">
            <TableHeader>
              <TableRow>
                <TableHead>Case</TableHead>
                <TableHead>Protection</TableHead>
                <TableHead>Before</TableHead>
                <TableHead>Under proposal</TableHead>
                <TableHead>Impact</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {visible.map((c) => {
                const before = REPLAY_OUTCOME_STYLE[c.beforeOutcome]
                const after = REPLAY_OUTCOME_STYLE[c.afterOutcome]
                const blocking = c.flipped && c.protected && !c.supersededByRuling
                return (
                  <TableRow key={c.caseId} data-replay-case={c.caseId} data-flipped={c.flipped ? "true" : "false"} data-protected-flip={blocking ? "true" : undefined}
                    className={cn(blocking && "bg-destructive/5")}>
                    <TableCell className="max-w-xs whitespace-normal">
                      <div className="grid gap-0.5 py-1">
                        <span>{c.label}</span>
                        <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
                          <Link to="/episodes/$id" params={{ id: c.episodeId }} className="font-mono underline-offset-4 hover:underline">{c.episodeId}</Link>
                          {technical && <code className="font-mono">{c.caseId}</code>}
                        </div>
                        {c.detail && <span className="text-meta text-muted-foreground">{c.detail}</span>}
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-wrap items-center gap-1">
                        {c.protected ? <Chip tone="violet" icon={ShieldCheck}>Protected</Chip> : <Chip tone="gray">Standard</Chip>}
                        {c.supersededByRuling && <Chip tone="blue">Superseded</Chip>}
                      </div>
                    </TableCell>
                    <TableCell><Chip tone={before.tone}>{before.label}</Chip></TableCell>
                    <TableCell><Chip tone={after.tone}>{after.label}</Chip></TableCell>
                    <TableCell>
                      {blocking
                        ? <Chip tone="red" icon={ShieldX}>Blocks admission</Chip>
                        : c.flipped
                          ? <Chip tone="amber" icon={ShieldAlert}>Flipped</Chip>
                          : <Chip tone="gray">Unchanged</Chip>}
                    </TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </TableCard>
      )}
    </Panel>
  )
}
