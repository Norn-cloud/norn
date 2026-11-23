import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { AlertTriangle, BookMarked, FolderLock, Layers, Lock, LockOpen, RefreshCw, ShieldCheck } from "lucide-react"
import { useEffect, useState } from "react"
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
import { EMPTY_QUERY, totalText } from "../lib/records-query.ts"
import type { ObjectRecord } from "../lib/transport.ts"
import { AGREEMENT_STYLE } from "./episode-page.tsx"
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
 * Curated cases pin ledger references only (no copied truth); eval sets group pinned case
 * references under a split key and seal into immutable versions. Every verb (curate, protect,
 * uncurate, create/rename/delete eval set, add/remove case, seal, new version) is offered solely
 * when the cell's action catalog and per-record offers serve it.
 */
export function CasesPage() {
  const { view } = useView()
  const technical = view === "technical"
  const client = useQueryClient()
  const [tab, setTab] = useState<"cases" | "eval-sets">("cases")
  const [selectedCaseId, setSelectedCaseId] = useState<string | null>(null)
  const [selectedSetId, setSelectedSetId] = useState<string | null>(null)

  const casesRead = useQuery({
    queryKey: ["learning", "cases"],
    queryFn: () => listCuratedCases(EMPTY_QUERY),
    placeholderData: keepPreviousData,
    retry: false,
  })
  const setsRead = useQuery({
    queryKey: ["learning", "eval-sets"],
    queryFn: () => listEvalSets(EMPTY_QUERY),
    placeholderData: keepPreviousData,
    retry: false,
  })

  const refreshAll = () => {
    void client.invalidateQueries({ queryKey: ["learning", "cases"] })
    void client.invalidateQueries({ queryKey: ["learning", "eval-sets"] })
  }

  const casesPage = casesRead.data?.kind === "page" ? casesRead.data : undefined
  const setsPage = setsRead.data?.kind === "page" ? setsRead.data : undefined

  const caseEntries = (casesPage?.rows ?? []).filter(
    (r): r is { readonly kind: "ok"; readonly item: CuratedCase; readonly record: ObjectRecord } => r.kind === "ok",
  )
  const setEntries = (setsPage?.rows ?? []).filter(
    (r): r is { readonly kind: "ok"; readonly item: EvalSet; readonly record: ObjectRecord } => r.kind === "ok",
  )

  const selectedCase = selectedCaseId === null ? null : caseEntries.find((c) => c.item.id === selectedCaseId) ?? null
  const activeSet = setEntries.find((s) => s.item.id === selectedSetId) ?? setEntries[0] ?? null

  useEffect(() => {
    if (selectedSetId !== null && !setEntries.some((s) => s.item.id === selectedSetId)) {
      setSelectedSetId(setEntries[0]?.item.id ?? null)
    }
  }, [setEntries, selectedSetId])

  const protectedCount = caseEntries.filter((c) => c.item.protected).length
  const sealedCount = setEntries.filter((s) => s.item.status === "sealed").length

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
            ? <TypeActions objectKind={CURATED_CASE_TYPE} primary onChanged={refreshAll} />
            : <TypeActions objectKind={EVAL_SET_TYPE} primary onChanged={refreshAll} />
        }
      />

      {(casesPage !== undefined || setsPage !== undefined) && (
        <StatStrip>
          <KpiTile icon={BookMarked} label="Curated cases" value={casesPage?.total?.count ?? caseEntries.length} sub={casesPage?.total ? totalText(casesPage.total) : "Pinned ledger refs"} />
          <KpiTile icon={ShieldCheck} label="Protected cases" value={protectedCount} sub="Refuse proposal admission if flipped" />
          <KpiTile icon={Layers} label="Eval sets" value={setsPage?.total?.count ?? setEntries.length} sub={setsPage?.total ? totalText(setsPage.total) : "Holdout & benchmark sets"} />
          <KpiTile icon={Lock} label="Sealed sets" value={sealedCount} sub="Append-only · immutable pin" />
        </StatStrip>
      )}

      <Tabs value={tab} onValueChange={(next) => setTab(next as "cases" | "eval-sets")}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <TabsList>
            <TabsTrigger value="cases">Curated cases</TabsTrigger>
            <TabsTrigger value="eval-sets">Eval sets</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="cases" className="pt-3">
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
            <EmptyState icon={BookMarked} title="No curated cases yet" next={<TypeActions objectKind={CURATED_CASE_TYPE} onChanged={refreshAll} />}>
              Only episodes with a recorded outcome can be curated into reusable ledger-pinned cases.
            </EmptyState>
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
                        <TableRow key={c.id} data-case-row={c.id} className="cursor-pointer" onClick={() => setSelectedCaseId(c.id)}>
                          <TableCell className="max-w-md whitespace-normal">
                            <div className="grid gap-0.5 py-1">
                              <button type="button" className="text-start font-medium underline-offset-4 hover:underline"
                                onClick={(event) => { event.stopPropagation(); setSelectedCaseId(c.id) }}>
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
            </Panel>
          )}
        </TabsContent>

        <TabsContent value="eval-sets" className="pt-3">
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
            <EmptyState icon={FolderLock} title="No eval sets yet" next={<TypeActions objectKind={EVAL_SET_TYPE} onChanged={refreshAll} />}>
              Create an eval set to pin curated cases under a split key and seal it before scoring experiments or replaying proposals.
            </EmptyState>
          ) : (
            <div className="grid gap-5 lg:grid-cols-[20rem_minmax(0,1fr)]">
              <Panel icon={Layers} label="Eval sets" action={`${setEntries.length}`}>
                <ul className="divide-y border-t" aria-label="Eval sets list">
                  {setsPage.rows.map((row) => {
                    if (row.kind === "withheld") {
                      return <li key={row.id} className="px-4 py-3 text-meta text-muted-foreground">{row.id} · Withheld</li>
                    }
                    if (row.kind === "malformed") {
                      return <li key={row.id} className="px-4 py-3 text-meta text-destructive">{row.id} · Malformed ({row.problem})</li>
                    }
                    const s = row.item
                    const active = activeSet?.item.id === s.id
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
              </Panel>

              {activeSet && (
                <div className="grid content-start gap-4" data-selected-eval-set={activeSet.item.id}>
                  <Panel
                    icon={activeSet.item.status === "sealed" ? Lock : LockOpen}
                    label={<span className="flex flex-wrap items-center gap-2"><span>{activeSet.item.name}</span><Chip tone="gray" mono>v{activeSet.item.setVersion}</Chip><Chip tone={activeSet.item.status === "sealed" ? "violet" : "amber"}>{activeSet.item.status === "sealed" ? "Sealed" : "Unsealed"}</Chip></span>}
                    action={`Split: ${activeSet.item.splitKey}`}
                  >
                    <Well className="grid gap-4 p-4">
                      <dl className="grid grid-cols-[minmax(8rem,auto)_1fr] gap-x-4 gap-y-1.5 text-sm">
                        <dt className="text-muted-foreground">Created</dt>
                        <dd>{activeSet.item.createdBy}, {when(activeSet.item.createdAtMs)}</dd>
                        {activeSet.item.sealedAtMs !== null && activeSet.item.sealedBy !== null && (
                          <>
                            <dt className="text-muted-foreground">Sealed</dt>
                            <dd>{activeSet.item.sealedBy}, {when(activeSet.item.sealedAtMs)}</dd>
                          </>
                        )}
                        {activeSet.item.parentSetId && (
                          <>
                            <dt className="text-muted-foreground">Derived from</dt>
                            <dd className="font-mono text-xs">{activeSet.item.parentSetId}</dd>
                          </>
                        )}
                        {technical && (
                          <>
                            <dt className="text-muted-foreground">Set id</dt>
                            <dd><CopyId value={activeSet.item.id} /></dd>
                          </>
                        )}
                      </dl>

                      <div className="border-t pt-3">
                        <RecordActions
                          objectKind={EVAL_SET_TYPE}
                          record={activeSet.record}
                          ariaLabel="Eval set actions"
                          emptyText="The cell offers you no actions on this eval set."
                          onChanged={refreshAll}
                        />
                      </div>
                    </Well>
                  </Panel>

                  <Panel icon={BookMarked} label="Pinned case references" action={`${activeSet.item.caseRefs.length} pinned`}>
                    {activeSet.item.caseRefs.length === 0 ? (
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
                            {activeSet.item.caseRefs.map((ref) => (
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
              )}
            </div>
          )}
        </TabsContent>
      </Tabs>

      <Sheet open={selectedCase !== null} onOpenChange={(open) => { if (!open) setSelectedCaseId(null) }}>
        <SheetContent className="w-[min(34rem,100vw)] overflow-y-auto border-node-border shadow-float sm:max-w-lg">
          {selectedCase && (
            <>
              <SheetHeader>
                <SheetTitle>{selectedCase.item.question}</SheetTitle>
                <SheetDescription className="flex flex-wrap items-center gap-2">
                  <span>Curated case</span>
                  <CopyId value={selectedCase.item.id} />
                </SheetDescription>
              </SheetHeader>
              <div className="grid gap-4 px-4 pb-6 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <Chip tone={AGREEMENT_TONE[selectedCase.item.agreement]}>{AGREEMENT_STYLE[selectedCase.item.agreement].label}</Chip>
                  {selectedCase.item.protected ? <Chip tone="violet" icon={ShieldCheck}>Protected</Chip> : <Chip tone="gray">Standard</Chip>}
                </div>

                <dl className="grid grid-cols-[minmax(8rem,auto)_1fr] gap-x-4 gap-y-2">
                  <dt className="text-muted-foreground">Episode</dt>
                  <dd>
                    <Link to="/episodes/$id" params={{ id: selectedCase.item.episodeId }} className="font-mono text-xs underline-offset-4 hover:underline">
                      {selectedCase.item.episodeId}
                    </Link>
                  </dd>
                  <dt className="text-muted-foreground">Expected choice</dt>
                  <dd className="font-medium">{selectedCase.item.expectedChoice}</dd>
                  <dt className="text-muted-foreground">Decision receipt</dt>
                  <dd><CopyId value={selectedCase.item.decisionReceipt.receiptHash} short={14} /> <span className="text-xs text-muted-foreground">commit {selectedCase.item.decisionReceipt.commitSeq}</span></dd>
                  <dt className="text-muted-foreground">Outcome receipt</dt>
                  <dd><CopyId value={selectedCase.item.outcomeReceipt.receiptHash} short={14} /> <span className="text-xs text-muted-foreground">commit {selectedCase.item.outcomeReceipt.commitSeq}</span></dd>
                  <dt className="text-muted-foreground">Curated</dt>
                  <dd>{selectedCase.item.curatedBy}, {when(selectedCase.item.curatedAtMs)}</dd>
                  {selectedCase.item.note && (
                    <>
                      <dt className="text-muted-foreground">Note</dt>
                      <dd className="text-muted-foreground">{selectedCase.item.note}</dd>
                    </>
                  )}
                </dl>

                <div className="border-t pt-3">
                  <h3 className="mb-2 text-sm font-medium">Case actions</h3>
                  <RecordActions
                    objectKind={CURATED_CASE_TYPE}
                    record={selectedCase.record}
                    ariaLabel="Case actions"
                    emptyText="The cell offers you no actions on this case."
                    onChanged={refreshAll}
                  />
                </div>
              </div>
            </>
          )}
        </SheetContent>
      </Sheet>
    </div>
  )
}
