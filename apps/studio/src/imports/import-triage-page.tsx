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
  classifyTriageActionResult,
  commitAliasBinding,
  commitAliasIgnore,
  commitEntityCreateAndBind,
  commitRowPromotion,
  commitSupersessionDecision,
  effectiveRowStatus,
  extractEditableValues,
  fetchImportTriage,
  IMPORT_TRIAGE_UNAVAILABLE,
  initialAliasDraft,
  initialSupersessionDraft,
  isFieldWithheld,
  isTriageStep,
  patchAliasDraftState,
  patchSupersessionDraftState,
  resolveRowEntity,
  runBatchPromotionQueue,
  triageSummary,
  type AliasCandidate,
  type AliasDraftState,
  type AliasResolution,
  type BatchRunReport,
  type CandidateRow,
  type ImportTriageWorkspace,
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

const QUERY_KEY = ["imports", "triage"] as const

/** Unsent drafts, in-flight/queued promotion locks, idempotency keys, and request outcomes scoped to one `batchId`. */
interface BatchDraftState {
  readonly aliases: Readonly<Record<string, AliasDraftState>>
  readonly reviewEdits: Readonly<Record<string, string>>
  readonly rowKeys: Readonly<Record<string, string>>
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
  rowKeys: {},
  rowOutcomes: {},
  busyRowIds: EMPTY_SET,
  queuedRowIds: EMPTY_SET,
  batchRunning: false,
  batchReport: null,
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
  const activeBatchIdRef = useRef<string | undefined>(workspace?.batchId)
  const workspaceRef = useRef<ImportTriageWorkspace | undefined>(workspace)
  activeBatchIdRef.current = workspace?.batchId
  workspaceRef.current = workspace

  const [draftsByBatch, setDraftsByBatch] = useState<Readonly<Record<string, BatchDraftState>>>({})
  const draftsRef = useRef<Readonly<Record<string, BatchDraftState>>>(draftsByBatch)
  draftsRef.current = draftsByBatch

  // Synchronous per-batch in-flight locks survive step unmounts and rapid double-clicks.
  const inFlightRowsRef = useRef<Map<string, Set<string>>>(new Map())
  const inFlightAliasesRef = useRef<Map<string, Set<string>>>(new Map())
  const inFlightSupersessionsRef = useRef<Map<string, Set<string>>>(new Map())
  const batchRunningRef = useRef<Set<string>>(new Set())

  // Synchronous idempotency key caches prevent two concurrent/first calls from minting distinct keys.
  const stableRowKeysRef = useRef<Map<string, string>>(new Map())
  const stableAliasKeysRef = useRef<Map<string, string>>(new Map())
  const stableSupersessionKeysRef = useRef<Map<string, string>>(new Map())

  const batchDraft = workspace ? (draftsByBatch[workspace.batchId] ?? EMPTY_BATCH_DRAFT) : EMPTY_BATCH_DRAFT

  // Clear stale local field edits whenever a refetch marks a row verified by the cell.
  useEffect(() => {
    if (!workspace) return
    const verifiedIds = new Set(workspace.rows.filter((r) => effectiveRowStatus(r) === "verified_fact").map((r) => r.id))
    if (verifiedIds.size === 0) return
    setDraftsByBatch((all) => {
      const current = all[workspace.batchId]
      if (!current) return all
      const staleKeys = Object.keys(current.reviewEdits).filter((k) => verifiedIds.has(k.split(":", 1)[0] ?? ""))
      if (staleKeys.length === 0) return all
      const nextEdits = { ...current.reviewEdits }
      for (const k of staleKeys) delete nextEdits[k]
      return { ...all, [workspace.batchId]: { ...current, reviewEdits: nextEdits } }
    })
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

  /** Apply a cache update strictly when the active batch still matches `expectedBatchId`. */
  const updateWorkspace = (expectedBatchId: string, updater: (current: ImportTriageWorkspace) => ImportTriageWorkspace) => {
    if (activeBatchIdRef.current !== expectedBatchId) return
    client.setQueryData<CellResult<ImportTriageWorkspace>>(QUERY_KEY, (current) => {
      if (current?.kind !== "ok" || current.value.batchId !== expectedBatchId) return current
      return { ...current, value: updater(current.value) }
    })
  }

  const patchBatchDraft = (expectedBatchId: string, updater: (current: BatchDraftState) => BatchDraftState) => {
    setDraftsByBatch((all) => {
      const current = all[expectedBatchId] ?? EMPTY_BATCH_DRAFT
      return { ...all, [expectedBatchId]: updater(current) }
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
    if (activeBatchIdRef.current !== expectedBatchId) return
    patchBatchDraft(expectedBatchId, (current) => {
      const nextDraft = patchAliasDraftState(current.aliases[alias.id], alias, update)
      if (nextDraft.idempotencyKey === null) {
        stableAliasKeysRef.current.delete(`${expectedBatchId}:${alias.id}`)
      }
      return { ...current, aliases: { ...current.aliases, [alias.id]: nextDraft } }
    })
  }

  const handleCommitAlias = async (requestBatchId: string, alias: AliasCandidate) => {
    const locks = getLockSet(inFlightAliasesRef.current, requestBatchId)
    if (locks.has(alias.id)) return
    const currentWorkspace = workspaceRef.current
    if (!currentWorkspace || currentWorkspace.batchId !== requestBatchId) return

    const currentDraft = draftsRef.current[requestBatchId]?.aliases[alias.id] ?? initialAliasDraft(alias)
    const composite = `${requestBatchId}:${alias.id}`
    const key = currentDraft.idempotencyKey ?? stableAliasKeysRef.current.get(composite) ?? newIdempotencyKey()
    stableAliasKeysRef.current.set(composite, key)

    if (currentDraft.mode === "match") {
      const target = currentWorkspace.availableEntities.find((entity) => entity.id === currentDraft.selectedTargetId)
      if (!target) {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          aliases: {
            ...curr.aliases,
            [alias.id]: patchAliasDraftState(curr.aliases[alias.id], alias, {
              idempotencyKey: key,
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
            idempotencyKey: key,
            notice: { kind: "refused", message: "Enter a record label before creating and binding." },
          }),
        },
      }))
      return
    }

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
      let createdEntity: TargetEntity | undefined
      let makeResolution: (receipt: ActionReceipt) => AliasResolution

      if (currentDraft.mode === "match") {
        const target = currentWorkspace.availableEntities.find((entity) => entity.id === currentDraft.selectedTargetId)!
        rawResult = await commitAliasBinding(currentWorkspace.actions.bindAlias, {
          batchId: requestBatchId,
          aliasId: alias.id,
          externalHandle: alias.externalHandle,
          targetRecordId: target.id,
        }, key)
        makeResolution = (receipt) => ({ kind: "bound", targetId: target.id, targetLabel: target.label, receipt })
      } else if (currentDraft.mode === "create") {
        const label = currentDraft.createLabel.trim()
        const externalKey = currentDraft.createExternalKey.trim()
        rawResult = await commitEntityCreateAndBind(currentWorkspace.actions.createEntity, {
          batchId: requestBatchId,
          aliasId: alias.id,
          externalHandle: alias.externalHandle,
          label,
          ...externalKey !== "" ? { externalKey } : {},
        }, key)
        makeResolution = (receipt) => {
          createdEntity = { id: receipt.objectId, label, subtitle: externalKey || undefined }
          return { kind: "created", targetId: receipt.objectId, targetLabel: label, receipt }
        }
      } else {
        const note = currentDraft.ignoreNote.trim()
        rawResult = await commitAliasIgnore(currentWorkspace.actions.ignoreAlias, {
          batchId: requestBatchId,
          aliasId: alias.id,
          externalHandle: alias.externalHandle,
          ...note !== "" ? { note } : {},
        }, key)
        makeResolution = (receipt) => ({ kind: "ignored", ...note !== "" ? { note } : {}, receipt })
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
        if (activeBatchIdRef.current === requestBatchId) {
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

  // ---- Step 2: Review & Batch Promotion (locks + queue survive step switches & abort on batch change) ----

  const handleSetFieldValue = (
    expectedBatchId: string,
    rowId: string,
    fieldName: string,
    currentValue: string,
    nextValue: string,
  ) => {
    if (activeBatchIdRef.current !== expectedBatchId) return
    if (currentValue === nextValue) return
    const freshKey = newIdempotencyKey()
    stableRowKeysRef.current.set(`${expectedBatchId}:${rowId}`, freshKey)
    patchBatchDraft(expectedBatchId, (current) => {
      const nextOutcomes = { ...current.rowOutcomes }
      delete nextOutcomes[rowId]
      return {
        ...current,
        reviewEdits: { ...current.reviewEdits, [`${rowId}:${fieldName}`]: nextValue },
        rowKeys: { ...current.rowKeys, [rowId]: freshKey },
        rowOutcomes: nextOutcomes,
      }
    })
  }

  const handleResetFieldValue = (expectedBatchId: string, rowId: string, fieldName: string) => {
    if (activeBatchIdRef.current !== expectedBatchId) return
    const freshKey = newIdempotencyKey()
    stableRowKeysRef.current.set(`${expectedBatchId}:${rowId}`, freshKey)
    patchBatchDraft(expectedBatchId, (current) => {
      const nextEdits = { ...current.reviewEdits }
      delete nextEdits[`${rowId}:${fieldName}`]
      const nextOutcomes = { ...current.rowOutcomes }
      delete nextOutcomes[rowId]
      return {
        ...current,
        reviewEdits: nextEdits,
        rowKeys: { ...current.rowKeys, [rowId]: freshKey },
        rowOutcomes: nextOutcomes,
      }
    })
  }

  const dispatchPromoteRow = async (
    requestBatchId: string,
    row: CandidateRow,
    artifact: StagedArtifact,
    snapshotValues?: Readonly<Record<string, string>>,
  ): Promise<"promoted" | "refused" | "pending" | "error" | "skipped"> => {
    const locks = getLockSet(inFlightRowsRef.current, requestBatchId)
    if (locks.has(row.id)) return "skipped"
    const currentWorkspace = workspaceRef.current
    if (!currentWorkspace || currentWorkspace.batchId !== requestBatchId) return "skipped"

    const currentAliases = new Map<string, AliasResolution>()
    for (const alias of currentWorkspace.aliases) {
      if (alias.resolution !== null) currentAliases.set(alias.id, alias.resolution)
    }
    const resolved = resolveRowEntity(row, currentAliases)
    if (resolved.kind !== "resolved" || effectiveRowStatus(row) !== "imported_unverified") {
      return "skipped"
    }

    const currentDraft = draftsRef.current[requestBatchId] ?? EMPTY_BATCH_DRAFT
    const values = snapshotValues ?? extractEditableValues(row, currentDraft.reviewEdits)
    const composite = `${requestBatchId}:${row.id}`
    const key = currentDraft.rowKeys[row.id] ?? stableRowKeysRef.current.get(composite) ?? newIdempotencyKey()
    stableRowKeysRef.current.set(composite, key)

    locks.add(row.id)
    patchBatchDraft(requestBatchId, (curr) => {
      const nextBusy = new Set(curr.busyRowIds)
      nextBusy.add(row.id)
      const nextOutcomes = { ...curr.rowOutcomes }
      delete nextOutcomes[row.id]
      return {
        ...curr,
        busyRowIds: nextBusy,
        rowKeys: curr.rowKeys[row.id] === key ? curr.rowKeys : { ...curr.rowKeys, [row.id]: key },
        rowOutcomes: nextOutcomes,
      }
    })

    try {
      const rawResult = await commitRowPromotion(currentWorkspace.actions.promoteRow, {
        batchId: requestBatchId,
        rowId: row.id,
        artifactId: artifact.id,
        blobDigest: artifact.blobDigest,
        functionReceiptDigest: artifact.functionReceiptDigest,
        targetRecordId: resolved.targetId,
        effectiveAt: row.effectiveAt,
        observedAt: row.observedAt,
        values,
      }, key)
      const classified = classifyTriageActionResult(rawResult)
      if (classified.kind === "receipt") {
        const recordedAt = new Date(classified.receipt.receipt.recordedAtMs).toISOString()
        updateWorkspace(requestBatchId, (curr) => ({
          ...curr,
          rows: curr.rows.map((candidate) => candidate.id !== row.id ? candidate : {
            ...candidate,
            status: "verified_fact",
            recordedAt,
            receipt: classified.receipt,
            fields: candidate.fields.map((field) => isFieldWithheld(field) ? field : {
              ...field,
              value: values[field.name] ?? field.value,
            }),
          }),
        }))
        patchBatchDraft(requestBatchId, (curr) => {
          const nextOutcomes = { ...curr.rowOutcomes }
          delete nextOutcomes[row.id]
          const nextEdits = { ...curr.reviewEdits }
          for (const fieldName of Object.keys(values)) delete nextEdits[`${row.id}:${fieldName}`]
          return { ...curr, reviewEdits: nextEdits, rowOutcomes: nextOutcomes }
        })
        if (activeBatchIdRef.current === requestBatchId) {
          void client.invalidateQueries({ queryKey: QUERY_KEY })
        }
        return "promoted"
      }
      if (classified.kind === "pending") {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          rowOutcomes: {
            ...curr.rowOutcomes,
            [row.id]: { kind: "pending", decisionId: classified.decisionId, expiresAtMs: classified.expiresAtMs },
          },
        }))
        return "pending"
      }
      if (classified.kind === "refused") {
        patchBatchDraft(requestBatchId, (curr) => ({
          ...curr,
          rowOutcomes: {
            ...curr.rowOutcomes,
            [row.id]: { kind: "refused", code: classified.code, message: classified.message },
          },
        }))
        return "refused"
      }
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        rowOutcomes: {
          ...curr.rowOutcomes,
          [row.id]: { kind: "error", message: classified.message },
        },
      }))
      return "error"
    } catch (error) {
      patchBatchDraft(requestBatchId, (curr) => ({
        ...curr,
        rowOutcomes: {
          ...curr.rowOutcomes,
          [row.id]: {
            kind: "error",
            message: error instanceof Error ? error.message : "The cell did not respond. Retry reuses the same idempotency key.",
          },
        },
      }))
      return "error"
    } finally {
      locks.delete(row.id)
      patchBatchDraft(requestBatchId, (curr) => {
        const nextBusy = new Set(curr.busyRowIds)
        nextBusy.delete(row.id)
        const nextQueued = new Set(curr.queuedRowIds)
        nextQueued.delete(row.id)
        return { ...curr, busyRowIds: nextBusy, queuedRowIds: nextQueued }
      })
    }
  }

  const handlePromoteRow = async (requestBatchId: string, row: CandidateRow, artifact: StagedArtifact) => {
    if (batchRunningRef.current.has(requestBatchId)) return
    await dispatchPromoteRow(requestBatchId, row, artifact)
  }

  const handlePromoteCleanBatch = async (
    requestBatchId: string,
    eligibleRows: readonly CandidateRow[],
    keptQuarantined: number,
  ) => {
    const locks = getLockSet(inFlightRowsRef.current, requestBatchId)
    if (eligibleRows.length === 0 || batchRunningRef.current.has(requestBatchId) || locks.size > 0) return
    const currentWorkspace = workspaceRef.current
    if (!currentWorkspace || currentWorkspace.batchId !== requestBatchId) return

    const currentDraft = draftsRef.current[requestBatchId] ?? EMPTY_BATCH_DRAFT
    const queue = eligibleRows.flatMap((row) => {
      const artifact = currentWorkspace.artifacts.find((item) => item.id === row.artifactId) ?? currentWorkspace.artifacts[0]
      if (!artifact) return []
      return [{ row, artifact, values: extractEditableValues(row, currentDraft.reviewEdits) }]
    })
    if (queue.length === 0) return

    batchRunningRef.current.add(requestBatchId)
    patchBatchDraft(requestBatchId, (curr) => ({
      ...curr,
      batchRunning: true,
      queuedRowIds: new Set(queue.map(({ row }) => row.id)),
      batchReport: null,
    }))

    try {
      const report = await runBatchPromotionQueue(
        requestBatchId,
        queue,
        () => activeBatchIdRef.current,
        keptQuarantined,
        (item) => dispatchPromoteRow(requestBatchId, item.row, item.artifact, item.values),
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
    if (activeBatchIdRef.current !== expectedBatchId) return
    patchBatchDraft(expectedBatchId, (current) => {
      const nextDraft = patchSupersessionDraftState(current.supersessions[conflict.id], conflict, update)
      if (nextDraft.idempotencyKey === null) {
        stableSupersessionKeysRef.current.delete(`${expectedBatchId}:${conflict.id}`)
      }
      return { ...current, supersessions: { ...current.supersessions, [conflict.id]: nextDraft } }
    })
  }

  const handleCommitSupersession = async (requestBatchId: string, conflict: SupersessionConflict) => {
    const locks = getLockSet(inFlightSupersessionsRef.current, requestBatchId)
    if (locks.has(conflict.id)) return
    const currentWorkspace = workspaceRef.current
    if (!currentWorkspace || currentWorkspace.batchId !== requestBatchId) return

    const currentDraft = draftsRef.current[requestBatchId]?.supersessions[conflict.id] ?? initialSupersessionDraft(conflict)
    const composite = `${requestBatchId}:${conflict.id}`
    const key = currentDraft.idempotencyKey ?? stableSupersessionKeysRef.current.get(composite) ?? newIdempotencyKey()
    stableSupersessionKeysRef.current.set(composite, key)

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
      const trimmed = currentDraft.note.trim()
      const rawResult = await commitSupersessionDecision(currentWorkspace.actions.resolveSupersession, {
        batchId: requestBatchId,
        conflictId: conflict.id,
        choice: currentDraft.choice,
        earlierArtifactId: conflict.earlier.artifactId,
        earlierBlobDigest: conflict.earlier.blobDigest,
        newerArtifactId: conflict.newer.artifactId,
        newerBlobDigest: conflict.newer.blobDigest,
        preserveEarlierEvidence: true,
        ...trimmed !== "" ? { note: trimmed } : {},
      }, key)
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
        if (activeBatchIdRef.current === requestBatchId) {
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
        <div className="space-y-3" data-triage-batch={workspace.batchId}>
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
