import type { Page, Route } from "@playwright/test"
import type {
  AliasCandidate,
  CandidateRow,
  ImportTriageWorkspace,
  StagedArtifact,
  SupersessionConflict,
  TargetEntity,
} from "../src/lib/import-triage.ts"

/**
 * Synthetic, strictly domain-agnostic stand-in for the cell's import triage read and governed
 * action routes. Zero real customer or personal data; uses inline SVG artifacts (and optional
 * scoped `/v1/blobs/...` media responses) so zoomable source captures render crisply without
 * external network calls.
 */

function artifactSvgMarkup(title: string, timestamp: string, rows: readonly [string, string, string, string][]): string {
  const body = rows.map(([entity, slot, duration, status], index) => {
    const y = 112 + index * 48
    const highlight = status === "amber" ? "#fef3c7" : status === "red" ? "#fee2e2" : "#f8fafc"
    return `
      <rect x="24" y="${y}" width="592" height="38" rx="4" fill="${highlight}" stroke="#cbd5e1" stroke-width="1"/>
      <text x="40" y="${y + 24}" font-family="ui-monospace, monospace" font-size="13" fill="#0f172a">${entity}</text>
      <text x="215" y="${y + 24}" font-family="ui-monospace, monospace" font-size="13" fill="#0f172a">${slot}</text>
      <text x="385" y="${y + 24}" font-family="ui-monospace, monospace" font-size="13" fill="#0f172a">${duration}</text>
      <text x="505" y="${y + 24}" font-family="ui-monospace, monospace" font-size="11" fill="#475569">${status === "red" ? "FLAG" : status === "amber" ? "LOW-CONF" : "OK"}</text>
    `
  }).join("")
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 640 370" width="640" height="370">
    <rect width="640" height="370" fill="#ffffff"/>
    <rect x="0" y="0" width="640" height="58" fill="#f1f5f9"/>
    <line x1="0" y1="58" x2="640" y2="58" stroke="#cbd5e1" stroke-width="1"/>
    <text x="24" y="28" font-family="system-ui, sans-serif" font-size="15" font-weight="600" fill="#0f172a">${title}</text>
    <text x="24" y="47" font-family="ui-monospace, monospace" font-size="11" fill="#475569">Captured ${timestamp} · Synthetic Grid Snapshot</text>
    <text x="40" y="92" font-family="system-ui, sans-serif" font-size="11" font-weight="600" fill="#64748b">RECORD</text>
    <text x="215" y="92" font-family="system-ui, sans-serif" font-size="11" font-weight="600" fill="#64748b">WINDOW SLOT</text>
    <text x="385" y="92" font-family="system-ui, sans-serif" font-size="11" font-weight="600" fill="#64748b">HOURS</text>
    <text x="505" y="92" font-family="system-ui, sans-serif" font-size="11" font-weight="600" fill="#64748b">SCAN</text>
    <line x1="24" y1="100" x2="616" y2="100" stroke="#e2e8f0" stroke-width="1"/>
    ${body}
  </svg>`
}

const ARTIFACT_A_SVG = artifactSvgMarkup("Window W47 Allocation Grid — Capture A", "2026-11-16 09:04 UTC", [
  ["Unit Alpha", "SLOT-01 · Mon", "8.00", "amber"],
  ["@handle_north_1", "SLOT-02 · Tue", "7.50", "clean"],
  ["Unit Gamma", "SLOT-02 · Tue", "16.00", "red"],
  ["Unit Delta", "2026-02-31", "6.00", "red"],
  ["Unit Zeta", "SLOT-09 · Wed", "8.00", "red"],
])

const ARTIFACT_A_URI = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(ARTIFACT_A_SVG)}`

const ARTIFACT_B_URI = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(artifactSvgMarkup("Window W47 Allocation Grid — Capture B (Revised)", "2026-11-16 11:19 UTC", [
  ["Unit Zeta", "SLOT-10 · Wed", "8.50", "clean"],
]))}`

export interface TriageCall {
  readonly actionId: string
  readonly idempotencyKey: string
  readonly input: Record<string, unknown>
}

export interface ImportTriageCell {
  readonly calls: TriageCall[]
  readonly blobRequests: string[]
  /** Make the next promotion call for `rowId` refuse with a 422 law violation. */
  refuseNextPromote(rowId: string, code: string, message: string): void
  /** Make the next promotion call for `rowId` return 202 pending approval. */
  pendingNextPromote(rowId: string, decisionId: string): void
  /** Make the next promotion call for `rowId` fail with a structured JSON HTTP 500 server error. */
  failNextPromote(rowId: string): void
  /** Make the next supersession call for `conflictId` fail with a structured JSON HTTP 500 server error. */
  failNextSupersession(conflictId: string): void
  /** Hold the next promotion call for `rowId` until the returned release callback is invoked. */
  holdNextPromote(rowId: string): () => void
  /** Hold the next `GET /v1/imports/triage` read until the returned release callback is invoked. */
  holdNextTriageRead(): () => void
  /** Simulate another operator committing a row on the cell with authoritative values. */
  commitRowElsewhere(rowId: string, values: Readonly<Record<string, string>>): void
  /** Switch the cell's active triage batch to a fresh batchId (e.g. next window). */
  switchBatch(nextBatchId: string): void
}

const hash = (seed: number) => `sha256:${seed.toString(16).padStart(64, "b")}`

export function createFixtureWorkspace(options: {
  readonly batchId?: string
  readonly includeWithheldRow?: boolean
  readonly artifactMediaMode?: "data" | "cell-blob" | "broken-blob" | "external"
} = {}): ImportTriageWorkspace {
  const entities: TargetEntity[] = [
    { id: "rec-001", label: "Unit Alpha", subtitle: "Zone North" },
    { id: "rec-002", label: "Unit Beta", subtitle: "Zone North" },
    { id: "rec-003", label: "Unit Gamma", subtitle: "Zone East" },
    { id: "rec-004", label: "Unit Delta", subtitle: "Zone West" },
    { id: "rec-005", label: "Unit Zeta", subtitle: "Zone South" },
  ]

  const aliases: AliasCandidate[] = [
    {
      id: "alias-north-1",
      externalHandle: "@handle_north_1",
      sourceChannel: "feed-ops-north",
      occurrences: 5,
      sampleExcerpt: "Posting updated W47 allocation grid for Zone North.",
      firstObservedAt: "2026-11-16T08:52:00Z",
      suggestedTargetId: "rec-002",
      suggestedTargetLabel: "Unit Beta",
      suggestionConfidenceBps: 8140,
      resolution: null,
    },
    {
      id: "alias-temp-9",
      externalHandle: "Handle-Temp-09",
      sourceChannel: "feed-ops-north",
      occurrences: 2,
      sampleExcerpt: "Covered backup slot for East annex on Wednesday.",
      firstObservedAt: "2026-11-16T09:10:00Z",
      suggestedTargetId: null,
      suggestedTargetLabel: null,
      suggestionConfidenceBps: null,
      resolution: null,
    },
    {
      id: "alias-bot-notify",
      externalHandle: "AutoRelay-Bot",
      sourceChannel: "feed-ops-north",
      occurrences: 9,
      sampleExcerpt: "Automated export timestamp sync.",
      firstObservedAt: "2026-11-15T18:00:00Z",
      suggestedTargetId: null,
      suggestedTargetLabel: null,
      suggestionConfidenceBps: null,
      resolution: null,
    },
  ]

  const artifactAUri = options.artifactMediaMode === "external"
    ? "https://untrusted.example/capture-a.png"
    : options.artifactMediaMode === "cell-blob"
      ? "/v1/blobs/capture-w47-a"
      : options.artifactMediaMode === "broken-blob"
        ? "/v1/blobs/capture-w47-broken"
        : ARTIFACT_A_URI

  const artifacts: StagedArtifact[] = [
    {
      id: "art-w47-a",
      label: "Capture A · W47 Grid (09:04)",
      sourceChannel: "feed-ops-north",
      mediaType: "image/svg+xml",
      documentUri: artifactAUri,
      blobDigest: hash(101),
      functionReceiptDigest: hash(201),
      extractor: "norn.extract.grid",
      extractorVersion: "1.4.0",
      promptDigest: hash(301),
      observedAt: "2026-11-16T09:04:00Z",
      windowLabel: "2026-W47",
      markings: ["pii.raw_media", "pii.handle"],
      contextMessages: [
        {
          id: "msg-1",
          senderHandle: "@handle_north_1",
          aliasId: "alias-north-1",
          observedAt: "2026-11-16T09:02:00Z",
          body: "Sharing initial W47 allocation grid capture for review.",
        },
        {
          id: "msg-2",
          senderHandle: "@handle_north_1",
          aliasId: "alias-north-1",
          observedAt: "2026-11-16T09:04:00Z",
          body: "Attached Capture A (W47 Grid). Note row 1 hours were handwritten.",
          isAnchor: true,
        },
        {
          id: "msg-3",
          senderHandle: "Unit Alpha",
          observedAt: "2026-11-16T09:12:00Z",
          body: "Confirmed Mon slot starts at 08:00 UTC.",
        },
      ],
    },
    {
      id: "art-w47-b",
      label: "Capture B · W47 Revised (11:19)",
      sourceChannel: "feed-ops-south",
      mediaType: "image/svg+xml",
      documentUri: ARTIFACT_B_URI,
      blobDigest: hash(102),
      functionReceiptDigest: hash(202),
      extractor: "norn.extract.grid",
      extractorVersion: "1.4.0",
      promptDigest: hash(301),
      observedAt: "2026-11-16T11:19:00Z",
      windowLabel: "2026-W47",
      markings: ["pii.raw_media", "pii.handle"],
      contextMessages: [
        {
          id: "msg-4",
          senderHandle: "@handle_north_1",
          aliasId: "alias-north-1",
          observedAt: "2026-11-16T11:19:00Z",
          body: "Updated Zone South snapshot — adjusted Unit Zeta slot and duration.",
          isAnchor: true,
        },
      ],
    },
  ]

  const rows: CandidateRow[] = [
    {
      id: "row-w47-1",
      artifactId: "art-w47-a",
      targetObjectType: "canon.Allocation",
      windowLabel: "2026-W47",
      entityAliasId: null,
      entityRecordId: "rec-001",
      entityLabel: "Unit Alpha",
      status: "imported_unverified",
      effectiveAt: "2026-11-16T08:00:00Z",
      observedAt: "2026-11-16T09:04:00Z",
      recordedAt: null,
      fields: [
        {
          id: "f-1-slot",
          name: "slot_code",
          label: "Window slot",
          extractedValue: "SLOT-01",
          value: "SLOT-01",
          confidenceBps: 9840,
          box: { x: "0.32", y: "0.30", w: "0.24", h: "0.10" },
          findingIds: [],
        },
        {
          id: "f-1-hours",
          name: "duration_hours",
          label: "Duration (hours)",
          extractedValue: "8.00",
          value: "8.00",
          confidenceBps: 6800,
          box: { x: "0.59", y: "0.30", w: "0.16", h: "0.10" },
          findingIds: [],
        },
      ],
      findings: [],
      ambiguities: ["Handwritten decimal digit"],
      receipt: null,
    },
    {
      id: "row-w47-2",
      artifactId: "art-w47-a",
      targetObjectType: "canon.Allocation",
      windowLabel: "2026-W47",
      entityAliasId: "alias-north-1",
      entityRecordId: null,
      entityLabel: "@handle_north_1",
      status: "imported_unverified",
      effectiveAt: "2026-11-17T08:00:00Z",
      observedAt: "2026-11-16T09:04:00Z",
      recordedAt: null,
      fields: [
        {
          id: "f-2-slot",
          name: "slot_code",
          label: "Window slot",
          extractedValue: "SLOT-02",
          value: "SLOT-02",
          confidenceBps: 9650,
          box: { x: "0.32", y: "0.43", w: "0.24", h: "0.10" },
          findingIds: [],
        },
        {
          id: "f-2-hours",
          name: "duration_hours",
          label: "Duration (hours)",
          extractedValue: "7.50",
          value: "7.50",
          confidenceBps: 9420,
          box: { x: "0.59", y: "0.43", w: "0.16", h: "0.10" },
          findingIds: [],
        },
      ],
      findings: [],
      ambiguities: [],
      receipt: null,
    },
    {
      id: "row-w47-3",
      artifactId: "art-w47-a",
      targetObjectType: "canon.Allocation",
      windowLabel: "2026-W47",
      entityAliasId: null,
      entityRecordId: "rec-003",
      entityLabel: "Unit Gamma",
      status: "quarantined",
      effectiveAt: "2026-11-17T08:00:00Z",
      observedAt: "2026-11-16T09:04:00Z",
      recordedAt: null,
      fields: [
        {
          id: "f-3-slot",
          name: "slot_code",
          label: "Window slot",
          extractedValue: "SLOT-02",
          value: "SLOT-02",
          confidenceBps: 9700,
          box: { x: "0.32", y: "0.56", w: "0.24", h: "0.10" },
          findingIds: [],
        },
        {
          id: "f-3-hours",
          name: "duration_hours",
          label: "Duration (hours)",
          extractedValue: "16.00",
          value: "16.00",
          confidenceBps: 9510,
          box: { x: "0.59", y: "0.56", w: "0.16", h: "0.10" },
          findingIds: ["finding-obs-bad"],
        },
      ],
      findings: [
        {
          id: "finding-obs-bad",
          code: "OBS-BAD",
          ruleId: "canon.allocation.max_window_duration",
          severity: "error",
          message: "Double-booked window exceeds the 12.00 hour cap (observed 16.00).",
          fieldName: "duration_hours",
          status: "open",
        },
      ],
      ambiguities: [],
      receipt: null,
    },
    {
      id: "row-w47-4",
      artifactId: "art-w47-a",
      targetObjectType: "canon.Allocation",
      windowLabel: "2026-W47",
      entityAliasId: null,
      entityRecordId: "rec-004",
      entityLabel: "Unit Delta",
      status: "quarantined",
      effectiveAt: "2026-02-31",
      observedAt: "2026-11-16T09:04:00Z",
      recordedAt: null,
      fields: [
        {
          id: "f-4-date",
          name: "effective_date",
          label: "Effective date",
          extractedValue: "2026-02-31",
          value: "2026-02-31",
          confidenceBps: 9300,
          box: { x: "0.32", y: "0.69", w: "0.24", h: "0.10" },
          findingIds: ["finding-invalid-date"],
        },
        {
          id: "f-4-hours",
          name: "duration_hours",
          label: "Duration (hours)",
          extractedValue: "6.00",
          value: "6.00",
          confidenceBps: 9600,
          box: { x: "0.59", y: "0.69", w: "0.16", h: "0.10" },
          findingIds: [],
        },
      ],
      findings: [
        {
          id: "finding-invalid-date",
          code: "InvalidTemporalObservation",
          ruleId: "core.temporal.valid_calendar_date",
          severity: "error",
          message: "Date 2026-02-31 is not a valid calendar date.",
          fieldName: "effective_date",
          status: "open",
        },
      ],
      ambiguities: [],
      receipt: null,
    },
    // Disputed row referenced by the unresolved FEED-CONFLICT: held in quarantine until governed resolution.
    {
      id: "row-w47-5",
      artifactId: "art-w47-a",
      targetObjectType: "canon.Allocation",
      windowLabel: "2026-W47",
      entityAliasId: null,
      entityRecordId: "rec-005",
      entityLabel: "Unit Zeta",
      status: "quarantined",
      effectiveAt: "2026-11-18T08:00:00Z",
      observedAt: "2026-11-16T09:04:00Z",
      recordedAt: null,
      fields: [
        {
          id: "f-5-slot",
          name: "slot_code",
          label: "Window slot",
          extractedValue: "SLOT-09",
          value: "SLOT-09",
          confidenceBps: 9540,
          box: { x: "0.32", y: "0.82", w: "0.24", h: "0.10" },
          findingIds: ["finding-feed-conflict"],
        },
        {
          id: "f-5-hours",
          name: "duration_hours",
          label: "Duration (hours)",
          extractedValue: "8.00",
          value: "8.00",
          confidenceBps: 9600,
          box: { x: "0.59", y: "0.82", w: "0.16", h: "0.10" },
          findingIds: ["finding-feed-conflict"],
        },
      ],
      findings: [
        {
          id: "finding-feed-conflict",
          code: "FEED-CONFLICT",
          ruleId: "core.boundary.import_finding",
          severity: "error",
          message: "Conflicting superseding capture for zone-south/2026-W47 — resolve in Step 3 before promotion.",
          fieldName: null,
          status: "open",
        },
      ],
      ambiguities: [],
      receipt: null,
    },
  ]

  if (options.includeWithheldRow) {
    rows.push({
      id: "row-w47-withheld",
      artifactId: "art-w47-a",
      targetObjectType: "canon.Allocation",
      windowLabel: "2026-W47",
      entityAliasId: null,
      entityRecordId: "rec-001",
      entityLabel: "Unit Alpha",
      status: "imported_unverified",
      effectiveAt: "2026-11-19T08:00:00Z",
      observedAt: "2026-11-16T09:04:00Z",
      recordedAt: null,
      fields: [
        {
          id: "f-w-slot",
          name: "slot_code",
          label: "Window slot",
          extractedValue: "SLOT-04",
          value: "SLOT-04",
          confidenceBps: 9600,
          box: null,
          findingIds: [],
        },
        {
          kind: "withheld",
          id: "f-w-note",
          name: "contact_note",
          label: "Contact note",
          masking: "redacted",
          displayMask: "Redacted from your role",
          markings: ["pii.contact"],
          confidenceBps: null,
          box: null,
          findingIds: [],
        },
      ],
      findings: [],
      ambiguities: [],
      receipt: null,
    })
  }

  // FEED-CONFLICT scopes strictly to the disputed row (`row-w47-5`), never clean promotable rows.
  const supersessions: SupersessionConflict[] = [
    {
      id: "conflict-w47-south",
      findingCode: "FEED-CONFLICT",
      windowLabel: "2026-W47",
      scopeKey: "zone-south/2026-W47",
      earlier: {
        artifactId: "art-w47-a",
        label: "Capture A · W47 Grid (09:04)",
        blobDigest: hash(101),
        functionReceiptDigest: hash(201),
        observedAt: "2026-11-16T09:04:00Z",
        sourceChannel: "feed-ops-south",
      },
      newer: {
        artifactId: "art-w47-b",
        label: "Capture B · W47 Revised (11:19)",
        blobDigest: hash(102),
        functionReceiptDigest: hash(202),
        observedAt: "2026-11-16T11:19:00Z",
        sourceChannel: "feed-ops-south",
      },
      cells: [
        {
          id: "cell-1",
          rowKey: "row-w47-5",
          entityLabel: "Unit Zeta",
          fieldName: "duration_hours",
          fieldLabel: "Duration (hours)",
          earlierValue: "8.00",
          newerValue: "8.50",
          changed: true,
          newerConfidenceBps: 9740,
        },
        {
          id: "cell-2",
          rowKey: "row-w47-5",
          entityLabel: "Unit Zeta",
          fieldName: "slot_code",
          fieldLabel: "Window slot",
          earlierValue: "SLOT-09",
          newerValue: "SLOT-10",
          changed: true,
          newerConfidenceBps: 8650,
        },
        {
          id: "cell-3",
          rowKey: "row-w47-5",
          entityLabel: "Unit Zeta",
          fieldName: "zone_code",
          fieldLabel: "Zone",
          earlierValue: "SOUTH",
          newerValue: "SOUTH",
          changed: false,
          newerConfidenceBps: 9810,
        },
      ],
      defaultChoice: "accept_newer",
      resolution: null,
    },
  ]

  return {
    batchId: options.batchId ?? "batch-w47-sync",
    title: "Window W47 multi-capture batch",
    targetObjectType: "canon.Allocation",
    lowConfidenceThresholdBps: 9000,
    actions: {
      bindAlias: "canon.alias.bind",
      createEntity: "canon.entity.create_and_bind",
      ignoreAlias: "canon.alias.ignore",
      promoteRow: "canon.import.promote_row",
      resolveSupersession: "canon.import.resolve_supersession",
    },
    aliases,
    availableEntities: entities,
    artifacts,
    rows,
    supersessions,
  }
}

export async function importTriageCell(
  page: Page,
  options: { readonly includeWithheldRow?: boolean; readonly artifactMediaMode?: "data" | "cell-blob" | "broken-blob" | "external" } = {},
): Promise<ImportTriageCell> {
  let workspace = createFixtureWorkspace(options)
  let seq = 500
  const calls: TriageCall[] = []
  const blobRequests: string[] = []
  const refusals = new Map<string, { code: string; message: string }>()
  const pendings = new Map<string, string>()
  const promoteFailures = new Set<string>()
  const supersessionFailures = new Set<string>()
  const holds = new Map<string, Promise<void>>()
  let heldRead: Promise<void> | null = null

  const scoped = (url: URL) => url.searchParams.get("orgId") === "org_demo" && url.searchParams.get("environmentId") === "analytics"
  const refuse = (route: Route, status: number, code: string, message: string) =>
    route.fulfill({ status, json: { format: "norn/refusal@2", code, category: status === 403 ? "permission" : "precondition", retryable: false, reason: null, message, explanation: message, diagnostics: [{ code, severity: "error", message, span: null }] } })

  await page.route("**/api/host/v1/blobs/**", async (route) => {
    const url = new URL(route.request().url())
    blobRequests.push(`${url.pathname}${url.search}`)
    if (!scoped(url)) return refuse(route, 403, "scope.denied", "Not your workspace.")
    if (url.pathname.endsWith("/capture-w47-broken")) {
      return route.fulfill({ status: 200, contentType: "image/png", body: Buffer.from("not-a-valid-png") })
    }
    return route.fulfill({ status: 200, contentType: "image/svg+xml", body: ARTIFACT_A_SVG })
  })

  await page.route("**/api/host/v1/imports/triage**", async (route) => {
    const url = new URL(route.request().url())
    if (!scoped(url)) return refuse(route, 403, "scope.denied", "Not your workspace.")
    if (heldRead) {
      const waitFor = heldRead
      heldRead = null
      await waitFor
    }
    return route.fulfill({ json: workspace })
  })

  await page.route("**/api/host/v1/actions/**", async (route) => {
    const url = new URL(route.request().url())
    if (!scoped(url)) return refuse(route, 403, "scope.denied", "Not your workspace.")
    const actionId = decodeURIComponent(url.pathname.split("/v1/actions/")[1] ?? "")
    const body = JSON.parse(route.request().postData() ?? "{}") as { input?: Record<string, unknown>; idempotencyKey?: string }
    const input = body.input ?? {}
    const idempotencyKey = String(body.idempotencyKey ?? "")
    calls.push({ actionId, idempotencyKey, input })

    if (actionId === workspace.actions.promoteRow) {
      const rowId = String(input.rowId ?? "")
      const held = holds.get(rowId)
      if (held) {
        holds.delete(rowId)
        await held
      }
      if (promoteFailures.has(rowId)) {
        promoteFailures.delete(rowId)
        // Structured JSON 500 error (must remain retryable, NOT a non-retryable refusal).
        return route.fulfill({
          status: 500,
          json: { format: "norn/refusal@2", code: "internal.storage_busy", category: "internal", retryable: true, reason: null, message: "Upstream ledger timeout", explanation: "the ledger did not answer in time" },
        })
      }
      const pendingDecision = pendings.get(rowId)
      if (pendingDecision) {
        pendings.delete(rowId)
        return route.fulfill({
          status: 202,
          json: { outcome: "pending", decision: { id: pendingDecision, expiresAtMs: Date.UTC(2026, 10, 24, 12, 0, 0) } },
        })
      }
      const injected = refusals.get(rowId)
      if (injected) {
        refusals.delete(rowId)
        return refuse(route, 422, injected.code, injected.message)
      }
      const row = workspace.rows.find((candidate) => candidate.id === rowId)
      if (row && row.findings.some((f) => f.status === "open")) {
        return refuse(route, 422, "law.violation_quarantined", "Row has an open finding or FEED-CONFLICT hold and stays quarantined.")
      }
    }

    if (actionId === workspace.actions.resolveSupersession) {
      const conflictId = String(input.conflictId ?? "")
      if (supersessionFailures.has(conflictId)) {
        supersessionFailures.delete(conflictId)
        return route.fulfill({
          status: 500,
          json: { format: "norn/refusal@2", code: "internal.storage_busy", category: "internal", retryable: true, reason: null, message: "Upstream ledger timeout", explanation: "the ledger did not answer in time" },
        })
      }
    }

    seq += 1
    const recordedAtMs = Date.UTC(2026, 10, 23, 15, 45, seq % 60)
    const objectId = actionId === workspace.actions.createEntity
      ? `rec-${String(seq).padStart(3, "0")}`
      : String(input.rowId ?? input.targetRecordId ?? input.conflictId ?? input.aliasId ?? `obj-${seq}`)
    const receipt = {
      outcome: "committed" as const,
      objectType: workspace.targetObjectType,
      objectId,
      version: 1,
      receipt: { receiptHash: hash(seq).replace("sha256:", ""), commitSeq: seq, recordedAtMs },
    }

    if (input.batchId !== workspace.batchId) {
      return route.fulfill({ json: receipt })
    }

    if (actionId === workspace.actions.bindAlias) {
      const aliasId = String(input.aliasId)
      const targetId = String(input.targetRecordId)
      const targetLabel = workspace.availableEntities.find((e) => e.id === targetId)?.label ?? targetId
      workspace = {
        ...workspace,
        aliases: workspace.aliases.map((a) => a.id === aliasId ? { ...a, resolution: { kind: "bound", targetId, targetLabel, receipt } } : a),
        rows: workspace.rows.map((r) => r.entityAliasId === aliasId ? { ...r, entityRecordId: targetId, entityLabel: targetLabel } : r),
      }
    } else if (actionId === workspace.actions.createEntity) {
      const aliasId = String(input.aliasId)
      const label = String(input.label)
      const created: TargetEntity = { id: objectId, label }
      workspace = {
        ...workspace,
        availableEntities: [...workspace.availableEntities, created],
        aliases: workspace.aliases.map((a) => a.id === aliasId ? { ...a, resolution: { kind: "created", targetId: objectId, targetLabel: label, receipt } } : a),
      }
    } else if (actionId === workspace.actions.ignoreAlias) {
      const aliasId = String(input.aliasId)
      const note = typeof input.note === "string" ? input.note : undefined
      workspace = {
        ...workspace,
        aliases: workspace.aliases.map((a) => a.id === aliasId ? { ...a, resolution: { kind: "ignored", ...note ? { note } : {}, receipt } } : a),
      }
    } else if (actionId === workspace.actions.promoteRow) {
      const rowId = String(input.rowId)
      const values = (input.values ?? {}) as Record<string, string>
      workspace = {
        ...workspace,
        rows: workspace.rows.map((r) => r.id !== rowId ? r : {
          ...r,
          status: "verified_fact",
          recordedAt: new Date(recordedAtMs).toISOString(),
          receipt,
          fields: r.fields.map((f) => f.kind === "withheld" ? f : { ...f, value: values[f.name] ?? f.value }),
        }),
      }
    } else if (actionId === workspace.actions.resolveSupersession) {
      const conflictId = String(input.conflictId)
      const choice = input.choice as "accept_newer" | "keep_earlier" | "keep_disputed"
      const note = typeof input.note === "string" ? input.note : undefined
      workspace = {
        ...workspace,
        supersessions: workspace.supersessions.map((s) => s.id === conflictId ? {
          ...s,
          resolution: {
            choice,
            ...note ? { note } : {},
            preservedHistoricalArtifactId: choice === "keep_earlier" ? s.newer.artifactId : s.earlier.artifactId,
            // Cell explicitly reports remaining verification status (imported_unverified until row promotion).
            factStatus: choice === "keep_disputed" ? "quarantined" : "imported_unverified",
            receipt,
          },
        } : s),
        // Governed conflict resolution releases the FEED-CONFLICT hold and aligns the row's
        // evidence artifact, observedAt, and field values with the accepted capture.
        rows: choice === "keep_disputed" ? workspace.rows : workspace.rows.map((r) => {
          if (r.id !== "row-w47-5") return r
          const conflict = workspace.supersessions.find((s) => s.id === conflictId)
          const acceptedRef = choice === "accept_newer" ? conflict?.newer : conflict?.earlier
          const nextFields = r.fields.map((f) => {
            if (f.kind === "withheld") return f
            if (choice === "accept_newer" && f.name === "duration_hours") {
              return {
                ...f,
                extractedValue: "8.50",
                value: "8.50",
                box: { x: "0.59", y: "0.30", w: "0.16", h: "0.10" },
              }
            }
            if (choice === "accept_newer" && f.name === "slot_code") {
              return {
                ...f,
                extractedValue: "SLOT-10",
                value: "SLOT-10",
                box: { x: "0.32", y: "0.30", w: "0.24", h: "0.10" },
              }
            }
            return f
          })
          return {
            ...r,
            artifactId: acceptedRef?.artifactId ?? r.artifactId,
            observedAt: acceptedRef?.observedAt ?? r.observedAt,
            status: "imported_unverified",
            fields: nextFields,
            findings: r.findings.map((f) => f.code === "FEED-CONFLICT" ? { ...f, status: "resolved" as const } : f),
          }
        }),
      }
    }

    return route.fulfill({ json: receipt })
  })

  return {
    calls,
    blobRequests,
    refuseNextPromote: (rowId, code, message) => { refusals.set(rowId, { code, message }) },
    pendingNextPromote: (rowId, decisionId) => { pendings.set(rowId, decisionId) },
    failNextPromote: (rowId) => { promoteFailures.add(rowId) },
    failNextSupersession: (conflictId) => { supersessionFailures.add(conflictId) },
    holdNextPromote: (rowId) => {
      let release!: () => void
      holds.set(rowId, new Promise<void>((resolve) => { release = resolve }))
      return release
    },
    holdNextTriageRead: () => {
      let release!: () => void
      heldRead = new Promise<void>((resolve) => { release = resolve })
      return release
    },
    commitRowElsewhere: (rowId, values) => {
      seq += 1
      const recordedAtMs = Date.UTC(2026, 10, 23, 16, 0, seq % 60)
      const receipt = {
        outcome: "committed" as const,
        objectType: workspace.targetObjectType,
        objectId: rowId,
        version: 2,
        receipt: { receiptHash: hash(seq).replace("sha256:", ""), commitSeq: seq, recordedAtMs },
      }
      workspace = {
        ...workspace,
        rows: workspace.rows.map((r) => r.id !== rowId ? r : {
          ...r,
          status: "verified_fact",
          recordedAt: new Date(recordedAtMs).toISOString(),
          receipt,
          fields: r.fields.map((f) => f.kind === "withheld" ? f : { ...f, value: values[f.name] ?? f.value }),
        }),
      }
    },
    switchBatch: (nextBatchId) => {
      workspace = createFixtureWorkspace({ ...options, batchId: nextBatchId })
    },
  }
}
