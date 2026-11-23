import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { renderToStaticMarkup } from "react-dom/server"
import { deriveFactStatus, FactStatusBadge } from "../src/components/fact-status-badge.tsx"
import {
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
  resolveArtifactMediaSource,
  resolveRowEntity,
  timeDeltaLabel,
  triageSummary,
  type AliasResolution,
  type CandidateRow,
  type ImportTriageWorkspace,
} from "../src/lib/import-triage.ts"

const sampleReceipt = {
  outcome: "committed" as const,
  objectType: "canon.Allocation",
  objectId: "alloc-01",
  version: 1,
  receipt: { receiptHash: "a".repeat(64), commitSeq: 41, recordedAtMs: Date.UTC(2026, 10, 23, 12, 0, 0) },
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
  // Absence of visible findings alone never implies "Verified fact".
  expect(deriveFactStatus({ quarantined: false, openFindings: 0 })).toBeNull()
})

test("effectiveRowStatus gives Quarantined and Withheld precedence even if raw status claims verified_fact", () => {
  const contradictory: CandidateRow = { ...violatingRow, status: "verified_fact" }
  expect(effectiveRowStatus(contradictory)).toBe("quarantined")
  expect(effectiveRowStatus(withheldRow)).toBe("withheld")
  expect(effectiveRowStatus(verifiedRow)).toBe("verified_fact")
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

  // Withheld fields are strictly excluded from governed promotion values.
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

test("resolveArtifactMediaSource authorizes cell-scoped/data/object-store URIs and rejects external HTTP(S) URLs", () => {
  expect(resolveArtifactMediaSource("data:image/svg+xml;charset=utf-8,%3Csvg/%3E")).toEqual({
    kind: "direct",
    url: "data:image/svg+xml;charset=utf-8,%3Csvg/%3E",
  })
  expect(resolveArtifactMediaSource("/v1/blobs/sha256:abc")).toEqual({
    kind: "direct",
    url: "/api/host/v1/blobs/sha256:abc",
  })
  expect(resolveArtifactMediaSource("s3://bucket/capture-a.png")).toEqual({
    kind: "cell-scan",
    uri: "s3://bucket/capture-a.png",
  })
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
    aliases: [
      {
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
      },
    ],
    availableEntities: [{ id: "ent-001", label: "Unit Alpha" }],
    artifacts: [],
    rows: [cleanRow, violatingRow, withheldRow, verifiedRow],
    supersessions: [
      {
        id: "sup-1",
        findingCode: "FEED-CONFLICT",
        windowLabel: "2026-W47",
        scopeKey: "zone-north/2026-W47",
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
      },
    ],
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
