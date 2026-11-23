import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { renderToStaticMarkup } from "react-dom/server"
import { deriveFactStatus, FactStatusBadge } from "../src/components/fact-status-badge.tsx"
import {
  canonicalPayloadKey,
  classifyTriageActionResult,
  commitAliasBinding,
  commitRowPromotion,
  commitSupersessionDecision,
  effectiveRowStatus,
  extractEditableValues,
  fetchImportTriage,
  formatConfidence,
  formatTemporal,
  hasOpenLawViolation,
  isLowConfidence,
  isRowEligibleForBatchPromotion,
  partitionPromotionBatch,
  patchAliasDraftState,
  patchSupersessionDraftState,
  resolveArtifactMediaSource,
  resolvePayloadIdempotencyKey,
  resolveRowEntity,
  runBatchPromotionQueue,
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
  const queuedItem: QueuedPromotionItem = {
    rowId: cleanRow.id,
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
