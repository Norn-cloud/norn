import { useQuery, useQueryClient } from "@tanstack/react-query"
import { useNavigate, useSearch } from "@tanstack/react-router"
import { Columns2, GitCompareArrows, Inbox, Link2, RefreshCw, ShieldCheck } from "lucide-react"
import { useEffect, useMemo, useRef, useState } from "react"
import { Chip } from "@/components/chip"
import { FactStatusBadge } from "@/components/fact-status-badge"
import { EmptyState, PageHeader } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { type ActionReceipt } from "../../../../packages/norn-sdk/src/index.ts"
import type { CellResult } from "@/lib/cell-read"
import { NO_DATA_CELL } from "@/lib/definitions"
import { usePinnedScope } from "@/lib/session"
import {
  batchLockedRowIds,
  classifyTriageActionResult,
  commitAliasBinding,
  commitAliasIgnore,
  commitEntityCreateAndBind,
  commitRowPromotion,
  commitSupersessionDecision,
  effectiveRowStatus,
  extractEditableValues,
  fetchImportTriage,
  fetchRequestedDecisionForReconciliation,
  fieldEditKey,
  IMPORT_TRIAGE_QUERY_KEY as QUERY_KEY,
  IMPORT_TRIAGE_UNAVAILABLE,
  initialAliasDraft,
  initialSupersessionDraft,
  isOutcomeLocked,
  isTriageStep,
  patchAliasDraftState,
  patchSupersessionDraftState,
  readLiveWorkspaceFromCache,
  reconcilePendingRowOutcome,
  resolvePayloadIdempotencyKey,
  resolveRowEntity,
  retirePayloadIdempotencyKey,
  rowBasisKey,
  runBatchPromotionQueue,
  triageSummary,
  verifyQueuedPromotionBasis,
  type AliasCandidate,
  type AliasDraftState,
  type AliasResolution,
  type BatchRunReport,
  type CandidateRow,
  type ImportTriageWorkspace,
  type PayloadBoundKeyEntry,
  type QueuedPromotionItem,
  type RowActionOutcome,
  type StagedArtifact,
  type SupersessionConflict,
  type SupersessionDraftState,
  type SupersessionResolution,
  type TargetEntity,
  type TriageStep,
} from "@/lib/import-triage"
import { cn } from "@/lib/utils"
import { AliasMatcher } from "./alias-matcher.tsx"
import { SplitPaneReview } from "./split-pane-review.tsx"
import { SupersessionDiffList } from "./supersession-diff.tsx"

/** Unsent drafts, in-flight/queued promotion locks, and request outcomes scoped to one `batchId`. */
interface BatchDraftState {
  readonly aliases: Readonly<Record<string, AliasDraftState>>
  readonly reviewEdits: Readonly<Record<string, string>>
  readonly rowOutcomes: Readonly<Record<string, RowActionOutcome>>
  readonly busyRowIds: ReadonlySet<string>
  readonly queuedRowIds: ReadonlySet<string>
  readonly batchRunning: boolean
  readonly batchReport: BatchRunReport | null
  readonly supersessions: Readonly<Record<string, SupersessionDraftState>>
}

const EMPTY_SET: ReadonlySet<string> = new Set()

const EMPTY_BATCH_DRAFT: BatchDraftState = {
  aliases: {},
  reviewEdits: {},
  rowOutcomes: {},
  busyRowIds: EMPTY_SET,
  queuedRowIds: EMPTY_SET,
  batchRunning: false,
  batchReport: null,
  supersessions: {},
}

export { useImportTriageBadge } from "@/lib/nav-menus"

/**
 * `/imports`: Dedicated domain-agnostic Split-Pane Import Triage surface (separate from DecisionSheet).
 * Three flat steps:
 *   1. Match aliases / entities
 *   2. Split-pane artifact triage & batch governed promotion
 *   3. Supersession diff cards (FEED-CONFLICT)
 *
 * All unsent drafts, promotion locks, queued row sets, and idempotency keys live here scoped by
 * `batchId`, so switching between Steps 1/2/3 preserves locks and unsent work while a batch switch
 * immediately halts remaining queued dispatches for the superseded batch.
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
  const pinnedScope = usePinnedScope(workspace?.batchId ?? "")
  const workspaceRef = useRef<ImportTriageWorkspace | undefined>(workspace)
  workspaceRef.current = workspace

  // Read the live TanStack Query cache directly at dispatch time and fail closed on thrown refetch
  // errors so a refetch that updated or errored prior to React re-rendering never authorizes stale dispatch.
  const getLiveWorkspace = () =>
    readLiveWorkspaceFromCache(
      () => client.getQueryData<CellResult<ImportTriageWorkspace>>(QUERY_KEY),
      workspaceRef.current,
      () => client.getQueryState(QUERY_KEY),
    )

  // Flipped on `/imports` unmount so queued batch promotions stop immediately if the operator navigates away.
  const mountedRef = useRef(true)
  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
    }
  }, [])

  const [draftsByBatch, setDraftsByBatch] = useState<Readonly<Record<string, BatchDraftState>>>({})
  const draftsRef = useRef<Readonly<Record<string, BatchDraftState>>>(draftsByBatch)
  draftsRef.current = draftsByBatch

  // Synchronous per-batch in-flight locks survive step unmounts and rapid double-clicks.
  const inFlightRowsRef = useRef<Map<string, Set<string>>>(new Map())
  const inFlightAliasesRef = useRef<Map<string, Set<string>>>(new Map())
  const inFlightSupersessionsRef = useRef<Map<string, Set<string>>>(new Map())
  const batchRunningRef = useRef<Set<string>>(new Set())

  // Canonical outgoing-payload-bound idempotency key cache (reuses exact key on identical retries;
  // automatically mints a fresh key when any outgoing payload field or cell basis changes).
  const payloadKeyCacheRef = useRef<Map<string, PayloadBoundKeyEntry>>(new Map())

  const batchDraft = workspace ? (draftsByBatch[workspace.batchId] ?? EMPTY_BATCH_DRAFT) : EMPTY_BATCH_DRAFT

  /** Reconcile any local `pending` approval outcomes against authoritative `/v1/decisions:requested` state. */
  const reconcilePendingOutcomes = async (requestBatchId: string) => {
    const current = draftsRef.current[requestBatchId]
    if (!current) return
    const pendingEntries = Object.entries(current.rowOutcomes).filter(
      (entry): entry is [string, Extract<RowActionOutcome, { readonly kind: "pending" }>] => entry[1].kind === "pending",
    )
    if (pendingEntries.length === 0) return

    let needsWorkspaceRefresh = false
    await Promise.all(pendingEntries.map(async ([rowId, pendingOutcome]) => {
      try {
        const lookup = await fetchRequestedDecisionForReconciliation(pendingOutcome.decisionId)
        if (!mountedRef.current || lookup.kind === "malformed") return
        const { nextOutcome, shouldRefreshWorkspace, retireIdempotencyKey } = reconcilePendingRowOutcome(pendingOutcome, lookup)
        if (shouldRefreshWorkspace) needsWorkspaceRefresh = true
        if (nextOutcome === pendingOutcome) return
        const currentLive = draftsRef.current[requestBatchId]?.rowOutcomes[rowId]
        if (currentLive?.kind !== "pending" || currentLive.decisionId !== pendingOutcome.decisionId) return
        if (retireIdempotencyKey) {
          retirePayloadIdempotencyKey(payloadKeyCacheRef.current, `row:${requestBatchId}:${rowId}`)
        }
        patchBatchDraft(requestBatchId, (curr) => {
          const liveOutcome = curr.rowOutcomes[rowId]
          if (liveOutcome?.kind !== "pending" || liveOutcome.decisionId !== pendingOutcome.decisionId) return curr
          return {
            ...curr,
            rowOutcomes: { ...curr.rowOutcomes, [rowId]: nextOutcome },
          }
        })
      } catch {
        // Transient decision lookup failure preserves the pending lock safely.
      }
    }))

    if (needsWorkspaceRefresh && mountedRef.current && getLiveWorkspace()?.batchId === requestBatchId) {
      void client.invalidateQueries({ queryKey: QUERY_KEY })
    }
  }

  // Prune local edits whenever a row becomes verified or its authoritative basis (capture/digests/baseline) changes,
  // clear transient outcomes once Verified fact is confirmed, and reconcile pending approvals on refresh.
  useEffect(() => {
    if (!workspace) return
    const verifiedIds = new Set<string>()
    const activeEditPrefixes = new Set<string>()
    for (const row of workspace.rows) {
      if (effectiveRowStatus(row) === "verified_fact") {
        verifiedIds.add(row.id)
      } else {
        const artifact = workspace.artifacts.find((item) => item.id === row.artifactId)
        activeEditPrefixes.add(`${row.id}\u0000${rowBasisKey(row, artifact)}\u0000`)
      }
    }
    setDraftsByBatch((all) => {
      const current = all[workspace.batchId]
      if (!current) return all
      const staleEditKeys = Object.keys(current.reviewEdits).filter((k) => {
        for (const prefix of activeEditPrefixes) {
          if (k.startsWith(prefix)) return false
        }
        return true
      })
      const staleOutcomeIds = Object.keys(current.rowOutcomes).filter((id) => verifiedIds.has(id))
      if (staleEditKeys.length === 0 && staleOutcomeIds.length === 0) return all
      const nextEdits = staleEditKeys.length > 0 ? { ...current.reviewEdits } : current.reviewEdits
      for (const k of staleEditKeys) delete (nextEdits as Record<string, string>)[k]
      const nextOutcomes = staleOutcomeIds.length > 0 ? { ...current.rowOutcomes } : current.rowOutcomes
      for (const id of staleOutcomeIds) delete (nextOutcomes as Record<string, RowActionOutcome>)[id]
      return { ...all, [workspace.batchId]: { ...current, reviewEdits: nextEdits, rowOutcomes: nextOutcomes } }
    })
    void reconcilePendingOutcomes(workspace.batchId)
  }, [workspace])

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

  /** Apply a cache update strictly while mounted and the active batch still matches `expectedBatchId`. */
  const updateWorkspace = (expectedBatchId: string, updater: (current: ImportTriageWorkspace) => ImportTriageWorkspace) => {
    if (!mountedRef.current || getLiveWorkspace()?.batchId !== expectedBatchId) return
    client.setQueryData<CellResult<ImportTriageWorkspace>>(QUERY_KEY, (current) => {
      if (current?.kind !== "ok" || current.value.batchId !== expectedBatchId) return current
      return { ...current, value: updater(current.value) }
    })
  }

  const patchBatchDraft = (expectedBatchId: string, updater: (current: BatchDraftState) => BatchDraftState) => {
    if (!mountedRef.current) return
    setDraftsByBatch((all) => {
      const current = all[expectedBatchId] ?? EMPTY_BATCH_DRAFT
      const next = updater(current)
      if (next === current) return all
      return { ...all, [expectedBatchId]: next }
    })
  }

  const getLockSet = (map: Map<string, Set<string>>, bId: string) => {
    let set = map.get(bId)
    if (!set) { set = new Set(); map.set(bId, set) }
    return set
  }

  // ---- Step 1: Aliases ------------------------------------------------------------------------

  const handlePatchAliasDraft = (
    expectedBatchId: string,
    alias: AliasCandidate,
    update: Partial<AliasDraftState>,
  ) => {
    if (!mountedRef.current || getLiveWorkspace()?.batchId !== expectedBatchId) return
    patchBatchDraft(expectedBatchId, (current) => {
      const nextDraft = patchAliasDraftState(current.aliases[alias.id], alias, update)
      return { ...current, aliases: { ...current.aliases, [alias.id]: nextDraft } }
    })
  }

  const handleCommitAlias = async (requestBatchId: string, alias: AliasCandidate) => {
    if (!mountedRef.current) return
    const locks = getLockSet(inFlightAliasesRef.current, requestBatchId)
    if (locks.has(alias.id)) return
    const currentWorkspace = getLiveWorkspace()
    if (!currentWorkspace || currentWorkspace.batchId !== requestBatchId) return

    const currentDraft = draftsRef.current[requestBatchId]?.aliases[alias.id] ?? initialAliasDraft(alias)

    if (currentDraft.mode === "match") {
      const target = currentWorkspace.availableEntities.find((entity) => entity.id === currentDraft.selectedTargetId)
      if (!target) {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          aliases: {
            ...curr.aliases,
            [alias.id]: patchAliasDraftState(curr.aliases[alias.id], alias, {
              notice: { kind: "refused", message: "Select a canonical record before binding this alias." },
            }),
          },
        }))
        return
      }
    } else if (currentDraft.mode === "create" && currentDraft.createLabel.trim() === "") {
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        aliases: {
          ...curr.aliases,
          [alias.id]: patchAliasDraftState(curr.aliases[alias.id], alias, {
            notice: { kind: "refused", message: "Enter a record label before creating and binding." },
          }),
        },
      }))
      return
    }

    const scopeKey = `alias:${requestBatchId}:${alias.id}`
    let actionId: string
    let payload: Record<string, unknown>
    let createdEntity: TargetEntity | undefined
    let makeResolution: (receipt: ActionReceipt) => AliasResolution

    if (currentDraft.mode === "match") {
      const target = currentWorkspace.availableEntities.find((entity) => entity.id === currentDraft.selectedTargetId)!
      actionId = currentWorkspace.actions.bindAlias
      payload = {
        batchId: requestBatchId,
        aliasId: alias.id,
        externalHandle: alias.externalHandle,
        targetRecordId: target.id,
      }
      makeResolution = (receipt) => ({ kind: "bound", targetId: target.id, targetLabel: target.label, receipt })
    } else if (currentDraft.mode === "create") {
      const label = currentDraft.createLabel.trim()
      const externalKey = currentDraft.createExternalKey.trim()
      actionId = currentWorkspace.actions.createEntity
      payload = {
        batchId: requestBatchId,
        aliasId: alias.id,
        externalHandle: alias.externalHandle,
        label,
        ...externalKey !== "" ? { externalKey } : {},
      }
      makeResolution = (receipt) => {
        createdEntity = { id: receipt.objectId, label, subtitle: externalKey || undefined }
        return { kind: "created", targetId: receipt.objectId, targetLabel: label, receipt }
      }
    } else {
      const note = currentDraft.ignoreNote.trim()
      actionId = currentWorkspace.actions.ignoreAlias
      payload = {
        batchId: requestBatchId,
        aliasId: alias.id,
        externalHandle: alias.externalHandle,
        ...note !== "" ? { note } : {},
      }
      makeResolution = (receipt) => ({ kind: "ignored", ...note !== "" ? { note } : {}, receipt })
    }

    const key = resolvePayloadIdempotencyKey(payloadKeyCacheRef.current, scopeKey, actionId, payload)

    locks.add(alias.id)
    patchBatchDraft(requestBatchId, (curr) => ({
      ...curr,
      aliases: {
        ...curr.aliases,
        [alias.id]: patchAliasDraftState(curr.aliases[alias.id], alias, {
          idempotencyKey: key,
          pending: true,
          notice: null,
        }),
      },
    }))

    try {
      let rawResult
      if (currentDraft.mode === "match") {
        rawResult = await commitAliasBinding(actionId, payload as Parameters<typeof commitAliasBinding>[1], key, undefined, pinnedScope)
      } else if (currentDraft.mode === "create") {
        rawResult = await commitEntityCreateAndBind(actionId, payload as Parameters<typeof commitEntityCreateAndBind>[1], key, undefined, pinnedScope)
      } else {
        rawResult = await commitAliasIgnore(actionId, payload as Parameters<typeof commitAliasIgnore>[1], key, undefined, pinnedScope)
      }

      const classified = classifyTriageActionResult(rawResult)
      if (classified.kind === "receipt") {
        const resolution = makeResolution(classified.receipt)
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          aliases: {
            ...curr.aliases,
            [alias.id]: patchAliasDraftState(curr.aliases[alias.id], alias, { idempotencyKey: key, pending: false, notice: null }),
          },
        }))
        updateWorkspace(requestBatchId, (curr) => ({
          ...curr,
          availableEntities: createdEntity && !curr.availableEntities.some((e) => e.id === createdEntity!.id)
            ? [...curr.availableEntities, createdEntity!]
            : curr.availableEntities,
          aliases: curr.aliases.map((item) => item.id === alias.id ? { ...item, resolution } : item),
          rows: curr.rows.map((row) => {
            if (row.entityAliasId !== alias.id) return row
            if (resolution.kind === "bound" || resolution.kind === "created") {
              return { ...row, entityRecordId: resolution.targetId, entityLabel: resolution.targetLabel }
            }
            return row
          }),
        }))
        if (mountedRef.current && getLiveWorkspace()?.batchId === requestBatchId) {
          void client.invalidateQueries({ queryKey: QUERY_KEY })
        }
      } else if (classified.kind === "pending") {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          aliases: {
            ...curr.aliases,
            [alias.id]: patchAliasDraftState(curr.aliases[alias.id], alias, {
              idempotencyKey: key,
              pending: false,
              notice: { kind: "pending", message: `Pending approval decision (${classified.decisionId}).` },
            }),
          },
        }))
      } else if (classified.kind === "refused") {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          aliases: {
            ...curr.aliases,
            [alias.id]: patchAliasDraftState(curr.aliases[alias.id], alias, {
              idempotencyKey: key,
              pending: false,
              notice: { kind: "refused", message: `${classified.code}: ${classified.message}` },
            }),
          },
        }))
      } else {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          aliases: {
            ...curr.aliases,
            [alias.id]: patchAliasDraftState(curr.aliases[alias.id], alias, {
              idempotencyKey: key,
              pending: false,
              notice: { kind: "error", message: classified.message },
            }),
          },
        }))
      }
    } catch (error) {
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        aliases: {
          ...curr.aliases,
          [alias.id]: patchAliasDraftState(curr.aliases[alias.id], alias, {
            idempotencyKey: key,
            pending: false,
            notice: { kind: "error", message: error instanceof Error ? error.message : "The cell did not respond. Retry reuses the same idempotency key." },
          }),
        },
      }))
    } finally {
      locks.delete(alias.id)
    }
  }

  // ---- Step 2: Review & Batch Promotion (locks + queue survive step switches, check live basis, halt on unmount/batch switch) ----

  const handleSetFieldValue = (
    expectedBatchId: string,
    rowId: string,
    basisKey: string,
    fieldName: string,
    baselineValue: string,
    currentValue: string,
    nextValue: string,
  ) => {
    if (!mountedRef.current || getLiveWorkspace()?.batchId !== expectedBatchId) return
    if (currentValue === nextValue) return
    const editKey = fieldEditKey(rowId, basisKey, fieldName)
    patchBatchDraft(expectedBatchId, (current) => {
      if (isOutcomeLocked(current.rowOutcomes[rowId])) return current
      const nextEdits = { ...current.reviewEdits }
      if (nextValue === baselineValue) {
        delete nextEdits[editKey]
      } else {
        nextEdits[editKey] = nextValue
      }
      const nextOutcomes = { ...current.rowOutcomes }
      delete nextOutcomes[rowId]
      return {
        ...current,
        reviewEdits: nextEdits,
        rowOutcomes: nextOutcomes,
      }
    })
  }

  const handleResetFieldValue = (
    expectedBatchId: string,
    rowId: string,
    basisKey: string,
    fieldName: string,
    baselineValue: string,
  ) => {
    if (!mountedRef.current || getLiveWorkspace()?.batchId !== expectedBatchId) return
    const editKey = fieldEditKey(rowId, basisKey, fieldName)
    patchBatchDraft(expectedBatchId, (current) => {
      if (isOutcomeLocked(current.rowOutcomes[rowId])) return current
      if (!(editKey in current.reviewEdits)) return current
      const previousEdit = current.reviewEdits[editKey]
      const nextEdits = { ...current.reviewEdits }
      delete nextEdits[editKey]
      if (previousEdit === baselineValue) {
        return { ...current, reviewEdits: nextEdits }
      }
      const nextOutcomes = { ...current.rowOutcomes }
      delete nextOutcomes[rowId]
      return {
        ...current,
        reviewEdits: nextEdits,
        rowOutcomes: nextOutcomes,
      }
    })
  }

  const dispatchPromoteRow = async (
    requestBatchId: string,
    row: CandidateRow,
    artifact: StagedArtifact,
    snapshotValues?: Readonly<Record<string, string>>,
    expectedTargetRecordId?: string,
  ): Promise<"promoted" | "refused" | "pending" | "error" | "stale" | "skipped"> => {
    if (!mountedRef.current) return "skipped"
    const locks = getLockSet(inFlightRowsRef.current, requestBatchId)
    if (locks.has(row.id)) return "skipped"
    const currentWorkspace = getLiveWorkspace()
    if (!currentWorkspace || currentWorkspace.batchId !== requestBatchId) return "skipped"
    const currentDraft = draftsRef.current[requestBatchId] ?? EMPTY_BATCH_DRAFT
    if (isOutcomeLocked(currentDraft.rowOutcomes[row.id])) return "skipped"

    // Re-read the live row, artifact, entity target, and canonical baseline values before dispatch
    // so stale evidence, drifted baselines, or newly quarantined rows never submit.
    const liveRow = currentWorkspace.rows.find((candidate) => candidate.id === row.id)
    const liveArtifact = liveRow ? currentWorkspace.artifacts.find((item) => item.id === liveRow.artifactId) : undefined
    const expectedBasisKey = rowBasisKey(row, artifact)
    const liveBasisKey = liveRow ? rowBasisKey(liveRow, liveArtifact) : null
    if (
      !liveRow
      || !liveArtifact
      || liveBasisKey !== expectedBasisKey
    ) {
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        rowOutcomes: {
          ...curr.rowOutcomes,
          [row.id]: { kind: "stale", message: "Row evidence, capture digests, or baseline values changed before dispatch — review and promote again." },
        },
      }))
      return "stale"
    }

    const currentAliases = new Map<string, AliasResolution>()
    for (const alias of currentWorkspace.aliases) {
      if (alias.resolution !== null) currentAliases.set(alias.id, alias.resolution)
    }
    const resolved = resolveRowEntity(liveRow, currentAliases)
    if (expectedTargetRecordId !== undefined && (resolved.kind !== "resolved" || resolved.targetId !== expectedTargetRecordId)) {
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        rowOutcomes: {
          ...curr.rowOutcomes,
          [row.id]: { kind: "stale", message: "Row entity resolution changed before dispatch — review and promote again." },
        },
      }))
      return "stale"
    }
    if (resolved.kind !== "resolved" || effectiveRowStatus(liveRow) !== "imported_unverified") {
      return "skipped"
    }

    const values = snapshotValues ?? extractEditableValues(liveRow, currentDraft.reviewEdits, liveBasisKey)
    const payload = {
      batchId: requestBatchId,
      rowId: liveRow.id,
      artifactId: liveArtifact.id,
      blobDigest: liveArtifact.blobDigest,
      functionReceiptDigest: liveArtifact.functionReceiptDigest,
      targetRecordId: resolved.targetId,
      effectiveAt: liveRow.effectiveAt,
      observedAt: liveRow.observedAt,
      values,
    }
    const key = resolvePayloadIdempotencyKey(
      payloadKeyCacheRef.current,
      `row:${requestBatchId}:${liveRow.id}`,
      currentWorkspace.actions.promoteRow,
      payload,
    )

    locks.add(liveRow.id)
    patchBatchDraft(requestBatchId, (curr) => {
      const nextBusy = new Set(curr.busyRowIds)
      nextBusy.add(liveRow.id)
      const nextOutcomes = { ...curr.rowOutcomes }
      delete nextOutcomes[liveRow.id]
      return {
        ...curr,
        busyRowIds: nextBusy,
        rowOutcomes: nextOutcomes,
      }
    })

    try {
      const rawResult = await commitRowPromotion(currentWorkspace.actions.promoteRow, payload, key, undefined, pinnedScope)
      const classified = classifyTriageActionResult(rawResult)
      if (classified.kind === "receipt") {
        // Never optimistically mark submitted values as "Verified fact" from a receipt alone.
        // Record the commit receipt in rowOutcomes and let the authoritative refetched cell read
        // supply committed field values and Verified fact status.
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          rowOutcomes: {
            ...curr.rowOutcomes,
            [liveRow.id]: { kind: "committed", receipt: classified.receipt },
          },
        }))
        if (mountedRef.current && getLiveWorkspace()?.batchId === requestBatchId) {
          void client.invalidateQueries({ queryKey: QUERY_KEY })
        }
        return "promoted"
      }
      if (classified.kind === "pending") {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          rowOutcomes: {
            ...curr.rowOutcomes,
            [liveRow.id]: { kind: "pending", decisionId: classified.decisionId, expiresAtMs: classified.expiresAtMs },
          },
        }))
        return "pending"
      }
      if (classified.kind === "refused") {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          rowOutcomes: {
            ...curr.rowOutcomes,
            [liveRow.id]: { kind: "refused", code: classified.code, message: classified.message },
          },
        }))
        return "refused"
      }
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        rowOutcomes: {
          ...curr.rowOutcomes,
          [liveRow.id]: { kind: "error", message: classified.message },
        },
      }))
      return "error"
    } catch (error) {
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        rowOutcomes: {
          ...curr.rowOutcomes,
          [liveRow.id]: {
            kind: "error",
            message: error instanceof Error ? error.message : "The cell did not respond. Retry reuses the same idempotency key.",
          },
        },
      }))
      return "error"
    } finally {
      locks.delete(liveRow.id)
      patchBatchDraft(requestBatchId, (curr) => {
        const nextBusy = new Set(curr.busyRowIds)
        nextBusy.delete(liveRow.id)
        const nextQueued = new Set(curr.queuedRowIds)
        nextQueued.delete(liveRow.id)
        return { ...curr, busyRowIds: nextBusy, queuedRowIds: nextQueued }
      })
    }
  }

  const handlePromoteRow = async (
    requestBatchId: string,
    row: CandidateRow,
    artifact: StagedArtifact,
    expectedTargetRecordId?: string,
  ) => {
    if (!mountedRef.current || batchRunningRef.current.has(requestBatchId)) return
    await dispatchPromoteRow(requestBatchId, row, artifact, undefined, expectedTargetRecordId)
  }

  const handlePromoteCleanBatch = async (
    requestBatchId: string,
    renderedQueue: readonly QueuedPromotionItem[],
    keptQuarantined: number,
  ) => {
    if (!mountedRef.current) return
    const locks = getLockSet(inFlightRowsRef.current, requestBatchId)
    if (renderedQueue.length === 0 || batchRunningRef.current.has(requestBatchId) || locks.size > 0) return

    const currentDraft = draftsRef.current[requestBatchId] ?? EMPTY_BATCH_DRAFT
    const lockedIds = batchLockedRowIds(locks, currentDraft.queuedRowIds, currentDraft.rowOutcomes)
    const queue = renderedQueue.filter((item) => !lockedIds.has(item.rowId))
    if (queue.length === 0) return

    batchRunningRef.current.add(requestBatchId)
    patchBatchDraft(requestBatchId, (curr) => ({
      ...curr,
      batchRunning: true,
      queuedRowIds: new Set(queue.map((item) => item.rowId)),
      batchReport: null,
    }))

    try {
      const report = await runBatchPromotionQueue(
        requestBatchId,
        queue,
        () => getLiveWorkspace()?.batchId,
        keptQuarantined,
        async (item) => {
          const liveOutcome = draftsRef.current[requestBatchId]?.rowOutcomes[item.rowId]
          if (isOutcomeLocked(liveOutcome)) {
            patchBatchDraft(requestBatchId, (curr) => {
              const nextQueued = new Set(curr.queuedRowIds)
              nextQueued.delete(item.rowId)
              return { ...curr, queuedRowIds: nextQueued }
            })
            return "skipped"
          }
          const basis = verifyQueuedPromotionBasis(getLiveWorkspace(), requestBatchId, item)
          if (basis.kind === "stale") {
            patchBatchDraft(requestBatchId, (curr) => {
              const nextQueued = new Set(curr.queuedRowIds)
              nextQueued.delete(item.rowId)
              return {
                ...curr,
                queuedRowIds: nextQueued,
                rowOutcomes: {
                  ...curr.rowOutcomes,
                  [item.rowId]: { kind: "stale", message: basis.reason },
                },
              }
            })
            return "stale"
          }
          return dispatchPromoteRow(requestBatchId, basis.row, basis.artifact, item.values, basis.targetRecordId)
        },
        () => mountedRef.current,
      )
      if (!report.aborted) {
        patchBatchDraft(requestBatchId, (curr) => ({ ...curr, batchReport: report }))
      }
    } finally {
      batchRunningRef.current.delete(requestBatchId)
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        batchRunning: false,
        queuedRowIds: EMPTY_SET,
      }))
    }
  }

  // ---- Step 3: Supersession Diffs -------------------------------------------------------------

  const handlePatchSupersessionDraft = (
    expectedBatchId: string,
    conflict: SupersessionConflict,
    update: Partial<SupersessionDraftState>,
  ) => {
    if (!mountedRef.current || getLiveWorkspace()?.batchId !== expectedBatchId) return
    patchBatchDraft(expectedBatchId, (current) => {
      const nextDraft = patchSupersessionDraftState(current.supersessions[conflict.id], conflict, update)
      return { ...current, supersessions: { ...current.supersessions, [conflict.id]: nextDraft } }
    })
  }

  const handleCommitSupersession = async (requestBatchId: string, conflict: SupersessionConflict) => {
    if (!mountedRef.current) return
    const locks = getLockSet(inFlightSupersessionsRef.current, requestBatchId)
    if (locks.has(conflict.id)) return
    const currentWorkspace = getLiveWorkspace()
    if (!currentWorkspace || currentWorkspace.batchId !== requestBatchId) return

    const currentDraft = draftsRef.current[requestBatchId]?.supersessions[conflict.id] ?? initialSupersessionDraft(conflict)
    const trimmed = currentDraft.note.trim()
    const payload = {
      batchId: requestBatchId,
      conflictId: conflict.id,
      choice: currentDraft.choice,
      earlierArtifactId: conflict.earlier.artifactId,
      earlierBlobDigest: conflict.earlier.blobDigest,
      newerArtifactId: conflict.newer.artifactId,
      newerBlobDigest: conflict.newer.blobDigest,
      preserveEarlierEvidence: true as const,
      ...trimmed !== "" ? { note: trimmed } : {},
    }
    const key = resolvePayloadIdempotencyKey(
      payloadKeyCacheRef.current,
      `supersession:${requestBatchId}:${conflict.id}`,
      currentWorkspace.actions.resolveSupersession,
      payload,
    )

    locks.add(conflict.id)
    patchBatchDraft(requestBatchId, (curr) => ({
      ...curr,
      supersessions: {
        ...curr.supersessions,
        [conflict.id]: patchSupersessionDraftState(curr.supersessions[conflict.id], conflict, {
          idempotencyKey: key,
          pending: true,
          notice: null,
        }),
      },
    }))

    try {
      const rawResult = await commitSupersessionDecision(currentWorkspace.actions.resolveSupersession, payload, key, undefined, pinnedScope)
      const classified = classifyTriageActionResult(rawResult)
      if (classified.kind === "receipt") {
        const resolution: SupersessionResolution = {
          choice: currentDraft.choice,
          ...trimmed !== "" ? { note: trimmed } : {},
          preservedHistoricalArtifactId: currentDraft.choice === "keep_earlier" ? conflict.newer.artifactId : conflict.earlier.artifactId,
          receipt: classified.receipt,
        }
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          supersessions: {
            ...curr.supersessions,
            [conflict.id]: patchSupersessionDraftState(curr.supersessions[conflict.id], conflict, {
              idempotencyKey: key,
              pending: false,
              notice: null,
            }),
          },
        }))
        updateWorkspace(requestBatchId, (curr) => ({
          ...curr,
          supersessions: curr.supersessions.map((item) => item.id === conflict.id ? { ...item, resolution } : item),
        }))
        if (mountedRef.current && getLiveWorkspace()?.batchId === requestBatchId) {
          void client.invalidateQueries({ queryKey: QUERY_KEY })
        }
      } else if (classified.kind === "pending") {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          supersessions: {
            ...curr.supersessions,
            [conflict.id]: patchSupersessionDraftState(curr.supersessions[conflict.id], conflict, {
              idempotencyKey: key,
              pending: false,
              notice: { kind: "pending", message: `Pending approval decision (${classified.decisionId}).` },
            }),
          },
        }))
      } else if (classified.kind === "refused") {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          supersessions: {
            ...curr.supersessions,
            [conflict.id]: patchSupersessionDraftState(curr.supersessions[conflict.id], conflict, {
              idempotencyKey: key,
              pending: false,
              notice: { kind: "refused", message: `${classified.code}: ${classified.message}` },
            }),
          },
        }))
      } else {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          supersessions: {
            ...curr.supersessions,
            [conflict.id]: patchSupersessionDraftState(curr.supersessions[conflict.id], conflict, {
              idempotencyKey: key,
              pending: false,
              notice: { kind: "error", message: classified.message },
            }),
          },
        }))
      }
    } catch (error) {
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        supersessions: {
          ...curr.supersessions,
          [conflict.id]: patchSupersessionDraftState(curr.supersessions[conflict.id], conflict, {
            idempotencyKey: key,
            pending: false,
            notice: { kind: "error", message: error instanceof Error ? error.message : "The cell did not respond. Retry reuses the same idempotency key." },
          }),
        },
      }))
    } finally {
      locks.delete(conflict.id)
    }
  }

  const refresh = () => {
    void client.invalidateQueries({ queryKey: QUERY_KEY })
    const activeBatchId = getLiveWorkspace()?.batchId ?? workspaceRef.current?.batchId
    if (activeBatchId) void reconcilePendingOutcomes(activeBatchId)
  }

  return (
    <div className="space-y-4">
      <PageHeader
        title="Import triage"
        description="Inspect staged captures beside source evidence, resolve external aliases, and promote clean rows through governed cell laws."
        meta={workspace ? (
          <>
            <Chip tone="gray" mono>{workspace.targetObjectType}</Chip>
            <span className="text-meta text-muted-foreground"><bdi>{workspace.title}</bdi></span>
          </>
        ) : undefined}
        actions={
          <Button variant="outline" size="sm" onClick={refresh} disabled={read.isFetching} className="h-8 gap-1.5 border-border bg-background text-meta shadow-none">
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
        <div role="alert" className="flex flex-wrap items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive">
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
        <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-body text-destructive">
          {read.data.refusal.message}
        </p>
      )}

      {workspace && summary && (
        <div className="space-y-3" data-triage-batch={workspace.batchId}>
          {/* Progressive go-live honest status strip */}
          <div
            aria-label="Progressive verification status"
            className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-surface-subtle px-3.5 py-2"
          >
            <div className="flex flex-wrap items-center gap-2 text-meta tabular-nums">
              <span className="inline-flex items-center gap-1.5">
                <FactStatusBadge status="verified_fact" />
                <span className="text-foreground">{summary.verifiedRows}</span>
              </span>
              <span className="text-border">|</span>
              <span className="inline-flex items-center gap-1.5">
                <FactStatusBadge status="imported_unverified" />
                <span className="text-foreground">{summary.unverifiedRows}</span>
              </span>
              <span className="text-border">|</span>
              <span className="inline-flex items-center gap-1.5">
                <FactStatusBadge status="quarantined" />
                <span className="text-foreground">{summary.quarantinedRows}</span>
              </span>
              {summary.withheldRows > 0 && (
                <>
                  <span className="text-border">|</span>
                  <span className="inline-flex items-center gap-1.5">
                    <FactStatusBadge status="withheld" />
                    <span className="text-foreground">{summary.withheldRows}</span>
                  </span>
                </>
              )}
            </div>
            <span className="inline-flex items-center gap-1.5 text-meta text-muted-foreground">
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
                    "inline-flex h-9 items-center gap-2 rounded-md border px-3 text-meta transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    active
                      ? "border-foreground/25 bg-background text-foreground"
                      : "border-transparent text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                  )}
                >
                  <span className={cn(
                    "flex size-5 items-center justify-center rounded text-meta tabular-nums",
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
              drafts={batchDraft.aliases}
              onPatchDraft={handlePatchAliasDraft}
              onCommitAlias={handleCommitAlias}
            />
          )}

          {step === "review" && (
            <SplitPaneReview
              key={workspace.batchId}
              batchId={workspace.batchId}
              artifacts={workspace.artifacts}
              rows={workspace.rows}
              lowConfidenceThresholdBps={workspace.lowConfidenceThresholdBps}
              resolvedAliases={resolvedAliases}
              edits={batchDraft.reviewEdits}
              rowOutcomes={batchDraft.rowOutcomes}
              busyRowIds={batchDraft.busyRowIds}
              queuedRowIds={batchDraft.queuedRowIds}
              batchRunning={batchDraft.batchRunning}
              batchReport={batchDraft.batchReport}
              onSetFieldValue={handleSetFieldValue}
              onResetFieldValue={handleResetFieldValue}
              onPromoteRow={handlePromoteRow}
              onPromoteCleanBatch={handlePromoteCleanBatch}
              onJumpToAliases={() => setStep("aliases")}
            />
          )}

          {step === "supersession" && (
            <SupersessionDiffList
              key={workspace.batchId}
              batchId={workspace.batchId}
              conflicts={workspace.supersessions}
              lowConfidenceThresholdBps={workspace.lowConfidenceThresholdBps}
              drafts={batchDraft.supersessions}
              onPatchDraft={handlePatchSupersessionDraft}
              onCommitSupersession={handleCommitSupersession}
            />
          )}
        </div>
      )}
    </div>
  )
}
