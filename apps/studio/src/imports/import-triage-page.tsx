import { useQuery, useQueryClient } from "@tanstack/react-query"
import { useNavigate, useSearch } from "@tanstack/react-router"
import { Columns2, GitCompareArrows, Inbox, Link2, RefreshCw, ShieldCheck } from "lucide-react"
import { useEffect, useMemo, useRef, useState } from "react"
import { Chip } from "@/components/chip"
import { FactStatusBadge } from "@/components/fact-status-badge"
import { EmptyState, PageHeader } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { newIdempotencyKey, type ActionReceipt } from "@/lib/actions"
import type { CellResult } from "@/lib/cell-read"
import { NO_DATA_CELL } from "@/lib/definitions"
import {
  fetchImportTriage,
  IMPORT_TRIAGE_UNAVAILABLE,
  isFieldWithheld,
  isTriageStep,
  triageSummary,
  type AliasCandidate,
  type AliasResolution,
  type ImportTriageWorkspace,
  type RowActionOutcome,
  type SupersessionConflict,
  type SupersessionResolution,
  type TargetEntity,
  type TriageStep,
} from "@/lib/import-triage"
import { cn } from "@/lib/utils"
import { AliasMatcher, initialAliasDraft, type AliasDraftState } from "./alias-matcher.tsx"
import { SplitPaneReview } from "./split-pane-review.tsx"
import { initialSupersessionDraft, SupersessionDiffList, type SupersessionDraftState } from "./supersession-diff.tsx"

const QUERY_KEY = ["imports", "triage"] as const

/** Unsent drafts, persisted idempotency keys, and request outcomes scoped to one `batchId`. */
interface BatchDraftState {
  readonly aliases: Readonly<Record<string, AliasDraftState>>
  readonly reviewEdits: Readonly<Record<string, string>>
  readonly rowKeys: Readonly<Record<string, string>>
  readonly rowOutcomes: Readonly<Record<string, RowActionOutcome>>
  readonly supersessions: Readonly<Record<string, SupersessionDraftState>>
}

const EMPTY_BATCH_DRAFT: BatchDraftState = {
  aliases: {},
  reviewEdits: {},
  rowKeys: {},
  rowOutcomes: {},
  supersessions: {},
}

/** Sidebar rail badge showing total open triage items awaiting operator review. */
export function useImportTriageBadge(): number | undefined {
  const read = useQuery({
    queryKey: QUERY_KEY,
    queryFn: () => fetchImportTriage(),
    refetchInterval: 60_000,
    retry: false,
  })
  if (read.data?.kind !== "ok") return undefined
  const total = triageSummary(read.data.value).totalAttention
  return total > 0 ? total : undefined
}

/**
 * `/imports`: Dedicated domain-agnostic Split-Pane Import Triage surface (separate from DecisionSheet).
 * Three flat steps:
 *   1. Match aliases / entities
 *   2. Split-pane artifact triage & batch governed promotion
 *   3. Supersession diff cards (FEED-CONFLICT)
 *
 * Unsent drafts and idempotency keys are scoped by `batchId` so switching between Steps 1/2/3
 * preserves work within the same batch while preventing stale responses from mutating another batch.
 */
export function ImportTriagePage() {
  const client = useQueryClient()
  const navigate = useNavigate()
  const search = useSearch({ strict: false }) as { step?: unknown }
  const step: TriageStep = isTriageStep(search.step) ? search.step : "aliases"

  const read = useQuery<CellResult<ImportTriageWorkspace>>({
    queryKey: QUERY_KEY,
    queryFn: () => fetchImportTriage(),
    retry: false,
  })

  const workspace = read.data?.kind === "ok" ? read.data.value : undefined
  const activeBatchIdRef = useRef<string | undefined>(workspace?.batchId)
  useEffect(() => {
    activeBatchIdRef.current = workspace?.batchId
  }, [workspace?.batchId])

  const [draftsByBatch, setDraftsByBatch] = useState<Readonly<Record<string, BatchDraftState>>>({})
  const batchDraft = workspace ? (draftsByBatch[workspace.batchId] ?? EMPTY_BATCH_DRAFT) : EMPTY_BATCH_DRAFT

  const summary = useMemo(() => (workspace ? triageSummary(workspace) : null), [workspace])

  const resolvedAliases = useMemo(() => {
    const map = new Map<string, AliasResolution>()
    for (const alias of workspace?.aliases ?? []) {
      if (alias.resolution !== null) map.set(alias.id, alias.resolution)
    }
    return map
  }, [workspace])

  const setStep = (next: TriageStep) => {
    void navigate({ to: "/imports", search: { step: next } as never, replace: true })
  }

  /** Apply a cache update strictly when the active batch still matches `expectedBatchId`. */
  const updateWorkspace = (expectedBatchId: string, updater: (current: ImportTriageWorkspace) => ImportTriageWorkspace) => {
    if (activeBatchIdRef.current !== expectedBatchId) return
    client.setQueryData<CellResult<ImportTriageWorkspace>>(QUERY_KEY, (current) => {
      if (current?.kind !== "ok" || current.value.batchId !== expectedBatchId) return current
      return { ...current, value: updater(current.value) }
    })
  }

  const patchBatchDraft = (expectedBatchId: string, updater: (current: BatchDraftState) => BatchDraftState) => {
    if (activeBatchIdRef.current !== expectedBatchId) return
    setDraftsByBatch((all) => {
      const current = all[expectedBatchId] ?? EMPTY_BATCH_DRAFT
      return { ...all, [expectedBatchId]: updater(current) }
    })
  }

  const handlePatchAliasDraft = (
    expectedBatchId: string,
    alias: AliasCandidate,
    update: Partial<AliasDraftState>,
    rotateKey = false,
  ) => {
    patchBatchDraft(expectedBatchId, (current) => {
      const prev = current.aliases[alias.id] ?? initialAliasDraft(alias)
      return {
        ...current,
        aliases: {
          ...current.aliases,
          [alias.id]: {
            ...prev,
            ...update,
            ...rotateKey ? { idempotencyKey: newIdempotencyKey() } : {},
          },
        },
      }
    })
  }

  const handleAliasResolved = (
    expectedBatchId: string,
    aliasId: string,
    resolution: AliasResolution,
    createdEntity?: TargetEntity,
  ) => {
    updateWorkspace(expectedBatchId, (current) => ({
      ...current,
      availableEntities: createdEntity && !current.availableEntities.some((e) => e.id === createdEntity.id)
        ? [...current.availableEntities, createdEntity]
        : current.availableEntities,
      aliases: current.aliases.map((alias) => alias.id === aliasId ? { ...alias, resolution } : alias),
      rows: current.rows.map((row) => {
        if (row.entityAliasId !== aliasId) return row
        if (resolution.kind === "bound" || resolution.kind === "created") {
          return { ...row, entityRecordId: resolution.targetId, entityLabel: resolution.targetLabel }
        }
        return row
      }),
    }))
    // Re-read authoritative batch state from the cell after commit.
    void client.invalidateQueries({ queryKey: QUERY_KEY })
  }

  const handleSetFieldValue = (expectedBatchId: string, rowId: string, fieldName: string, nextValue: string) => {
    patchBatchDraft(expectedBatchId, (current) => {
      const nextOutcomes = { ...current.rowOutcomes }
      delete nextOutcomes[rowId]
      return {
        ...current,
        reviewEdits: { ...current.reviewEdits, [`${rowId}:${fieldName}`]: nextValue },
        // Rotate the row's idempotency key when the operator changes its draft values.
        rowKeys: { ...current.rowKeys, [rowId]: newIdempotencyKey() },
        rowOutcomes: nextOutcomes,
      }
    })
  }

  const handleResetFieldValue = (expectedBatchId: string, rowId: string, fieldName: string) => {
    patchBatchDraft(expectedBatchId, (current) => {
      const nextEdits = { ...current.reviewEdits }
      delete nextEdits[`${rowId}:${fieldName}`]
      const nextOutcomes = { ...current.rowOutcomes }
      delete nextOutcomes[rowId]
      return {
        ...current,
        reviewEdits: nextEdits,
        rowKeys: { ...current.rowKeys, [rowId]: newIdempotencyKey() },
        rowOutcomes: nextOutcomes,
      }
    })
  }

  const handleEnsureRowKey = (expectedBatchId: string, rowId: string): string => {
    const existing = draftsByBatch[expectedBatchId]?.rowKeys[rowId]
    if (existing !== undefined) return existing
    const fresh = newIdempotencyKey()
    patchBatchDraft(expectedBatchId, (current) => (
      current.rowKeys[rowId] !== undefined
        ? current
        : { ...current, rowKeys: { ...current.rowKeys, [rowId]: fresh } }
    ))
    return fresh
  }

  const handleRowPromoted = (
    expectedBatchId: string,
    rowId: string,
    values: Readonly<Record<string, string>>,
    receipt: ActionReceipt,
  ) => {
    const recordedAt = new Date(receipt.receipt.recordedAtMs).toISOString()
    updateWorkspace(expectedBatchId, (current) => ({
      ...current,
      rows: current.rows.map((row) => row.id !== rowId ? row : {
        ...row,
        status: "verified_fact",
        recordedAt,
        receipt,
        fields: row.fields.map((field) => isFieldWithheld(field) ? field : {
          ...field,
          value: values[field.name] ?? field.value,
        }),
      }),
    }))
    patchBatchDraft(expectedBatchId, (current) => {
      const nextOutcomes = { ...current.rowOutcomes }
      delete nextOutcomes[rowId]
      return { ...current, rowOutcomes: nextOutcomes }
    })
    // Re-read authoritative batch state from the cell after commit.
    void client.invalidateQueries({ queryKey: QUERY_KEY })
  }

  const handleRowOutcome = (expectedBatchId: string, rowId: string, outcome: RowActionOutcome | null) => {
    patchBatchDraft(expectedBatchId, (current) => {
      const nextOutcomes = { ...current.rowOutcomes }
      if (outcome === null) delete nextOutcomes[rowId]
      else nextOutcomes[rowId] = outcome
      return { ...current, rowOutcomes: nextOutcomes }
    })
  }

  const handlePatchSupersessionDraft = (
    expectedBatchId: string,
    conflict: SupersessionConflict,
    update: Partial<SupersessionDraftState>,
    rotateKey = false,
  ) => {
    patchBatchDraft(expectedBatchId, (current) => {
      const prev = current.supersessions[conflict.id] ?? initialSupersessionDraft(conflict)
      return {
        ...current,
        supersessions: {
          ...current.supersessions,
          [conflict.id]: {
            ...prev,
            ...update,
            ...rotateKey ? { idempotencyKey: newIdempotencyKey() } : {},
          },
        },
      }
    })
  }

  const handleSupersessionResolved = (
    expectedBatchId: string,
    conflictId: string,
    resolution: SupersessionResolution,
  ) => {
    updateWorkspace(expectedBatchId, (current) => ({
      ...current,
      supersessions: current.supersessions.map((item) => item.id === conflictId ? { ...item, resolution } : item),
    }))
    void client.invalidateQueries({ queryKey: QUERY_KEY })
  }

  const refresh = () => void client.invalidateQueries({ queryKey: QUERY_KEY })

  return (
    <div className="space-y-4">
      <PageHeader
        title="Import triage"
        description="Inspect staged captures beside source evidence, resolve external aliases, and promote clean rows through governed cell laws."
        meta={workspace ? (
          <>
            <Chip tone="gray" mono>{workspace.targetObjectType}</Chip>
            <span className="text-xs text-muted-foreground"><bdi>{workspace.title}</bdi></span>
          </>
        ) : undefined}
        actions={
          <Button variant="outline" size="sm" onClick={refresh} disabled={read.isFetching} className="h-8 gap-1.5 border-border bg-background text-xs shadow-none">
            <RefreshCw className={cn("size-3.5", read.isFetching && "animate-spin")} />
            Refresh
          </Button>
        }
      />

      {/* Loading / error / unavailable / no-cell states */}
      {read.isPending && (
        <div className="grid gap-3" aria-label="Reading import triage batch">
          <Skeleton className="h-12 rounded-lg" />
          <Skeleton className="h-80 rounded-lg" />
        </div>
      )}

      {read.isError && (
        <div role="alert" className="flex flex-wrap items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
          <span>{read.error instanceof Error ? read.error.message : "The import triage batch could not be read."}</span>
          <span className="flex-1" />
          <Button variant="outline" size="sm" onClick={() => void read.refetch()}>Retry</Button>
        </div>
      )}

      {!read.isPending && !read.isError && read.data?.kind === "no-cell" && (
        <div className="rounded-lg border border-border bg-background">
          <EmptyState icon={Inbox} title="No data cell">{NO_DATA_CELL}</EmptyState>
        </div>
      )}

      {!read.isPending && !read.isError && read.data?.kind === "unavailable" && (
        <div className="rounded-lg border border-border bg-background">
          <EmptyState icon={Inbox} title="Not served yet">{IMPORT_TRIAGE_UNAVAILABLE}</EmptyState>
        </div>
      )}

      {!read.isPending && !read.isError && read.data?.kind === "refused" && (
        <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
          {read.data.refusal.message}
        </p>
      )}

      {workspace && summary && (
        <div className="space-y-3">
          {/* Progressive go-live honest status strip */}
          <div
            aria-label="Progressive verification status"
            className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-surface-subtle px-3.5 py-2"
          >
            <div className="flex flex-wrap items-center gap-2 text-xs tabular-nums">
              <span className="inline-flex items-center gap-1.5">
                <FactStatusBadge status="verified_fact" />
                <span className="font-medium text-foreground">{summary.verifiedRows}</span>
              </span>
              <span className="text-border">|</span>
              <span className="inline-flex items-center gap-1.5">
                <FactStatusBadge status="imported_unverified" />
                <span className="font-medium text-foreground">{summary.unverifiedRows}</span>
              </span>
              <span className="text-border">|</span>
              <span className="inline-flex items-center gap-1.5">
                <FactStatusBadge status="quarantined" />
                <span className="font-medium text-foreground">{summary.quarantinedRows}</span>
              </span>
              {summary.withheldRows > 0 && (
                <>
                  <span className="text-border">|</span>
                  <span className="inline-flex items-center gap-1.5">
                    <FactStatusBadge status="withheld" />
                    <span className="font-medium text-foreground">{summary.withheldRows}</span>
                  </span>
                </>
              )}
            </div>
            <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
              <ShieldCheck aria-hidden className="size-3.5 text-muted-foreground" />
              Advisory extraction only — every write runs through governed cell actions.
            </span>
          </div>

          {/* 3-step flat horizontal stepper */}
          <nav aria-label="Import triage steps" className="flex flex-wrap items-center gap-1 border-b border-border pb-2">
            {([
              {
                id: "aliases" as const,
                number: "1",
                label: "Match aliases",
                icon: Link2,
                count: summary.unresolvedAliases,
                tone: summary.unresolvedAliases > 0 ? "amber" as const : "green" as const,
              },
              {
                id: "review" as const,
                number: "2",
                label: "Artifact triage",
                icon: Columns2,
                count: summary.unverifiedRows + summary.quarantinedRows + summary.withheldRows,
                tone: summary.quarantinedRows > 0 ? "red" as const : summary.unverifiedRows > 0 ? "amber" as const : "green" as const,
              },
              {
                id: "supersession" as const,
                number: "3",
                label: "Supersession diffs",
                icon: GitCompareArrows,
                count: summary.openConflicts,
                tone: summary.openConflicts > 0 ? "red" as const : "green" as const,
              },
            ]).map((item) => {
              const Icon = item.icon
              const active = step === item.id
              return (
                <button
                  key={item.id}
                  type="button"
                  data-triage-step={item.id}
                  aria-current={active ? "step" : undefined}
                  onClick={() => setStep(item.id)}
                  className={cn(
                    "inline-flex h-9 items-center gap-2 rounded-md border px-3 text-xs font-medium transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    active
                      ? "border-foreground/25 bg-background text-foreground"
                      : "border-transparent text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                  )}
                >
                  <span className={cn(
                    "flex size-5 items-center justify-center rounded text-[0.6875rem] font-semibold tabular-nums",
                    active ? "bg-foreground text-background" : "bg-muted text-muted-foreground",
                  )}>
                    {item.number}
                  </span>
                  <Icon aria-hidden className="size-3.5" />
                  <span>{item.label}</span>
                  <Chip tone={item.tone} className="tabular-nums">{item.count}</Chip>
                </button>
              )
            })}
          </nav>

          {/* Active step body */}
          {step === "aliases" && (
            <AliasMatcher
              key={workspace.batchId}
              batchId={workspace.batchId}
              aliases={workspace.aliases}
              entities={workspace.availableEntities}
              actions={workspace.actions}
              drafts={batchDraft.aliases}
              onPatchDraft={handlePatchAliasDraft}
              onResolved={handleAliasResolved}
            />
          )}

          {step === "review" && (
            <SplitPaneReview
              key={workspace.batchId}
              batchId={workspace.batchId}
              artifacts={workspace.artifacts}
              rows={workspace.rows}
              lowConfidenceThresholdBps={workspace.lowConfidenceThresholdBps}
              actions={workspace.actions}
              resolvedAliases={resolvedAliases}
              edits={batchDraft.reviewEdits}
              rowKeys={batchDraft.rowKeys}
              rowOutcomes={batchDraft.rowOutcomes}
              onSetFieldValue={handleSetFieldValue}
              onResetFieldValue={handleResetFieldValue}
              onEnsureRowKey={handleEnsureRowKey}
              onRowPromoted={handleRowPromoted}
              onRowOutcome={handleRowOutcome}
              onJumpToAliases={() => setStep("aliases")}
            />
          )}

          {step === "supersession" && (
            <SupersessionDiffList
              key={workspace.batchId}
              batchId={workspace.batchId}
              conflicts={workspace.supersessions}
              actions={workspace.actions}
              lowConfidenceThresholdBps={workspace.lowConfidenceThresholdBps}
              drafts={batchDraft.supersessions}
              onPatchDraft={handlePatchSupersessionDraft}
              onResolved={handleSupersessionResolved}
            />
          )}
        </div>
      )}
    </div>
  )
}
