import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { AlertTriangle, BookMarked, FolderLock, Layers, Lock, LockOpen, RefreshCw, ShieldCheck } from "lucide-react"
import { useEffect, useState } from "react"
import { ActionSheet, type OpenAction } from "../actions/action-sheet.tsx"
import { Chip, type Tone } from "@/components/chip"
import { CopyId } from "@/components/copy-id"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { EmptyState, PageHeader, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Button } from "@/components/ui/button"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { Skeleton } from "@/components/ui/skeleton"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { cn } from "@/lib/utils"
import { useView } from "@/lib/view"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import {
  CASES_UNAVAILABLE, CURATED_CASE_TYPE, EVAL_SET_TYPE, EVAL_SETS_UNAVAILABLE,
  listCuratedCases, listEvalSets, type CuratedCase, type EvalSet,
} from "../lib/learning/cases.ts"
import type { Agreement } from "../lib/learning/episode.ts"
import type { ObjectListRow } from "../lib/learning/objects.ts"
import { DATA_CHANGED, LIMITS, recordsQueryParams, totalText, type RecordsQuery } from "../lib/records-query.ts"
import type { ObjectRecord } from "../lib/transport.ts"
import { AGREEMENT_STYLE } from "./episode-page.tsx"
import { LearningGate } from "./gate.tsx"
import { RecordActions, TypeActions } from "./record-actions.tsx"

const AGREEMENT_TONE: Readonly<Record<Agreement, Tone>> = {
  accepted: "green",
  edited: "violet",
  overridden: "amber",
  none: "gray",
}

const when = (ms: number) => new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })

/**
 * `/learning/cases`: curated cases and eval sets (#1220 L-3).
 *
 * Gated behind S1 capability discovery (`learning.CuratedCase` + `learning.EvalSet`).
 * Both tabs follow the server cursor pagination workflow (`LIMITS.pageSize`, `after`, `next`,
 * `object.snapshot_moved` reload). Open action and receipt state (`ActionSheet`) is held at the
 * page root independently of live record details so:
 * - when a fresh refetch withholds, omits, or deletes a case or eval set, stale details and
 *   offers clear immediately (and eval sets never silently jump to another set);
 * - the open `ActionSheet` keeps its receipt and server-offered Undo mounted even if the record
 *   was uncurated or the last eval set was deleted.
 */
export function CasesPage() {
  return (
    <LearningGate surface="cases" icon={BookMarked} title="Cases & eval sets">
      <CasesPageContent />
    </LearningGate>
  )
}

function rowIdOf<T extends { readonly id: string }>(row: ObjectListRow<T>): string {
  return row.kind === "ok" ? row.item.id : row.id
}

function CasesPageContent() {
  const { view } = useView()
  const technical = view === "technical"
  const client = useQueryClient()
  const [tab, setTab] = useState<"cases" | "eval-sets">("cases")
  // Keep only the selected ids — never stale record snapshots (PR #1631 S3).
  const [openedCaseId, setOpenedCaseId] = useState<string | null>(null)
  const [selectedSetId, setSelectedSetId] = useState<string | null>(null)
  // Action/receipt/Undo sheet lives at page root so refetches, withheld rows, and deleting the last
  // eval set never unmount the receipt or drop a server-offered Undo.
  const [acting, setActing] = useState<OpenAction | null>(null)

  // Cursor pagination state for Curated cases (PR #1631 S4).
  const [casesCursors, setCasesCursors] = useState<readonly (string | undefined)[]>([undefined])
  const [casesPageIndex, setCasesPageIndex] = useState(0)
  const [casesNotice, setCasesNotice] = useState<string | null>(null)
  const casesQuery: RecordsQuery = {
    sort: [],
    filter: null,
    limit: LIMITS.pageSize,
    ...(casesCursors[casesPageIndex] === undefined ? {} : { after: casesCursors[casesPageIndex] }),
  }
  const casesParamString = recordsQueryParams(casesQuery).toString()

  // Cursor pagination state for Eval sets (PR #1631 S4).
  const [setsCursors, setSetsCursors] = useState<readonly (string | undefined)[]>([undefined])
  const [setsPageIndex, setSetsPageIndex] = useState(0)
  const [setsNotice, setSetsNotice] = useState<string | null>(null)
  const setsQuery: RecordsQuery = {
    sort: [],
    filter: null,
    limit: LIMITS.pageSize,
    ...(setsCursors[setsPageIndex] === undefined ? {} : { after: setsCursors[setsPageIndex] }),
  }
  const setsParamString = recordsQueryParams(setsQuery).toString()

  const casesRead = useQuery({
    queryKey: ["learning", "cases", casesParamString],
    queryFn: () => listCuratedCases(casesQuery),
    placeholderData: keepPreviousData,
    retry: false,
  })
  const setsRead = useQuery({
    queryKey: ["learning", "eval-sets", setsParamString],
    queryFn: () => listEvalSets(setsQuery),
    placeholderData: keepPreviousData,
    retry: false,
  })

  useEffect(() => {
    if (casesRead.data?.kind !== "restart") return
    void client.removeQueries({ queryKey: ["learning", "cases"] })
    setCasesCursors([undefined])
    setCasesPageIndex(0)
    setOpenedCaseId(null)
    setCasesNotice(DATA_CHANGED)
  }, [casesRead.data, client])

  useEffect(() => {
    if (setsRead.data?.kind !== "restart") return
    void client.removeQueries({ queryKey: ["learning", "eval-sets"] })
    setSetsCursors([undefined])
    setSetsPageIndex(0)
    setSelectedSetId(null)
    setSetsNotice(DATA_CHANGED)
  }, [setsRead.data, client])

  const refreshAll = () => {
    setCasesNotice(null)
    setSetsNotice(null)
    void client.invalidateQueries({ queryKey: ["learning", "cases"] })
    void client.invalidateQueries({ queryKey: ["learning", "eval-sets"] })
    void client.invalidateQueries({ queryKey: ["actions", CURATED_CASE_TYPE] })
    void client.invalidateQueries({ queryKey: ["actions", EVAL_SET_TYPE] })
  }

  const casesPage = casesRead.isError ? undefined : (casesRead.data?.kind === "page" ? casesRead.data : undefined)
  const setsPage = setsRead.isError ? undefined : (setsRead.data?.kind === "page" ? setsRead.data : undefined)

  const caseEntries = (casesPage?.rows ?? []).filter(
    (r): r is { readonly kind: "ok"; readonly item: CuratedCase; readonly record: ObjectRecord } => r.kind === "ok",
  )
  const setEntries = (setsPage?.rows ?? []).filter(
    (r): r is { readonly kind: "ok"; readonly item: EvalSet; readonly record: ObjectRecord } => r.kind === "ok",
  )

  // Initial default selection only when nothing has been selected yet on the current fresh page.
  // Once selected, deleting/withholding that set NEVER silently switches selection to another set.
  useEffect(() => {
    if (selectedSetId === null && !setsRead.isPlaceholderData && setEntries.length > 0) {
      setSelectedSetId(setEntries[0]!.item.id)
    }
  }, [selectedSetId, setsRead.isPlaceholderData, setEntries])

  // Resolve the opened case and selected eval set strictly from the CURRENT fresh page.
  // If a refetch withholds, omits, or fails, stale details and offers disappear immediately.
  const liveCaseRow: ObjectListRow<CuratedCase> | null = openedCaseId === null
    ? null
    : (casesPage?.rows.find((r) => rowIdOf(r) === openedCaseId) ?? null)
  const liveSetRow: ObjectListRow<EvalSet> | null = selectedSetId === null
    ? null
    : (setsPage?.rows.find((r) => rowIdOf(r) === selectedSetId) ?? null)

  const protectedCount = caseEntries.filter((c) => c.item.protected).length
  const sealedCount = setEntries.filter((s) => s.item.status === "sealed").length
  const casesPaged = casesPageIndex > 0 || casesPage?.next !== undefined
  const setsPaged = setsPageIndex > 0 || setsPage?.next !== undefined

  return (
    <div className="grid max-w-6xl gap-5">
      <PageHeader
        title="Cases & eval sets"
        description="Measured episodes pinned as ledger references for replay and evaluation. Sealed sets evolve by versioning."
        actions={
          <Button type="button" variant="ghost" size="sm" onClick={refreshAll} disabled={casesRead.isFetching || setsRead.isFetching}>
            <RefreshCw className={cn((casesRead.isFetching || setsRead.isFetching) && "animate-spin")} /> Refresh
          </Button>
        }
        primary={
          tab === "cases"
            ? <TypeActions objectKind={CURATED_CASE_TYPE} primary onOpen={setActing} />
            : <TypeActions objectKind={EVAL_SET_TYPE} primary onOpen={setActing} />
        }
      />

      {(casesPage !== undefined || setsPage !== undefined) && (
        <StatStrip>
          <KpiTile icon={BookMarked} label="Curated cases" value={casesPage?.total?.count ?? caseEntries.length} sub={casesPage?.total ? totalText(casesPage.total) : "Pinned ledger refs"} />
          <KpiTile icon={ShieldCheck} label="Protected cases" value={protectedCount} sub={casesPaged ? "On this page · refuse admission if flipped" : "Refuse proposal admission if flipped"} />
          <KpiTile icon={Layers} label="Eval sets" value={setsPage?.total?.count ?? setEntries.length} sub={setsPage?.total ? totalText(setsPage.total) : "Holdout & benchmark sets"} />
          <KpiTile icon={Lock} label="Sealed sets" value={sealedCount} sub={setsPaged ? "On this page · immutable pin" : "Append-only · immutable pin"} />
        </StatStrip>
      )}

      <Tabs value={tab} onValueChange={(next) => setTab(next as "cases" | "eval-sets")}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <TabsList>
            <TabsTrigger value="cases">Curated cases</TabsTrigger>
            <TabsTrigger value="eval-sets">Eval sets</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="cases" className="grid gap-3 pt-3">
          {casesNotice && <p role="status" className="rounded-lg border bg-muted/50 px-3 py-2 text-sm">{casesNotice}</p>}
          {casesRead.isPending ? (
            <div className="grid gap-2" aria-label="Reading curated cases">{[0, 1, 2].map((i) => <Skeleton key={i} className="h-14 rounded-lg" />)}</div>
          ) : casesRead.isError ? (
            <div role="alert" className="flex items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
              <span>{casesRead.error instanceof Error ? casesRead.error.message : "Curated cases could not be read."}</span>
              <span className="flex-1" />
              <Button variant="outline" size="sm" onClick={() => void casesRead.refetch()}>Retry</Button>
            </div>
          ) : casesRead.data?.kind === "no-cell" ? (
            <EmptyState icon={BookMarked} title="No data cell">{NO_DATA_CELL}</EmptyState>
          ) : casesRead.data?.kind === "unavailable" ? (
            <EmptyState icon={BookMarked} title="Not served yet">{CASES_UNAVAILABLE}</EmptyState>
          ) : casesRead.data?.kind === "refused" ? (
            <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive">{casesRead.data.message}</p>
          ) : casesPage === undefined || casesPage.rows.length === 0 ? (
            casesPageIndex > 0 ? (
              <EmptyState icon={BookMarked} title="No curated cases on this page"
                next={<Button type="button" variant="outline" size="sm" onClick={() => { setCasesPageIndex(casesPageIndex - 1); setOpenedCaseId(null) }}>Previous page</Button>}>
                This page has no curated cases. Return to the previous page to browse earlier cases.
              </EmptyState>
            ) : (
              <EmptyState icon={BookMarked} title="No curated cases yet" next={<TypeActions objectKind={CURATED_CASE_TYPE} onOpen={setActing} />}>
                Only episodes with a recorded outcome can be curated into reusable ledger-pinned cases.
              </EmptyState>
            )
          ) : (
            <Panel icon={BookMarked} label="Curated cases" action={casesPage.total ? totalText(casesPage.total) : `${caseEntries.length} cases`}>
              <TableCard>
                <Table aria-label="Curated cases">
                  <TableHeader>
                    <TableRow>
                      <TableHead>Case & episode</TableHead>
                      <TableHead>Agreement</TableHead>
                      <TableHead>Protection</TableHead>
                      <TableHead>Eval sets</TableHead>
                      <TableHead className="text-right">Curated</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {casesPage.rows.map((row) => {
                      if (row.kind === "withheld") {
                        return (
                          <TableRow key={row.id} data-withheld="true">
                            <TableCell className="font-mono text-xs text-muted-foreground">{row.id}</TableCell>
                            <TableCell colSpan={4} className="text-muted-foreground">Withheld by cell policy</TableCell>
                          </TableRow>
                        )
                      }
                      if (row.kind === "malformed") {
                        return (
                          <TableRow key={row.id} data-malformed="true">
                            <TableCell className="font-mono text-xs">{row.id}</TableCell>
                            <TableCell colSpan={4} className="text-destructive">
                              <span className="inline-flex items-center gap-1.5"><AlertTriangle className="size-3.5" aria-hidden />Malformed case ({row.problem})</span>
                            </TableCell>
                          </TableRow>
                        )
                      }
                      const c = row.item
                      return (
                        <TableRow key={c.id} data-case-row={c.id} className="cursor-pointer" onClick={() => setOpenedCaseId(c.id)}>
                          <TableCell className="max-w-md whitespace-normal">
                            <div className="grid gap-0.5 py-1">
                              <button type="button" className="text-start font-medium underline-offset-4 hover:underline"
                                onClick={(event) => { event.stopPropagation(); setOpenedCaseId(c.id) }}>
                                {c.question}
                              </button>
                              <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                                <Link to="/episodes/$id" params={{ id: c.episodeId }} className="underline-offset-4 hover:underline" onClick={(event) => event.stopPropagation()}>
                                  Episode {c.episodeId}
                                </Link>
                                {technical && <code className="font-mono">{c.id}</code>}
                              </div>
                            </div>
                          </TableCell>
                          <TableCell><Chip tone={AGREEMENT_TONE[c.agreement]}>{AGREEMENT_STYLE[c.agreement].short}</Chip></TableCell>
                          <TableCell>
                            {c.protected
                              ? <Chip tone="violet" icon={ShieldCheck}>Protected</Chip>
                              : <Chip tone="gray">Standard</Chip>}
                          </TableCell>
                          <TableCell className="text-muted-foreground tabular-nums">
                            {c.evalSetIds.length === 0 ? "None" : `${c.evalSetIds.length} ${c.evalSetIds.length === 1 ? "set" : "sets"}`}
                          </TableCell>
                          <TableCell className="text-right text-muted-foreground">
                            <div>{c.curatedBy}</div>
                            <div className="text-xs">{when(c.curatedAtMs)}</div>
                          </TableCell>
                        </TableRow>
                      )
                    })}
                  </TableBody>
                </Table>
              </TableCard>
              <div className="flex flex-wrap items-center justify-between gap-2 border-t px-4 py-2.5 text-meta text-muted-foreground">
                <span>{casesPage.quarantineCount > 0 ? `${casesPage.quarantineCount} quarantined` : "Pinned ledger references"}</span>
                <nav className="flex items-center gap-2" aria-label="Curated cases pages">
                  <Button type="button" variant="outline" size="sm" disabled={casesPageIndex === 0}
                    onClick={() => { setCasesPageIndex(casesPageIndex - 1); setOpenedCaseId(null) }}>
                    Previous
                  </Button>
                  <span className="tabular-nums">Page {casesPageIndex + 1}</span>
                  <Button type="button" variant="outline" size="sm" disabled={casesPage.next === undefined || casesRead.isFetching}
                    onClick={() => {
                      if (casesPage.next === undefined) return
                      setCasesCursors([...casesCursors.slice(0, casesPageIndex + 1), casesPage.next])
                      setCasesPageIndex(casesPageIndex + 1)
                      setOpenedCaseId(null)
                    }}>
                    Next
                  </Button>
                </nav>
              </div>
            </Panel>
          )}
        </TabsContent>

        <TabsContent value="eval-sets" className="grid gap-3 pt-3">
          {setsNotice && <p role="status" className="rounded-lg border bg-muted/50 px-3 py-2 text-sm">{setsNotice}</p>}
          {setsRead.isPending ? (
            <div className="grid gap-2" aria-label="Reading eval sets">{[0, 1, 2].map((i) => <Skeleton key={i} className="h-14 rounded-lg" />)}</div>
          ) : setsRead.isError ? (
            <div role="alert" className="flex items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
              <span>{setsRead.error instanceof Error ? setsRead.error.message : "Eval sets could not be read."}</span>
              <span className="flex-1" />
              <Button variant="outline" size="sm" onClick={() => void setsRead.refetch()}>Retry</Button>
            </div>
          ) : setsRead.data?.kind === "no-cell" ? (
            <EmptyState icon={FolderLock} title="No data cell">{NO_DATA_CELL}</EmptyState>
          ) : setsRead.data?.kind === "unavailable" ? (
            <EmptyState icon={FolderLock} title="Not served yet">{EVAL_SETS_UNAVAILABLE}</EmptyState>
          ) : setsRead.data?.kind === "refused" ? (
            <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive">{setsRead.data.message}</p>
          ) : setsPage === undefined || setsPage.rows.length === 0 ? (
            setsPageIndex > 0 ? (
              <EmptyState icon={FolderLock} title="No eval sets on this page"
                next={<Button type="button" variant="outline" size="sm" onClick={() => { setSetsPageIndex(setsPageIndex - 1); setSelectedSetId(null) }}>Previous page</Button>}>
                This page has no eval sets. Return to the previous page to browse earlier sets.
              </EmptyState>
            ) : (
              <EmptyState icon={FolderLock} title="No eval sets yet" next={<TypeActions objectKind={EVAL_SET_TYPE} onOpen={setActing} />}>
                Create an eval set to pin curated cases under a split key and seal it before scoring experiments or replaying proposals.
              </EmptyState>
            )
          ) : (
            <div className="grid gap-5 lg:grid-cols-[20rem_minmax(0,1fr)]">
              <Panel icon={Layers} label="Eval sets" action={setsPage.total ? totalText(setsPage.total) : `${setEntries.length}`}>
                <ul className="divide-y border-t" aria-label="Eval sets list">
                  {setsPage.rows.map((row) => {
                    if (row.kind === "withheld") {
                      return <li key={row.id} data-withheld="true" className="px-4 py-3 text-meta text-muted-foreground">{row.id} · Withheld</li>
                    }
                    if (row.kind === "malformed") {
                      return <li key={row.id} data-malformed="true" className="px-4 py-3 text-meta text-destructive">{row.id} · Malformed ({row.problem})</li>
                    }
                    const s = row.item
                    const active = selectedSetId === s.id
                    return (
                      <li key={s.id}>
                        <button type="button" data-eval-set={s.id} aria-current={active ? "true" : undefined}
                          onClick={() => setSelectedSetId(s.id)}
                          className={cn("grid w-full gap-1 px-4 py-3 text-start transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset", active && "bg-muted")}>
                          <span className="flex items-center justify-between gap-2">
                            <span className="truncate text-sm font-medium">{s.name}</span>
                            <Chip tone={s.status === "sealed" ? "violet" : "amber"} icon={s.status === "sealed" ? Lock : LockOpen}>
                              {s.status === "sealed" ? "Sealed" : "Unsealed"}
                            </Chip>
                          </span>
                          <span className="flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground tabular-nums">
                            <span>v{s.setVersion}</span>
                            <span>·</span>
                            <span>split: {s.splitKey}</span>
                            <span>·</span>
                            <span>{s.caseRefs.length} {s.caseRefs.length === 1 ? "case" : "cases"}</span>
                          </span>
                        </button>
                      </li>
                    )
                  })}
                </ul>
                <div className="flex items-center justify-between gap-2 border-t px-3 py-2 text-meta text-muted-foreground">
                  <nav className="flex w-full items-center justify-between gap-2" aria-label="Eval sets pages">
                    <Button type="button" variant="outline" size="sm" disabled={setsPageIndex === 0}
                      onClick={() => { setSetsPageIndex(setsPageIndex - 1); setSelectedSetId(null) }}>
                      Previous
                    </Button>
                    <span className="tabular-nums">Page {setsPageIndex + 1}</span>
                    <Button type="button" variant="outline" size="sm" disabled={setsPage.next === undefined || setsRead.isFetching}
                      onClick={() => {
                        if (setsPage.next === undefined) return
                        setSetsCursors([...setsCursors.slice(0, setsPageIndex + 1), setsPage.next])
                        setSetsPageIndex(setsPageIndex + 1)
                        setSelectedSetId(null)
                      }}>
                      Next
                    </Button>
                  </nav>
                </div>
              </Panel>

              {selectedSetId !== null && (
                liveSetRow?.kind === "ok" ? (
                  <div className="grid content-start gap-4" data-selected-eval-set={liveSetRow.item.id}>
                    <Panel
                      icon={liveSetRow.item.status === "sealed" ? Lock : LockOpen}
                      label={<span className="flex flex-wrap items-center gap-2"><span>{liveSetRow.item.name}</span><Chip tone="gray" mono>v{liveSetRow.item.setVersion}</Chip><Chip tone={liveSetRow.item.status === "sealed" ? "violet" : "amber"}>{liveSetRow.item.status === "sealed" ? "Sealed" : "Unsealed"}</Chip></span>}
                      action={`Split: ${liveSetRow.item.splitKey}`}
                    >
                      <Well className="grid gap-4 p-4">
                        <dl className="grid grid-cols-[minmax(8rem,auto)_1fr] gap-x-4 gap-y-1.5 text-sm">
                          <dt className="text-muted-foreground">Created</dt>
                          <dd>{liveSetRow.item.createdBy}, {when(liveSetRow.item.createdAtMs)}</dd>
                          {liveSetRow.item.sealedAtMs !== null && liveSetRow.item.sealedBy !== null && (
                            <>
                              <dt className="text-muted-foreground">Sealed</dt>
                              <dd>{liveSetRow.item.sealedBy}, {when(liveSetRow.item.sealedAtMs)}</dd>
                            </>
                          )}
                          {liveSetRow.item.parentSetId && (
                            <>
                              <dt className="text-muted-foreground">Derived from</dt>
                              <dd className="font-mono text-xs">{liveSetRow.item.parentSetId}</dd>
                            </>
                          )}
                          {technical && (
                            <>
                              <dt className="text-muted-foreground">Set id</dt>
                              <dd><CopyId value={liveSetRow.item.id} /></dd>
                            </>
                          )}
                        </dl>

                        <div className="border-t pt-3">
                          <RecordActions
                            objectKind={EVAL_SET_TYPE}
                            record={liveSetRow.record}
                            ariaLabel="Eval set actions"
                            emptyText="The cell offers you no actions on this eval set."
                            onOpen={(open) => { setSelectedSetId(liveSetRow.item.id); setActing(open) }}
                          />
                        </div>
                      </Well>
                    </Panel>

                    <Panel icon={BookMarked} label="Pinned case references" action={`${liveSetRow.item.caseRefs.length} pinned`}>
                      {liveSetRow.item.caseRefs.length === 0 ? (
                        <Well><EmptyState icon={BookMarked} title="No cases pinned yet">Use an offered action above to pin curated cases into this eval set.</EmptyState></Well>
                      ) : (
                        <TableCard>
                          <Table aria-label="Pinned case references">
                            <TableHeader>
                              <TableRow>
                                <TableHead>Case</TableHead>
                                <TableHead>Episode</TableHead>
                                <TableHead>Protection</TableHead>
                                <TableHead>Ledger receipt pin</TableHead>
                              </TableRow>
                            </TableHeader>
                            <TableBody>
                              {liveSetRow.item.caseRefs.map((ref) => (
                                <TableRow key={ref.caseId}>
                                  <TableCell className="font-medium">
                                    <div>{ref.label ?? ref.caseId}</div>
                                    {technical && <code className="font-mono text-xs text-muted-foreground">{ref.caseId}</code>}
                                  </TableCell>
                                  <TableCell>
                                    <Link to="/episodes/$id" params={{ id: ref.episodeId }} className="font-mono text-xs underline-offset-4 hover:underline">
                                      {ref.episodeId}
                                    </Link>
                                  </TableCell>
                                  <TableCell>
                                    {ref.protected ? <Chip tone="violet" icon={ShieldCheck}>Protected</Chip> : <Chip tone="gray">Standard</Chip>}
                                  </TableCell>
                                  <TableCell><CopyId value={ref.receiptHash} short={14} /></TableCell>
                                </TableRow>
                              ))}
                            </TableBody>
                          </Table>
                        </TableCard>
                      )}
                    </Panel>
                  </div>
                ) : liveSetRow?.kind === "withheld" ? (
                  <div className="grid content-start" data-eval-set-state="withheld">
                    <Panel icon={FolderLock} label="Eval set withheld">
                      <Well>
                        <EmptyState icon={FolderLock} title="Withheld by cell policy">
                          This eval set is withheld by cell policy. Previous details and actions have been cleared.
                        </EmptyState>
                      </Well>
                    </Panel>
                  </div>
                ) : liveSetRow?.kind === "malformed" ? (
                  <div className="grid content-start" data-eval-set-state="malformed">
                    <Panel icon={AlertTriangle} label="Eval set unreadable">
                      <Well>
                        <EmptyState icon={AlertTriangle} title="Can't show this eval set">
                          The cell sent an eval set Studio can't decode ({liveSetRow.problem}).
                        </EmptyState>
                      </Well>
                    </Panel>
                  </div>
                ) : (
                  <div className="grid content-start" data-eval-set-state="omitted">
                    <Panel icon={FolderLock} label="Eval set unavailable">
                      <Well>
                        <EmptyState icon={FolderLock} title="No longer available">
                          This eval set is no longer available on this page. Select another set from the list or use Undo if an action receipt offers one.
                        </EmptyState>
                      </Well>
                    </Panel>
                  </div>
                )
              )}
            </div>
          )}
        </TabsContent>
      </Tabs>

      <Sheet open={openedCaseId !== null} onOpenChange={(open) => { if (!open) setOpenedCaseId(null) }}>
        <SheetContent className="w-[min(34rem,100vw)] overflow-y-auto border-node-border shadow-float sm:max-w-lg">
          {openedCaseId !== null && (
            liveCaseRow?.kind === "ok" ? (
              <>
                <SheetHeader>
                  <SheetTitle>{liveCaseRow.item.question}</SheetTitle>
                  <SheetDescription className="flex flex-wrap items-center gap-2">
                    <span>Curated case</span>
                    <CopyId value={liveCaseRow.item.id} />
                  </SheetDescription>
                </SheetHeader>
                <div className="grid gap-4 px-4 pb-6 text-sm" data-case-sheet-state="ok">
                  <div className="flex flex-wrap items-center gap-2">
                    <Chip tone={AGREEMENT_TONE[liveCaseRow.item.agreement]}>{AGREEMENT_STYLE[liveCaseRow.item.agreement].label}</Chip>
                    {liveCaseRow.item.protected ? <Chip tone="violet" icon={ShieldCheck}>Protected</Chip> : <Chip tone="gray">Standard</Chip>}
                  </div>

                  <dl className="grid grid-cols-[minmax(8rem,auto)_1fr] gap-x-4 gap-y-2">
                    <dt className="text-muted-foreground">Episode</dt>
                    <dd>
                      <Link to="/episodes/$id" params={{ id: liveCaseRow.item.episodeId }} className="font-mono text-xs underline-offset-4 hover:underline">
                        {liveCaseRow.item.episodeId}
                      </Link>
                    </dd>
                    <dt className="text-muted-foreground">Expected choice</dt>
                    <dd className="font-medium">{liveCaseRow.item.expectedChoice}</dd>
                    <dt className="text-muted-foreground">Decision receipt</dt>
                    <dd><CopyId value={liveCaseRow.item.decisionReceipt.receiptHash} short={14} /> <span className="text-xs text-muted-foreground">commit {liveCaseRow.item.decisionReceipt.commitSeq}</span></dd>
                    <dt className="text-muted-foreground">Outcome receipt</dt>
                    <dd><CopyId value={liveCaseRow.item.outcomeReceipt.receiptHash} short={14} /> <span className="text-xs text-muted-foreground">commit {liveCaseRow.item.outcomeReceipt.commitSeq}</span></dd>
                    <dt className="text-muted-foreground">Curated</dt>
                    <dd>{liveCaseRow.item.curatedBy}, {when(liveCaseRow.item.curatedAtMs)}</dd>
                    {liveCaseRow.item.note && (
                      <>
                        <dt className="text-muted-foreground">Note</dt>
                        <dd className="text-muted-foreground">{liveCaseRow.item.note}</dd>
                      </>
                    )}
                  </dl>

                  <div className="border-t pt-3">
                    <h3 className="mb-2 text-sm font-medium">Case actions</h3>
                    <RecordActions
                      objectKind={CURATED_CASE_TYPE}
                      record={liveCaseRow.record}
                      ariaLabel="Case actions"
                      emptyText="The cell offers you no actions on this case."
                      onOpen={setActing}
                    />
                  </div>
                </div>
              </>
            ) : liveCaseRow?.kind === "withheld" ? (
              <>
                <SheetHeader>
                  <SheetTitle>Case withheld</SheetTitle>
                  <SheetDescription className="font-mono text-xs">{openedCaseId}</SheetDescription>
                </SheetHeader>
                <div className="px-4 pb-6" data-case-sheet-state="withheld">
                  <EmptyState icon={BookMarked} title="Withheld by cell policy">
                    This curated case is withheld by cell policy. Previous details and actions have been cleared.
                  </EmptyState>
                </div>
              </>
            ) : liveCaseRow?.kind === "malformed" ? (
              <>
                <SheetHeader>
                  <SheetTitle>Case unreadable</SheetTitle>
                  <SheetDescription className="font-mono text-xs">{openedCaseId}</SheetDescription>
                </SheetHeader>
                <div className="px-4 pb-6" data-case-sheet-state="malformed">
                  <EmptyState icon={AlertTriangle} title="Can't show this case">
                    The cell sent a case Studio can't decode ({liveCaseRow.problem}).
                  </EmptyState>
                </div>
              </>
            ) : (
              <>
                <SheetHeader>
                  <SheetTitle>Case unavailable</SheetTitle>
                  <SheetDescription className="font-mono text-xs">{openedCaseId}</SheetDescription>
                </SheetHeader>
                <div className="px-4 pb-6" data-case-sheet-state="omitted">
                  <EmptyState icon={BookMarked} title="No longer available">
                    This curated case is no longer available on this page. Previous details and actions have been cleared.
                  </EmptyState>
                </div>
              </>
            )
          )}
        </SheetContent>
      </Sheet>

      <ActionSheet open={acting} onClose={() => setActing(null)} onChanged={refreshAll} />
    </div>
  )
}
