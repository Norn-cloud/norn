import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { renderToStaticMarkup } from "react-dom/server"
import { deriveFactStatus, FactStatusBadge } from "../src/components/fact-status-badge.tsx"
import {
  batchLockedRowIds,
  canonicalPayloadKey,
  classifyTriageActionResult,
  commitAliasBinding,
  commitRowPromotion,
  commitSupersessionDecision,
  effectiveRowStatus,
  extractEditableValues,
  fetchImportTriage,
  fetchRequestedDecisionForReconciliation,
  fieldEditKey,
  formatConfidence,
  formatTemporal,
  hasOpenLawViolation,
  isLowConfidence,
  isOutcomeLocked,
  isRowEligibleForBatchPromotion,
  parseRequestedDecisionLookup,
  partitionPromotionBatch,
  patchAliasDraftState,
  patchSupersessionDraftState,
  readLiveWorkspaceFromCache,
  reconcilePendingRowOutcome,
  resolveArtifactMediaSource,
  resolvePayloadIdempotencyKey,
  resolveRowEntity,
  retirePayloadIdempotencyKey,
  rowBasisKey,
  runBatchPromotionQueue,
  snapshotPromotionQueue,
  supersessionFactStatus,
  timeDeltaLabel,
  triageSummary,
  verifyQueuedPromotionBasis,
  type AliasCandidate,
  type AliasResolution,
  type CandidateRow,
  type ImportTriageWorkspace,
  type PayloadBoundKeyEntry,
  type QueuedPromotionItem,
  type StagedArtifact,
  type SupersessionConflict,
} from "../src/lib/import-triage.ts"

const sampleReceipt = {
  outcome: "committed" as const,
  objectType: "canon.Allocation",
  objectId: "alloc-01",
  version: 1,
  receipt: { receiptHash: "a".repeat(64), commitSeq: 41, recordedAtMs: Date.UTC(2026, 10, 23, 12, 0, 0) },
}

const sampleAlias: AliasCandidate = {
  id: "alias-1",
  externalHandle: "@handle_north_1",
  sourceChannel: "channel-ops",
  occurrences: 3,
  sampleExcerpt: "Snapshot posted for W47.",
  firstObservedAt: "2026-11-16T08:30:00Z",
  suggestedTargetId: "ent-002",
  suggestedTargetLabel: "Unit Beta",
  suggestionConfidenceBps: 8100,
  resolution: null,
}

const sampleConflict: SupersessionConflict = {
  id: "sup-1",
  findingCode: "FEED-CONFLICT",
  windowLabel: "2026-W47",
  scopeKey: "zone-south/2026-W47",
  earlier: {
    artifactId: "art-1",
    label: "Snapshot A",
    blobDigest: "sha256:" + "1".repeat(64),
    functionReceiptDigest: "sha256:" + "2".repeat(64),
    observedAt: "2026-11-16T09:00:00Z",
    sourceChannel: "channel-ops",
  },
  newer: {
    artifactId: "art-2",
    label: "Snapshot B",
    blobDigest: "sha256:" + "3".repeat(64),
    functionReceiptDigest: "sha256:" + "4".repeat(64),
    observedAt: "2026-11-16T11:15:00Z",
    sourceChannel: "channel-ops",
  },
  cells: [],
  defaultChoice: "accept_newer",
  resolution: null,
}

const cleanRow: CandidateRow = {
  id: "row-clean",
  artifactId: "art-1",
  targetObjectType: "canon.Allocation",
  windowLabel: "2026-W47",
  entityAliasId: null,
  entityRecordId: "ent-001",
  entityLabel: "Unit Alpha",
  status: "imported_unverified",
  effectiveAt: "2026-11-16T08:00:00Z",
  observedAt: "2026-11-16T09:15:00Z",
  recordedAt: null,
  fields: [
    { id: "f1", name: "slot_code", label: "Slot code", extractedValue: "S-01", value: "S-01", confidenceBps: 9820, box: null, findingIds: [] },
    { id: "f2", name: "duration_hours", label: "Duration", extractedValue: "8.0", value: "8.0", confidenceBps: 7200, box: null, findingIds: [] },
  ],
  findings: [],
  ambiguities: [],
  receipt: null,
}

const aliasBlockedRow: CandidateRow = {
  ...cleanRow,
  id: "row-alias",
  entityAliasId: "alias-1",
  // Even a pre-populated entityRecordId MUST NOT bypass governed alias resolution.
  entityRecordId: "ent-prepopulated-guess",
  entityLabel: "@handle_north_1",
}

const violatingRow: CandidateRow = {
  ...cleanRow,
  id: "row-violation",
  status: "quarantined",
  findings: [
    {
      id: "find-1",
      code: "OBS-BAD",
      ruleId: "canon.allocation.non_overlapping_window",
      severity: "error",
      message: "Overlapping allocation window for the same record.",
      fieldName: "slot_code",
      status: "open",
    },
  ],
}

const withheldRow: CandidateRow = {
  ...cleanRow,
  id: "row-withheld",
  status: "verified_fact",
  fields: [
    { id: "f1", name: "slot_code", label: "Slot code", extractedValue: "S-01", value: "S-01", confidenceBps: 9900, box: null, findingIds: [] },
    { kind: "withheld", id: "f-secret", name: "contact_note", label: "Contact note", masking: "redacted", displayMask: "•••", markings: ["pii.contact"], confidenceBps: null, box: null, findingIds: [] },
  ],
}

const verifiedRow: CandidateRow = {
  ...cleanRow,
  id: "row-verified",
  status: "verified_fact",
  recordedAt: "2026-11-16T10:00:00Z",
  receipt: sampleReceipt,
}

test("FactStatusBadge renders all honest statuses with distinct data markers and labels", () => {
  const unverified = renderToStaticMarkup(<FactStatusBadge status="imported_unverified" />)
  const quarantined = renderToStaticMarkup(<FactStatusBadge status="quarantined" />)
  const withheld = renderToStaticMarkup(<FactStatusBadge status="withheld" />)
  const verified = renderToStaticMarkup(<FactStatusBadge status="verified_fact" />)

  expect(unverified).toContain('data-fact-status="imported_unverified"')
  expect(unverified).toContain("Imported (unverified)")
  expect(unverified).toContain('data-tone="amber"')

  expect(quarantined).toContain('data-fact-status="quarantined"')
  expect(quarantined).toContain("Quarantined")
  expect(quarantined).toContain('data-tone="red"')

  expect(withheld).toContain('data-fact-status="withheld"')
  expect(withheld).toContain("Withheld")
  expect(withheld).toContain('data-tone="gray"')

  expect(verified).toContain('data-fact-status="verified_fact"')
  expect(verified).toContain("Verified fact")
  expect(verified).toContain('data-tone="green"')
})

test("deriveFactStatus gives Quarantined and Withheld strict precedence and never infers Verified fact from empty findings", () => {
  expect(deriveFactStatus({ quarantined: true, verified: true })).toBe("quarantined")
  expect(deriveFactStatus({ openFindings: 1, verified: true })).toBe("quarantined")
  expect(deriveFactStatus({ withheld: true, verified: true })).toBe("withheld")
  expect(deriveFactStatus({ quarantined: false, openFindings: 0, unverifiedImport: true })).toBe("imported_unverified")
  expect(deriveFactStatus({ quarantined: false, openFindings: 0, verified: true })).toBe("verified_fact")
  expect(deriveFactStatus({ quarantined: false, openFindings: 0 })).toBeNull()
})

test("effectiveRowStatus and supersessionFactStatus never infer Verified fact without authoritative attestation", () => {
  const contradictory: CandidateRow = { ...violatingRow, status: "verified_fact" }
  expect(effectiveRowStatus(contradictory)).toBe("quarantined")
  expect(effectiveRowStatus(withheldRow)).toBe("withheld")
  expect(effectiveRowStatus(verifiedRow)).toBe("verified_fact")

  // Unresolved or keep_disputed supersession is strictly quarantined.
  expect(supersessionFactStatus(sampleConflict)).toBe("quarantined")
  expect(supersessionFactStatus({
    ...sampleConflict,
    resolution: { choice: "keep_disputed", preservedHistoricalArtifactId: "art-1", receipt: sampleReceipt },
  })).toBe("quarantined")
  // Accepting newer without explicit cell factStatus never fabricates "verified_fact".
  expect(supersessionFactStatus({
    ...sampleConflict,
    resolution: { choice: "accept_newer", preservedHistoricalArtifactId: "art-1", receipt: sampleReceipt },
  })).toBeNull()
  expect(supersessionFactStatus({
    ...sampleConflict,
    resolution: { choice: "accept_newer", preservedHistoricalArtifactId: "art-1", factStatus: "imported_unverified", receipt: sampleReceipt },
  })).toBe("imported_unverified")
  expect(supersessionFactStatus({
    ...sampleConflict,
    resolution: { choice: "accept_newer", preservedHistoricalArtifactId: "art-1", factStatus: "verified_fact", receipt: sampleReceipt },
  })).toBe("verified_fact")
})

test("draft idempotency keys remain stable on default submit retries and no-op edits, rotating only on logical payload changes", () => {
  // Default supersession: first submit stores key-1; retry/pending/error preserves key-1.
  const supSubmitting = patchSupersessionDraftState(undefined, sampleConflict, { idempotencyKey: "key-sup-1", pending: true })
  expect(supSubmitting.idempotencyKey).toBe("key-sup-1")
  const supFailed = patchSupersessionDraftState(supSubmitting, sampleConflict, { pending: false, notice: { kind: "error", message: "500" } })
  expect(supFailed.idempotencyKey).toBe("key-sup-1")
  // Re-selecting the same default choice keeps key-1.
  const supSameChoice = patchSupersessionDraftState(supFailed, sampleConflict, { choice: "accept_newer" })
  expect(supSameChoice.idempotencyKey).toBe("key-sup-1")
  // Changing choice or note clears/rotates the key.
  const supNewNote = patchSupersessionDraftState(supSameChoice, sampleConflict, { note: "Updated rationale" })
  expect(supNewNote.idempotencyKey).toBeNull()

  // Alias draft: re-clicking the already-selected mode keeps key-alias-1; changing mode rotates it.
  const aliasSelected = patchAliasDraftState(undefined, sampleAlias, { selectedTargetId: "ent-002", idempotencyKey: "key-alias-1" })
  const aliasSameMode = patchAliasDraftState(aliasSelected, sampleAlias, { mode: "match", notice: null })
  expect(aliasSameMode.idempotencyKey).toBe("key-alias-1")
  const aliasSameTarget = patchAliasDraftState(aliasSameMode, sampleAlias, { selectedTargetId: "ent-002" })
  expect(aliasSameTarget.idempotencyKey).toBe("key-alias-1")
  const aliasNewMode = patchAliasDraftState(aliasSameTarget, sampleAlias, { mode: "ignore" })
  expect(aliasNewMode.idempotencyKey).toBeNull()
})

test("runBatchPromotionQueue stops dispatching remaining queued rows immediately if active batchId changes", async () => {
  let activeBatch = "batch-A"
  const dispatched: string[] = []
  const report = await runBatchPromotionQueue(
    "batch-A",
    ["row-1", "row-2", "row-3"],
    () => activeBatch,
    1,
    async (rowId) => {
      dispatched.push(rowId)
      if (rowId === "row-1") activeBatch = "batch-B"
      return "promoted"
    },
  )
  expect(dispatched).toEqual(["row-1"])
  expect(report.aborted).toBe(true)
  expect(report.promoted).toBe(1)
})

test("runBatchPromotionQueue halts immediately when /imports unmounts and tallies basis-change skips", async () => {
  let mounted = true
  const dispatched: string[] = []
  const unmountReport = await runBatchPromotionQueue(
    "batch-A",
    ["row-1", "row-2", "row-3"],
    () => "batch-A",
    1,
    async (rowId) => {
      dispatched.push(rowId)
      if (rowId === "row-1") mounted = false
      return "promoted"
    },
    () => mounted,
  )
  expect(dispatched).toEqual(["row-1"])
  expect(unmountReport.aborted).toBe(true)
  expect(unmountReport.promoted).toBe(1)

  const staleReport = await runBatchPromotionQueue(
    "batch-A",
    ["row-1", "row-2"],
    () => "batch-A",
    0,
    async (rowId) => (rowId === "row-2" ? "stale" : "promoted"),
    () => true,
  )
  expect(staleReport.aborted).toBe(false)
  expect(staleReport.promoted).toBe(1)
  expect(staleReport.skippedStale).toBe(1)
})

test("verifyQueuedPromotionBasis skips queued rows whose live status, entity, or evidence digests drifted", () => {
  const artifactA: StagedArtifact = {
    id: "art-1",
    label: "Capture A",
    sourceChannel: "feed-ops",
    mediaType: "image/svg+xml",
    documentUri: "data:image/svg+xml;charset=utf-8,%3Csvg/%3E",
    blobDigest: "sha256:" + "1".repeat(64),
    functionReceiptDigest: "sha256:" + "2".repeat(64),
    extractor: "norn.extract.grid",
    extractorVersion: "1.0.0",
    observedAt: "2026-11-16T09:15:00Z",
    windowLabel: "2026-W47",
    markings: [],
    contextMessages: [],
  }
  const baseWorkspace: ImportTriageWorkspace = {
    batchId: "batch-1",
    title: "Window W47",
    targetObjectType: "canon.Allocation",
    lowConfidenceThresholdBps: 9000,
    actions: {
      bindAlias: "canon.alias.bind",
      createEntity: "canon.entity.create_and_bind",
      ignoreAlias: "canon.alias.ignore",
      promoteRow: "canon.import.promote_row",
      resolveSupersession: "canon.import.resolve_supersession",
    },
    aliases: [],
    availableEntities: [{ id: "ent-001", label: "Unit Alpha" }],
    artifacts: [artifactA],
    rows: [cleanRow],
    supersessions: [],
  }
  const basisKeyA = rowBasisKey(cleanRow, artifactA)
  const queuedItem: QueuedPromotionItem = {
    rowId: cleanRow.id,
    basisKey: basisKeyA,
    artifactId: artifactA.id,
    blobDigest: artifactA.blobDigest,
    functionReceiptDigest: artifactA.functionReceiptDigest,
    targetRecordId: "ent-001",
    effectiveAt: cleanRow.effectiveAt,
    observedAt: cleanRow.observedAt,
    values: { slot_code: "S-01", duration_hours: "8.0" },
  }

  expect(verifyQueuedPromotionBasis(baseWorkspace, "batch-1", queuedItem).kind).toBe("ready")

  // Digest drift -> stale.
  const digestChanged: ImportTriageWorkspace = {
    ...baseWorkspace,
    artifacts: [{ ...artifactA, blobDigest: "sha256:" + "9".repeat(64) }],
  }
  expect(verifyQueuedPromotionBasis(digestChanged, "batch-1", queuedItem).kind).toBe("stale")

  // Same-batch refresh changing a row's baseline field value -> stale (never submits old values).
  const baselineFieldChanged: ImportTriageWorkspace = {
    ...baseWorkspace,
    rows: [{
      ...cleanRow,
      fields: cleanRow.fields.map((f) => f.kind !== "withheld" && f.name === "duration_hours" ? { ...f, value: "8.25" } : f),
    }],
  }
  expect(verifyQueuedPromotionBasis(baselineFieldChanged, "batch-1", queuedItem).kind).toBe("stale")

  // Row became quarantined or verified before dispatch -> stale.
  const quarantinedNow: ImportTriageWorkspace = {
    ...baseWorkspace,
    rows: [{ ...cleanRow, status: "quarantined" }],
  }
  expect(verifyQueuedPromotionBasis(quarantinedNow, "batch-1", queuedItem).kind).toBe("stale")

  const verifiedNow: ImportTriageWorkspace = {
    ...baseWorkspace,
    rows: [{ ...cleanRow, status: "verified_fact", receipt: sampleReceipt }],
  }
  expect(verifyQueuedPromotionBasis(verifiedNow, "batch-1", queuedItem).kind).toBe("stale")

  // Row removed or batch switched -> stale.
  expect(verifyQueuedPromotionBasis({ ...baseWorkspace, rows: [] }, "batch-1", queuedItem).kind).toBe("stale")
  expect(verifyQueuedPromotionBasis({ ...baseWorkspace, batchId: "batch-2" }, "batch-1", queuedItem).kind).toBe("stale")

  // Local edits bound to Capture A's rowBasisKey NEVER survive supersession into Capture B.
  const artifactB: StagedArtifact = {
    ...artifactA,
    id: "art-2",
    blobDigest: "sha256:" + "3".repeat(64),
    functionReceiptDigest: "sha256:" + "4".repeat(64),
    observedAt: "2026-11-16T11:19:00Z",
  }
  const supersededRow: CandidateRow = {
    ...cleanRow,
    artifactId: "art-2",
    observedAt: "2026-11-16T11:19:00Z",
    fields: cleanRow.fields.map((f) => f.kind !== "withheld" && f.name === "duration_hours" ? { ...f, value: "8.50" } : f),
  }
  const basisKeyB = rowBasisKey(supersededRow, artifactB)
  expect(basisKeyB).not.toBe(basisKeyA)
  const editsUnderA = { [fieldEditKey(cleanRow.id, basisKeyA, "duration_hours")]: "9.99" }
  expect(extractEditableValues(cleanRow, editsUnderA, basisKeyA).duration_hours).toBe("9.99")
  expect(extractEditableValues(supersededRow, editsUnderA, basisKeyB).duration_hours).toBe("8.50")
})

test("batchLockedRowIds and isOutcomeLocked lock both committed-awaiting-read and pending-approval rows across edits, single-row, and batch", () => {
  expect(isOutcomeLocked({ kind: "committed", receipt: sampleReceipt })).toBe(true)
  expect(isOutcomeLocked({ kind: "pending", decisionId: "dec_1", expiresAtMs: Date.UTC(2026, 10, 24) })).toBe(true)
  expect(isOutcomeLocked({ kind: "error", message: "timeout" })).toBe(false)
  expect(isOutcomeLocked({ kind: "refused", code: "law.cap", message: "refused" })).toBe(false)
  expect(isOutcomeLocked({ kind: "stale", message: "basis changed" })).toBe(false)
  expect(isOutcomeLocked(null)).toBe(false)

  const locked = batchLockedRowIds(
    new Set(),
    new Set(),
    {
      "row-clean": { kind: "committed", receipt: sampleReceipt },
      "row-alias": { kind: "pending", decisionId: "dec_1", expiresAtMs: Date.UTC(2026, 10, 24) },
    },
  )
  const bound = new Map<string, AliasResolution>([
    ["alias-1", { kind: "bound", targetId: "ent-002", targetLabel: "Unit Beta", receipt: sampleReceipt }],
  ])
  const partition = partitionPromotionBatch([cleanRow, aliasBlockedRow], bound, locked)
  expect(partition.eligible).toHaveLength(0)
  expect(partition.inFlight.map((r) => r.id)).toEqual(["row-clean", "row-alias"])
})

test("readLiveWorkspaceFromCache prefers updated query cache over stale render ref and fails closed on non-ok cache entries", () => {
  const artifactA: StagedArtifact = {
    id: "art-1",
    label: "Capture A",
    sourceChannel: "feed-ops",
    mediaType: "image/svg+xml",
    documentUri: "data:image/svg+xml;charset=utf-8,%3Csvg/%3E",
    blobDigest: "sha256:" + "1".repeat(64),
    functionReceiptDigest: "sha256:" + "2".repeat(64),
    extractor: "norn.extract.grid",
    extractorVersion: "1.0.0",
    observedAt: "2026-11-16T09:15:00Z",
    windowLabel: "2026-W47",
    markings: [],
    contextMessages: [],
  }
  const staleRenderWorkspace: ImportTriageWorkspace = {
    batchId: "batch-1",
    title: "Window W47",
    targetObjectType: "canon.Allocation",
    lowConfidenceThresholdBps: 9000,
    actions: {
      bindAlias: "canon.alias.bind",
      createEntity: "canon.entity.create_and_bind",
      ignoreAlias: "canon.alias.ignore",
      promoteRow: "canon.import.promote_row",
      resolveSupersession: "canon.import.resolve_supersession",
    },
    aliases: [],
    availableEntities: [{ id: "ent-001", label: "Unit Alpha" }],
    artifacts: [artifactA],
    rows: [cleanRow],
    supersessions: [],
  }
  const freshCacheWorkspace: ImportTriageWorkspace = {
    ...staleRenderWorkspace,
    rows: [{
      ...cleanRow,
      fields: cleanRow.fields.map((f) => f.kind !== "withheld" && f.name === "duration_hours" ? { ...f, value: "8.25" } : f),
    }],
  }
  const queuedItem: QueuedPromotionItem = {
    rowId: cleanRow.id,
    basisKey: rowBasisKey(cleanRow, artifactA),
    artifactId: artifactA.id,
    blobDigest: artifactA.blobDigest,
    functionReceiptDigest: artifactA.functionReceiptDigest,
    targetRecordId: "ent-001",
    effectiveAt: cleanRow.effectiveAt,
    observedAt: cleanRow.observedAt,
    values: { slot_code: "S-01", duration_hours: "8.0" },
  }

  // Falls back to render ref only when cache is empty (undefined).
  expect(readLiveWorkspaceFromCache(() => undefined, staleRenderWorkspace)).toBe(staleRenderWorkspace)

  // Prefers fresh query cache even before React re-renders -> basis drift immediately caught!
  const live = readLiveWorkspaceFromCache(
    () => ({ kind: "ok", status: 200, etag: null, value: freshCacheWorkspace }),
    staleRenderWorkspace,
  )
  expect(live).toBe(freshCacheWorkspace)
  expect(verifyQueuedPromotionBasis(live, "batch-1", queuedItem).kind).toBe("stale")

  // Non-ok query cache result fails closed rather than falling back to a stale render ref.
  expect(readLiveWorkspaceFromCache(() => ({ kind: "unavailable" }), staleRenderWorkspace)).toBeUndefined()

  // Thrown refetch leaves stale ok data in getQueryData while getQueryState reports error -> fails closed!
  expect(
    readLiveWorkspaceFromCache(
      () => ({ kind: "ok", status: 200, etag: null, value: staleRenderWorkspace }),
      staleRenderWorkspace,
      () => ({ status: "error", error: new Error("Network timeout") }),
    ),
  ).toBeUndefined()
  expect(
    readLiveWorkspaceFromCache(
      () => ({ kind: "ok", status: 200, etag: null, value: staleRenderWorkspace }),
      staleRenderWorkspace,
      () => ({ status: "success", error: new Error("Refetch failed") }),
    ),
  ).toBeUndefined()
})

test("snapshotPromotionQueue captures rendered artifact & alias basis so pre-render cache updates skip queued rows as stale", () => {
  const renderedArtifact: StagedArtifact = {
    id: "art-1",
    label: "Capture A",
    sourceChannel: "feed-ops",
    mediaType: "image/svg+xml",
    documentUri: "data:image/svg+xml;charset=utf-8,%3Csvg/%3E",
    blobDigest: "sha256:" + "1".repeat(64),
    functionReceiptDigest: "sha256:" + "2".repeat(64),
    extractor: "norn.extract.grid",
    extractorVersion: "1.0.0",
    observedAt: "2026-11-16T09:15:00Z",
    windowLabel: "2026-W47",
    markings: [],
    contextMessages: [],
  }
  const renderedAliases = new Map<string, AliasResolution>([
    ["alias-1", { kind: "bound", targetId: "ent-002", targetLabel: "Unit Beta", receipt: sampleReceipt }],
  ])

  const renderedQueue = snapshotPromotionQueue(
    [cleanRow, aliasBlockedRow],
    [renderedArtifact],
    renderedAliases,
    {},
    new Set(),
  )
  expect(renderedQueue).toHaveLength(2)

  // Case 1: Pre-render cache update swapped artifact blobDigest -> queued row 1 skipped as stale.
  const liveArtifactChanged: ImportTriageWorkspace = {
    batchId: "batch-1",
    title: "Window W47",
    targetObjectType: "canon.Allocation",
    lowConfidenceThresholdBps: 9000,
    actions: {
      bindAlias: "canon.alias.bind",
      createEntity: "canon.entity.create_and_bind",
      ignoreAlias: "canon.alias.ignore",
      promoteRow: "canon.import.promote_row",
      resolveSupersession: "canon.import.resolve_supersession",
    },
    aliases: [{ ...sampleAlias, resolution: renderedAliases.get("alias-1")! }],
    availableEntities: [{ id: "ent-001", label: "Unit Alpha" }, { id: "ent-002", label: "Unit Beta" }],
    artifacts: [{ ...renderedArtifact, blobDigest: "sha256:" + "f".repeat(64) }],
    rows: [cleanRow, aliasBlockedRow],
    supersessions: [],
  }
  expect(verifyQueuedPromotionBasis(liveArtifactChanged, "batch-1", renderedQueue[0]!).kind).toBe("stale")

  // Case 2: Pre-render cache update rebound alias-1 to a different canonical entity -> queued row 2 skipped as stale.
  const liveAliasChanged: ImportTriageWorkspace = {
    ...liveArtifactChanged,
    artifacts: [renderedArtifact],
    aliases: [{
      ...sampleAlias,
      resolution: { kind: "bound", targetId: "ent-999", targetLabel: "Unit Omega", receipt: sampleReceipt },
    }],
  }
  expect(verifyQueuedPromotionBasis(liveAliasChanged, "batch-1", renderedQueue[1]!).kind).toBe("stale")
})

test("reconcilePendingRowOutcome retires idempotency key on terminal failures and fails closed on malformed 200 responses", async () => {
  const pending = { kind: "pending" as const, decisionId: "dec_42", expiresAtMs: Date.UTC(2026, 10, 24) }
  const makeDecision = (
    status: "pending" | "rejected" | "expired" | "withdrawn" | "approved",
    callState?: { readonly committed: boolean; readonly state: "committed" | "refused" | "not_executed"; readonly code?: string },
  ) => ({
    id: "dec_42",
    question: "Approve row promotion?",
    risk: "medium" as const,
    requestedAtMs: Date.UTC(2026, 10, 23, 12, 0),
    expiresAtMs: Date.UTC(2026, 10, 24, 12, 0),
    status,
    call: {
      action: "canon.import.promote_row",
      definition: "canon.Allocation",
      input: {},
      encodedInput: "{}",
      idempotencyKey: "key-1",
      ...callState ? { state: callState } : {},
    },
    canRun: false,
  })

  // Still pending -> remains locked and keeps key.
  const stillPending = reconcilePendingRowOutcome(pending, { kind: "found", decision: makeDecision("pending") })
  expect(isOutcomeLocked(stillPending.nextOutcome)).toBe(true)
  expect(stillPending.retireIdempotencyKey).toBe(false)

  // Rejected, expired, withdrawn, and execution-refused -> unlock AND signal idempotency key retirement.
  const cache = new Map<string, PayloadBoundKeyEntry>()
  let seq = 0
  const mint = () => `key-${++seq}`
  const payload = { batchId: "batch-1", rowId: "row-clean", values: { duration_hours: "8.0" } }
  const initialKey = resolvePayloadIdempotencyKey(cache, "row:batch-1:row-clean", "canon.import.promote_row", payload, mint)
  expect(initialKey).toBe("key-1")

  for (const terminal of ["rejected", "expired", "withdrawn"] as const) {
    const res = reconcilePendingRowOutcome(pending, { kind: "found", decision: makeDecision(terminal) })
    expect(isOutcomeLocked(res.nextOutcome)).toBe(false)
    expect(res.retireIdempotencyKey).toBe(true)
    expect(res.nextOutcome.kind).toBe("refused")
    if (res.nextOutcome.kind === "refused") {
      expect(res.nextOutcome.code).toBe(`decision.${terminal}`)
    }
  }

  const execRefused = reconcilePendingRowOutcome(pending, {
    kind: "found",
    decision: makeDecision("approved", { committed: false, state: "refused", code: "law.cap" }),
  })
  expect(isOutcomeLocked(execRefused.nextOutcome)).toBe(false)
  expect(execRefused.retireIdempotencyKey).toBe(true)

  // Retiring the cached key guarantees re-promoting the exact same unchanged payload mints a fresh key.
  retirePayloadIdempotencyKey(cache, "row:batch-1:row-clean")
  const freshKey = resolvePayloadIdempotencyKey(cache, "row:batch-1:row-clean", "canon.import.promote_row", payload, mint)
  expect(freshKey).toBe("key-2")

  // Valid empty items array -> absent, unlocks row with stale notice.
  expect(parseRequestedDecisionLookup({ items: [], serverNowMs: 1 }, "dec_42")).toEqual({ kind: "absent" })
  const gone = reconcilePendingRowOutcome(pending, { kind: "absent" })
  expect(isOutcomeLocked(gone.nextOutcome)).toBe(false)
  expect(gone.nextOutcome.kind).toBe("stale")
  expect(gone.shouldRefreshWorkspace).toBe(true)

  // Malformed HTTP 200 cases all fail closed and preserve the pending lock without retiring the key:
  // - missing/non-array items
  expect(parseRequestedDecisionLookup({}, "dec_42")).toEqual({ kind: "malformed" })
  expect(parseRequestedDecisionLookup({ items: "bad" }, "dec_42")).toEqual({ kind: "malformed" })
  // - array entry (items: [[]]) or entry missing string decision id
  expect(parseRequestedDecisionLookup({ items: [[]] }, "dec_42")).toEqual({ kind: "malformed" })
  expect(parseRequestedDecisionLookup({ items: [{ status: "pending" }] }, "dec_42")).toEqual({ kind: "malformed" })
  expect(parseRequestedDecisionLookup({ items: [{ id: "", status: "pending" }] }, "dec_42")).toEqual({ kind: "malformed" })
  // - trailing null / malformed entry after a valid match
  expect(parseRequestedDecisionLookup({ items: [makeDecision("rejected"), null] }, "dec_42")).toEqual({ kind: "malformed" })
  expect(parseRequestedDecisionLookup({ items: [makeDecision("rejected"), []] }, "dec_42")).toEqual({ kind: "malformed" })
  // - invalid matching entry shape
  expect(parseRequestedDecisionLookup({ items: [{ id: "dec_42", status: "rejected" }] }, "dec_42")).toEqual({ kind: "malformed" })
  // - approved decision missing call.state
  expect(parseRequestedDecisionLookup({ items: [makeDecision("approved")] }, "dec_42")).toEqual({ kind: "malformed" })
  const approvedWithoutState = reconcilePendingRowOutcome(pending, { kind: "found", decision: makeDecision("approved") })
  expect(approvedWithoutState.nextOutcome).toBe(pending)
  expect(isOutcomeLocked(approvedWithoutState.nextOutcome)).toBe(true)
  expect(approvedWithoutState.retireIdempotencyKey).toBe(false)
  // - approved decision with contradictory committed vs state ({ committed: true, state: "refused" })
  const contradictoryApproved = makeDecision("approved", { committed: true, state: "refused", code: "law.cap" })
  expect(parseRequestedDecisionLookup({ items: [contradictoryApproved] }, "dec_42")).toEqual({ kind: "malformed" })
  const contradictoryReconciled = reconcilePendingRowOutcome(pending, { kind: "found", decision: contradictoryApproved })
  expect(contradictoryReconciled.nextOutcome).toBe(pending)
  expect(isOutcomeLocked(contradictoryReconciled.nextOutcome)).toBe(true)
  expect(contradictoryReconciled.retireIdempotencyKey).toBe(false)
  // - approved decision with valid consistent not_executed state succeeds
  const validApprovedNotExec = makeDecision("approved", { committed: false, state: "not_executed" })
  expect(parseRequestedDecisionLookup({ items: [validApprovedNotExec] }, "dec_42")).toEqual({
    kind: "found",
    decision: validApprovedNotExec,
  })
  expect(parseRequestedDecisionLookup({ items: [{ ...makeDecision("approved"), call: { ...makeDecision("approved").call, state: { invalid: true } } }] }, "dec_42")).toEqual({ kind: "malformed" })

  const malformedFetcher = async (input: string): Promise<Response> => {
    if (input === "/api/me") {
      return new Response(JSON.stringify({ organization: { id: "org_demo" }, environmentId: "prod" }), { status: 200 })
    }
    return new Response(JSON.stringify({ items: [{ id: "dec_42", status: "rejected" }] }), { status: 200 })
  }
  const malformedLookup = await fetchRequestedDecisionForReconciliation("dec_42", malformedFetcher)
  expect(malformedLookup).toEqual({ kind: "malformed" })
  const keptLocked = reconcilePendingRowOutcome(pending, malformedLookup)
  expect(keptLocked.nextOutcome).toBe(pending)
  expect(isOutcomeLocked(keptLocked.nextOutcome)).toBe(true)
  expect(keptLocked.retireIdempotencyKey).toBe(false)
})

test("resolvePayloadIdempotencyKey reuses exact key for identical payloads (and no-op resets) and mints fresh key when payload or cell basis changes", () => {
  const cache = new Map<string, PayloadBoundKeyEntry>()
  let seq = 0
  const mint = () => `key-${++seq}`

  const payload1 = {
    batchId: "batch-1",
    rowId: "row-clean",
    artifactId: "art-1",
    blobDigest: "sha256:" + "1".repeat(64),
    values: { duration_hours: "8.0", slot_code: "S-01" },
  }
  // Same logical payload with reversed key insertion order -> identical canonical fingerprint & exact key reuse.
  const payload1Reordered = {
    values: { slot_code: "S-01", duration_hours: "8.0" },
    blobDigest: "sha256:" + "1".repeat(64),
    artifactId: "art-1",
    rowId: "row-clean",
    batchId: "batch-1",
  }

  expect(canonicalPayloadKey("canon.import.promote_row", payload1)).toBe(
    canonicalPayloadKey("canon.import.promote_row", payload1Reordered),
  )

  const k1 = resolvePayloadIdempotencyKey(cache, "row:batch-1:row-clean", "canon.import.promote_row", payload1, mint)
  const k1Retry = resolvePayloadIdempotencyKey(cache, "row:batch-1:row-clean", "canon.import.promote_row", payload1Reordered, mint)
  expect(k1).toBe("key-1")
  expect(k1Retry).toBe("key-1")

  // Refresh changes artifact blobDigest (outgoing payload changed) -> mints a fresh key instead of colliding.
  const payloadAfterRefresh = { ...payload1, blobDigest: "sha256:" + "2".repeat(64) }
  const k2 = resolvePayloadIdempotencyKey(cache, "row:batch-1:row-clean", "canon.import.promote_row", payloadAfterRefresh, mint)
  expect(k2).toBe("key-2")

  // Retrying the refreshed payload reuses k2; editing a value mints k3.
  expect(resolvePayloadIdempotencyKey(cache, "row:batch-1:row-clean", "canon.import.promote_row", payloadAfterRefresh, mint)).toBe("key-2")
  const payloadEdited = { ...payloadAfterRefresh, values: { slot_code: "S-01", duration_hours: "8.5" } }
  expect(resolvePayloadIdempotencyKey(cache, "row:batch-1:row-clean", "canon.import.promote_row", payloadEdited, mint)).toBe("key-3")
})

test("classifyTriageActionResult treats structured HTTP 5xx as retryable error and HTTP 4xx as domain refusal", () => {
  const server500 = classifyTriageActionResult({
    kind: "refused",
    status: 500,
    code: "internal.storage_busy",
    message: "Upstream ledger timeout",
    diagnostics: [],
  })
  expect(server500.kind).toBe("error")

  const law422 = classifyTriageActionResult({
    kind: "refused",
    status: 422,
    code: "law.boundary_cap",
    message: "Cap exceeded",
    diagnostics: [],
  })
  expect(law422).toEqual({ kind: "refused", code: "law.boundary_cap", message: "Cap exceeded" })
})

test("confidence, temporal formatting, and withheld field extraction guards", () => {
  expect(isLowConfidence(8999, 9000)).toBe(true)
  expect(isLowConfidence(9000, 9000)).toBe(false)
  expect(isLowConfidence(null, 9000)).toBe(false)
  expect(formatConfidence(6850)).toBe("68.5%")
  expect(formatConfidence(null)).toBe("—")
  expect(hasOpenLawViolation(violatingRow)).toBe(true)
  expect(hasOpenLawViolation(cleanRow)).toBe(false)
  expect(formatTemporal("2026-02-31")).toBe("2026-02-31")
  expect(formatTemporal("2026-11-16T08:00:00Z")).toBe("16 Nov 2026, 08:00 UTC")
  expect(timeDeltaLabel("2026-11-16T09:00:00Z", "2026-11-16T11:15:00Z")).toBe("+2h 15m later")

  expect(extractEditableValues(withheldRow, { "row-withheld:contact_note": "spoofed" })).toEqual({
    slot_code: "S-01",
  })
})

test("strict alias gating: pre-populated entityRecordId cannot bypass an unresolved or ignored alias, and busy rows are excluded from batch", () => {
  const unresolved = new Map<string, AliasResolution>()
  expect(resolveRowEntity(aliasBlockedRow, unresolved).kind).toBe("unresolved")
  expect(isRowEligibleForBatchPromotion(aliasBlockedRow, unresolved)).toBe(false)

  const ignored = new Map<string, AliasResolution>([
    ["alias-1", { kind: "ignored", note: "relay bot", receipt: sampleReceipt }],
  ])
  expect(resolveRowEntity(aliasBlockedRow, ignored).kind).toBe("ignored")
  expect(isRowEligibleForBatchPromotion(aliasBlockedRow, ignored)).toBe(false)

  const bound = new Map<string, AliasResolution>([
    ["alias-1", { kind: "bound", targetId: "ent-002", targetLabel: "Unit Beta", receipt: sampleReceipt }],
  ])
  expect(resolveRowEntity(aliasBlockedRow, bound)).toEqual({ kind: "resolved", targetId: "ent-002", label: "Unit Beta" })

  const busy = new Set<string>(["row-clean"])
  const partitioned = partitionPromotionBatch([cleanRow, aliasBlockedRow, violatingRow, withheldRow, verifiedRow], bound, busy)
  expect(partitioned.inFlight.map((r) => r.id)).toEqual(["row-clean"])
  expect(partitioned.eligible.map((r) => r.id)).toEqual(["row-alias"])
  expect(partitioned.quarantined.map((r) => r.id)).toEqual(["row-violation"])
  expect(partitioned.withheld.map((r) => r.id)).toEqual(["row-withheld"])
  expect(partitioned.verified.map((r) => r.id)).toEqual(["row-verified"])
})

test("resolveArtifactMediaSource normalizes paths, requires scoped cell-media reads, and rejects traversal or external HTTP(S) URLs", () => {
  expect(resolveArtifactMediaSource("data:image/svg+xml;charset=utf-8,%3Csvg/%3E")).toEqual({
    kind: "data-uri",
    url: "data:image/svg+xml;charset=utf-8,%3Csvg/%3E",
  })
  expect(resolveArtifactMediaSource("/v1/blobs/sha256:abc")).toEqual({
    kind: "cell-media",
    path: "/v1/blobs/sha256:abc",
  })
  expect(resolveArtifactMediaSource("/api/host/v1/uploads/cap-1")).toEqual({
    kind: "cell-media",
    path: "/v1/uploads/cap-1",
  })
  expect(resolveArtifactMediaSource("s3://bucket/capture-a.png")).toEqual({
    kind: "cell-scan",
    uri: "s3://bucket/capture-a.png",
  })
  // Reject traversal, percent-encoded traversal, protocol-relative, and arbitrary external URLs.
  expect(resolveArtifactMediaSource("/v1/blobs/../../secrets").kind).toBe("rejected")
  expect(resolveArtifactMediaSource("/v1/blobs/%2e%2e/secrets").kind).toBe("rejected")
  expect(resolveArtifactMediaSource("/api/host/v1/blobs/%2e%2e%2fsecrets").kind).toBe("rejected")
  expect(resolveArtifactMediaSource("//evil.example/v1/blobs/a").kind).toBe("rejected")
  expect(resolveArtifactMediaSource("https://external.example/tracker.png").kind).toBe("rejected")
  expect(resolveArtifactMediaSource("http://169.254.169.254/latest/meta-data").kind).toBe("rejected")
})

test("fetchImportTriage treats 404 route.not_found and 503 as unavailable while preserving real 403 refusals", async () => {
  const makeFetcher = (status: number, body: unknown) => async (input: string): Promise<Response> => {
    if (input === "/api/me") {
      return new Response(JSON.stringify({ organization: { id: "org_demo" }, environmentId: "prod" }), { status: 200 })
    }
    return new Response(JSON.stringify(body), { status })
  }

  const notFound = await fetchImportTriage(makeFetcher(404, { code: "route.not_found", message: "No route /v1/imports/triage" }))
  expect(notFound).toEqual({ kind: "unavailable" })

  const unavailable = await fetchImportTriage(makeFetcher(503, { code: "host.unavailable", message: "Starting" }))
  expect(unavailable).toEqual({ kind: "unavailable" })

  const forbidden = await fetchImportTriage(makeFetcher(403, { code: "authz.denied", message: "Not permitted." }))
  expect(forbidden.kind).toBe("refused")
})

test("triageSummary tallies unresolved aliases, unverified/quarantined/withheld/verified rows, and open conflicts", () => {
  const workspace: ImportTriageWorkspace = {
    batchId: "batch-1",
    title: "Window W47 capture",
    targetObjectType: "canon.Allocation",
    lowConfidenceThresholdBps: 9000,
    actions: {
      bindAlias: "canon.alias.bind",
      createEntity: "canon.entity.create_and_bind",
      ignoreAlias: "canon.alias.ignore",
      promoteRow: "canon.import.promote_row",
      resolveSupersession: "canon.import.resolve_supersession",
    },
    aliases: [sampleAlias],
    availableEntities: [{ id: "ent-001", label: "Unit Alpha" }],
    artifacts: [],
    rows: [cleanRow, violatingRow, withheldRow, verifiedRow],
    supersessions: [sampleConflict],
  }
  expect(triageSummary(workspace)).toEqual({
    unresolvedAliases: 1,
    resolvedAliases: 0,
    unverifiedRows: 1,
    quarantinedRows: 1,
    withheldRows: 1,
    verifiedRows: 1,
    openConflicts: 1,
    resolvedConflicts: 0,
    totalAttention: 5,
  })
})

test("governed triage commits send scoped POST /v1/actions requests with idempotency keys and preserve historical evidence", async () => {
  const requests: { url: string; body: string }[] = []
  const fetcher = async (input: string, init?: RequestInit): Promise<Response> => {
    if (input === "/api/me") {
      return new Response(JSON.stringify({ organization: { id: "org_demo" }, environmentId: "prod" }), { status: 200 })
    }
    requests.push({ url: input, body: String(init?.body ?? "") })
    return new Response(JSON.stringify(sampleReceipt), { status: 200 })
  }

  const bind = await commitAliasBinding("canon.alias.bind", {
    batchId: "batch-1",
    aliasId: "alias-1",
    externalHandle: "@handle_north_1",
    targetRecordId: "ent-002",
  }, "key-bind-1", fetcher)
  expect(bind.kind).toBe("receipt")

  const promote = await commitRowPromotion("canon.import.promote_row", {
    batchId: "batch-1",
    rowId: "row-clean",
    artifactId: "art-1",
    blobDigest: "sha256:" + "1".repeat(64),
    functionReceiptDigest: "sha256:" + "2".repeat(64),
    targetRecordId: "ent-001",
    effectiveAt: "2026-11-16T08:00:00Z",
    observedAt: "2026-11-16T09:15:00Z",
    values: { slot_code: "S-01", duration_hours: "8.0" },
  }, "key-promote-1", fetcher)
  expect(promote.kind).toBe("receipt")

  const supersede = await commitSupersessionDecision("canon.import.resolve_supersession", {
    batchId: "batch-1",
    conflictId: "sup-1",
    choice: "accept_newer",
    earlierArtifactId: "art-1",
    earlierBlobDigest: "sha256:" + "1".repeat(64),
    newerArtifactId: "art-2",
    newerBlobDigest: "sha256:" + "3".repeat(64),
    preserveEarlierEvidence: true,
    note: "Revised snapshot",
  }, "key-sup-1", fetcher)
  expect(supersede.kind).toBe("receipt")

  expect(requests.map((r) => new URL(r.url, "https://studio").pathname)).toEqual([
    "/api/host/v1/actions/canon.alias.bind",
    "/api/host/v1/actions/canon.import.promote_row",
    "/api/host/v1/actions/canon.import.resolve_supersession",
  ])
  expect(JSON.parse(requests[2]!.body)).toMatchObject({
    idempotencyKey: "key-sup-1",
    input: { choice: "accept_newer", preserveEarlierEvidence: true },
  })
})

test("domain-agnostic guardrail: zero domain-specific identifiers in Studio import triage sources", () => {
  const files = [
    "../src/components/fact-status-badge.tsx",
    "../src/lib/import-triage.ts",
    "../src/imports/import-triage-page.tsx",
    "../src/imports/alias-matcher.tsx",
    "../src/imports/split-pane-review.tsx",
    "../src/imports/supersession-diff.tsx",
  ]
  const forbidden = /\b(hr|payroll|roster|shift|shifts|leave|jewelry|scrap|melt|employee|employees|whatsapp|salary|attendance|gold|karat|carat)\b/i
  for (const rel of files) {
    const source = readFileSync(new URL(rel, import.meta.url), "utf8")
    const match = forbidden.exec(source)
    expect({ file: rel, match: match?.[0] ?? null }).toEqual({ file: rel, match: null })
  }
})
