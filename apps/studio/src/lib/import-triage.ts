/**
 * Domain-agnostic contract for the Split-Pane Import Triage surface.
 *
 * Reads staged extraction batches from `GET /v1/imports/triage`, and executes every state change
 * (alias binding, entity creation, alias ignore, candidate row promotion, and supersession
 * resolution) strictly through governed cell actions (`POST /v1/actions/{actionId}` with an
 * explicit `idempotencyKey`). Extraction confidence is strictly advisory and never confers write
 * authority or bypasses cell laws.
 */
import { deriveFactStatus, type FactStatus } from "../components/fact-status-badge.tsx"
import { callAction, newIdempotencyKey, type ActionReceipt, type CallResult } from "./actions.ts"
import { cellCall, type CellResult, type Fetch } from "./cell-read.ts"
import { readableTime } from "./format.ts"

export const IMPORT_TRIAGE_UNAVAILABLE = "This cell doesn't serve import triage yet."

/** Normalized bounding box on a source artifact (top-left origin, 0..1 page fractions as exact text). */
export interface NormalizedBox {
  readonly x: string
  readonly y: string
  readonly w: string
  readonly h: string
}

/** Governed cell action IDs declared by the batch; Studio submits every decision through these actions. */
export interface TriageActions {
  readonly bindAlias: string
  readonly createEntity: string
  readonly ignoreAlias: string
  readonly promoteRow: string
  readonly resolveSupersession: string
}

export interface TargetEntity {
  readonly id: string
  readonly label: string
  readonly subtitle?: string
}

export type AliasResolution =
  | { readonly kind: "bound"; readonly targetId: string; readonly targetLabel: string; readonly receipt: ActionReceipt }
  | { readonly kind: "created"; readonly targetId: string; readonly targetLabel: string; readonly receipt: ActionReceipt }
  | { readonly kind: "ignored"; readonly note?: string; readonly receipt: ActionReceipt }

export interface AliasCandidate {
  readonly id: string
  readonly externalHandle: string
  readonly sourceChannel: string
  readonly occurrences: number
  readonly sampleExcerpt: string
  readonly firstObservedAt: string
  /** Advisory suggestion only — never auto-linked; the operator must explicitly decide. */
  readonly suggestedTargetId: string | null
  readonly suggestedTargetLabel: string | null
  readonly suggestionConfidenceBps: number | null
  readonly resolution: AliasResolution | null
}

export interface SourceContextMessage {
  readonly id: string
  readonly senderHandle: string
  readonly aliasId?: string | null
  readonly observedAt: string
  readonly body: string
  readonly isAnchor?: boolean
  readonly markings?: readonly string[]
}

export interface StagedArtifact {
  readonly id: string
  readonly label: string
  readonly sourceChannel: string
  readonly mediaType: string
  /** Cell-scoped blob/upload path, object-store URI served via `/v1/uploads:read`, or local synthetic data URI. */
  readonly documentUri: string
  readonly blobDigest: string
  readonly functionReceiptDigest: string
  readonly extractor: string
  readonly extractorVersion: string
  readonly promptDigest?: string
  readonly observedAt: string
  readonly windowLabel: string
  readonly markings: readonly string[]
  readonly contextMessages: readonly SourceContextMessage[]
}

/** An extracted field whose readable value may be reviewed and edited before governed promotion. */
export interface ExtractedValueField {
  readonly kind?: "value"
  readonly id: string
  readonly name: string
  readonly label: string
  readonly extractedValue: string
  readonly value: string
  readonly confidenceBps: number | null
  readonly box: NormalizedBox | null
  readonly findingIds: readonly string[]
}

/**
 * A field withheld or redacted by cell policy: rendered read-only with its masking indicator and
 * never edited or submitted as a literal placeholder string.
 */
export interface WithheldExtractedField {
  readonly kind: "withheld"
  readonly id: string
  readonly name: string
  readonly label: string
  readonly masking: "redacted" | "pseudonym" | "refused"
  readonly displayMask?: string
  readonly markings?: readonly string[]
  readonly confidenceBps: number | null
  readonly box: NormalizedBox | null
  readonly findingIds: readonly string[]
}

export type ExtractedField = ExtractedValueField | WithheldExtractedField

export function isFieldWithheld(field: ExtractedField): field is WithheldExtractedField {
  return field.kind === "withheld"
}

export interface RowFinding {
  readonly id: string
  readonly code: string
  readonly ruleId: string
  readonly severity: "warning" | "error" | "critical"
  readonly message: string
  readonly fieldName: string | null
  readonly status: "open" | "acknowledged" | "resolved" | "superseded"
}

/** Honest client-observed outcome of a governed row promotion call (never fabricated as a cell finding). */
export type RowActionOutcome =
  | { readonly kind: "pending"; readonly decisionId: string; readonly expiresAtMs: number }
  | { readonly kind: "refused"; readonly code: string; readonly message: string }
  | { readonly kind: "error"; readonly message: string }

export interface CandidateRow {
  readonly id: string
  readonly artifactId: string
  readonly targetObjectType: string
  readonly windowLabel: string
  readonly entityAliasId: string | null
  readonly entityRecordId: string | null
  readonly entityLabel: string
  readonly status: FactStatus
  readonly withheld?: boolean
  readonly effectiveAt: string
  readonly observedAt: string
  readonly recordedAt: string | null
  readonly fields: readonly ExtractedField[]
  readonly findings: readonly RowFinding[]
  readonly ambiguities: readonly string[]
  readonly receipt: ActionReceipt | null
}

export interface SupersessionArtifactRef {
  readonly artifactId: string
  readonly label: string
  readonly blobDigest: string
  readonly functionReceiptDigest: string
  readonly observedAt: string
  readonly sourceChannel: string
}

export interface SupersessionDiffCell {
  readonly id: string
  readonly rowKey: string
  readonly entityLabel: string
  readonly fieldName: string
  readonly fieldLabel: string
  readonly earlierValue: string
  readonly newerValue: string
  readonly changed: boolean
  readonly newerConfidenceBps: number | null
  readonly hasFinding?: boolean
}

export type SupersessionChoice = "accept_newer" | "keep_earlier" | "keep_disputed"

export interface SupersessionResolution {
  readonly choice: SupersessionChoice
  readonly note?: string
  readonly preservedHistoricalArtifactId: string
  readonly receipt: ActionReceipt
}

export interface SupersessionConflict {
  readonly id: string
  readonly findingCode: "FEED-CONFLICT"
  readonly windowLabel: string
  readonly scopeKey: string
  readonly earlier: SupersessionArtifactRef
  readonly newer: SupersessionArtifactRef
  readonly cells: readonly SupersessionDiffCell[]
  readonly defaultChoice: "accept_newer"
  readonly resolution: SupersessionResolution | null
}

export interface ImportTriageWorkspace {
  readonly batchId: string
  readonly title: string
  readonly targetObjectType: string
  readonly lowConfidenceThresholdBps: number
  readonly actions: TriageActions
  readonly aliases: readonly AliasCandidate[]
  readonly availableEntities: readonly TargetEntity[]
  readonly artifacts: readonly StagedArtifact[]
  readonly rows: readonly CandidateRow[]
  readonly supersessions: readonly SupersessionConflict[]
}

export type TriageStep = "aliases" | "review" | "supersession"

export function isTriageStep(value: unknown): value is TriageStep {
  return value === "aliases" || value === "review" || value === "supersession"
}

/** Whether an extracted field's confidence falls below the batch's low-confidence threshold. */
export function isLowConfidence(confidenceBps: number | null, thresholdBps: number): boolean {
  return confidenceBps !== null && confidenceBps < thresholdBps
}

/** Format basis points (0..10000) as a fixed one-decimal percentage, or "—" when absent. */
export function formatConfidence(confidenceBps: number | null): string {
  if (confidenceBps === null) return "—"
  return `${(confidenceBps / 100).toFixed(1)}%`
}

/** Open law or contract violations on a candidate row (never silently bypassable). */
export function openFindingsOf(row: CandidateRow): readonly RowFinding[] {
  return row.findings.filter((finding) => finding.status === "open")
}

export function hasOpenLawViolation(row: CandidateRow): boolean {
  return openFindingsOf(row).length > 0
}

/** Whether a row itself or any of its extracted fields is withheld/redacted. */
export function hasWithheldContent(row: CandidateRow): boolean {
  return row.withheld === true || row.status === "withheld" || row.fields.some(isFieldWithheld)
}

/**
 * Honest effective status of a candidate row. Quarantined and Withheld ALWAYS take precedence
 * over Verified fact, so a row with an open finding or withheld field never claims "Verified fact".
 */
export function effectiveRowStatus(row: CandidateRow): FactStatus {
  return deriveFactStatus({
    quarantined: row.status === "quarantined",
    openFindings: openFindingsOf(row).length,
    withheld: hasWithheldContent(row),
    unverifiedImport: row.status === "imported_unverified",
    verified: row.status === "verified_fact",
  }) ?? "imported_unverified"
}

/**
 * Resolve a row's governed canonical entity. When a row references an `entityAliasId`, that alias
 * MUST have an explicit governed resolution (`bound` or `created`) — a pre-populated
 * `entityRecordId` can never bypass an unresolved or ignored alias.
 */
export function resolveRowEntity(
  row: CandidateRow,
  resolvedAliases: ReadonlyMap<string, AliasResolution>,
): { readonly kind: "resolved"; readonly targetId: string; readonly label: string }
  | { readonly kind: "ignored"; readonly label: string }
  | { readonly kind: "unresolved"; readonly label: string } {
  if (row.entityAliasId !== null) {
    const resolution = resolvedAliases.get(row.entityAliasId)
    if (resolution?.kind === "bound" || resolution?.kind === "created") {
      return { kind: "resolved", targetId: resolution.targetId, label: resolution.targetLabel }
    }
    if (resolution?.kind === "ignored") {
      return { kind: "ignored", label: row.entityLabel }
    }
    return { kind: "unresolved", label: row.entityLabel }
  }
  if (row.entityRecordId !== null) {
    return { kind: "resolved", targetId: row.entityRecordId, label: row.entityLabel }
  }
  return { kind: "unresolved", label: row.entityLabel }
}

export function hasResolvedEntity(
  row: CandidateRow,
  resolvedAliases: ReadonlyMap<string, AliasResolution>,
): boolean {
  return resolveRowEntity(row, resolvedAliases).kind === "resolved"
}

/**
 * Build the editable field map submitted on governed promotion. Withheld/redacted fields are
 * strictly excluded so masked placeholders are never written back as literal values.
 */
export function extractEditableValues(
  row: CandidateRow,
  edits: Readonly<Record<string, string>>,
): Readonly<Record<string, string>> {
  const values: Record<string, string> = {}
  for (const field of row.fields) {
    if (isFieldWithheld(field)) continue
    values[field.name] = edits[`${row.id}:${field.name}`] ?? field.value
  }
  return values
}

/**
 * A row is eligible for governed promotion ONLY when its effective status is `imported_unverified`
 * (not verified, not quarantined, not withheld), it has zero open law findings, is not currently
 * in flight, and resolves to a governed entity record.
 */
export function isRowEligibleForBatchPromotion(
  row: CandidateRow,
  resolvedAliases: ReadonlyMap<string, AliasResolution>,
  busyRowIds: ReadonlySet<string> = new Set(),
): boolean {
  if (busyRowIds.has(row.id)) return false
  if (effectiveRowStatus(row) !== "imported_unverified") return false
  return hasResolvedEntity(row, resolvedAliases)
}

/** Partition candidate rows into clean promotable rows, held/quarantined rows, withheld rows, alias-blocked rows, and verified rows. */
export function partitionPromotionBatch(
  rows: readonly CandidateRow[],
  resolvedAliases: ReadonlyMap<string, AliasResolution>,
  busyRowIds: ReadonlySet<string> = new Set(),
): {
  readonly eligible: readonly CandidateRow[]
  readonly quarantined: readonly CandidateRow[]
  readonly withheld: readonly CandidateRow[]
  readonly blockedByAlias: readonly CandidateRow[]
  readonly inFlight: readonly CandidateRow[]
  readonly verified: readonly CandidateRow[]
} {
  const eligible: CandidateRow[] = []
  const quarantined: CandidateRow[] = []
  const withheld: CandidateRow[] = []
  const blockedByAlias: CandidateRow[] = []
  const inFlight: CandidateRow[] = []
  const verified: CandidateRow[] = []
  for (const row of rows) {
    const status = effectiveRowStatus(row)
    if (status === "verified_fact") {
      verified.push(row)
    } else if (status === "quarantined") {
      quarantined.push(row)
    } else if (status === "withheld") {
      withheld.push(row)
    } else if (!hasResolvedEntity(row, resolvedAliases)) {
      blockedByAlias.push(row)
    } else if (busyRowIds.has(row.id)) {
      inFlight.push(row)
    } else {
      eligible.push(row)
    }
  }
  return { eligible, quarantined, withheld, blockedByAlias, inFlight, verified }
}

/**
 * Authorize how an artifact URI is loaded.
 * - Cell-scoped relative paths (`/api/host/v1/blobs/...`, `/api/host/v1/uploads/...`, `/v1/blobs/...`, `/v1/uploads/...`)
 *   and safe inline synthetic image data URIs (`data:image/svg+xml`, `png`, `webp`, `jpeg`) load directly.
 * - Object-store URIs (`s3://`, `r2://`, `blob://`, `norn-blob://`, `gs://`) are exchanged through
 *   the cell's policy-checked `POST /v1/uploads:read` endpoint.
 * - Arbitrary external `http://` / `https://` URLs are rejected so untrusted import payloads
 *   cannot trigger arbitrary external fetches or pixel tracking from the operator's browser.
 */
export type ArtifactMediaSource =
  | { readonly kind: "direct"; readonly url: string }
  | { readonly kind: "cell-scan"; readonly uri: string }
  | { readonly kind: "rejected"; readonly reason: string }

const SAFE_DATA_IMAGE = /^data:image\/(svg\+xml|png|webp|jpeg)(;charset=[^;,]+)?(;base64)?,/i
const CELL_MEDIA_PATH = /^\/(api\/host\/)?v1\/(blobs|uploads)\//
const OBJECT_STORE_URI = /^(s3|r2|gs|blob|norn-blob):\/\//i

export function resolveArtifactMediaSource(uri: string): ArtifactMediaSource {
  const trimmed = uri.trim()
  if (trimmed === "") {
    return { kind: "rejected", reason: "Artifact carries no media URI." }
  }
  if (SAFE_DATA_IMAGE.test(trimmed)) {
    return { kind: "direct", url: trimmed }
  }
  if (CELL_MEDIA_PATH.test(trimmed)) {
    return { kind: "direct", url: trimmed.startsWith("/api/host/") ? trimmed : `/api/host${trimmed}` }
  }
  if (OBJECT_STORE_URI.test(trimmed)) {
    return { kind: "cell-scan", uri: trimmed }
  }
  return {
    kind: "rejected",
    reason: "External artifact URLs are blocked; media must come from a cell-authorized blob or upload read.",
  }
}

/** Summary counts across all three steps for progressive go-live badges and step counters. */
export interface TriageSummary {
  readonly unresolvedAliases: number
  readonly resolvedAliases: number
  readonly unverifiedRows: number
  readonly quarantinedRows: number
  readonly withheldRows: number
  readonly verifiedRows: number
  readonly openConflicts: number
  readonly resolvedConflicts: number
  readonly totalAttention: number
}

export function triageSummary(workspace: ImportTriageWorkspace): TriageSummary {
  let unresolvedAliases = 0
  let resolvedAliases = 0
  for (const alias of workspace.aliases) {
    if (alias.resolution === null) unresolvedAliases += 1
    else resolvedAliases += 1
  }
  let unverifiedRows = 0
  let quarantinedRows = 0
  let withheldRows = 0
  let verifiedRows = 0
  for (const row of workspace.rows) {
    const status = effectiveRowStatus(row)
    if (status === "verified_fact") verifiedRows += 1
    else if (status === "quarantined") quarantinedRows += 1
    else if (status === "withheld") withheldRows += 1
    else unverifiedRows += 1
  }
  let openConflicts = 0
  let resolvedConflicts = 0
  for (const conflict of workspace.supersessions) {
    if (conflict.resolution === null) openConflicts += 1
    else resolvedConflicts += 1
  }
  return {
    unresolvedAliases,
    resolvedAliases,
    unverifiedRows,
    quarantinedRows,
    withheldRows,
    verifiedRows,
    openConflicts,
    resolvedConflicts,
    totalAttention: unresolvedAliases + unverifiedRows + quarantinedRows + withheldRows + openConflicts,
  }
}

/**
 * Format a valid ISO date or timestamp for display, preserving invalid calendar dates (e.g.
 * `2026-02-31`) verbatim instead of letting JS `Date` silently roll them into the next month.
 */
export function formatTemporal(text: string): string {
  const dateOnly = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text)
  if (dateOnly !== null) {
    const parsed = new Date(`${text}T00:00:00Z`)
    if (Number.isNaN(parsed.getTime()) || parsed.toISOString().slice(0, 10) !== text) return text
  }
  return readableTime(text)
}

/** Human-readable delta between two ISO timestamps (e.g. "+2h 15m later"). */
export function timeDeltaLabel(earlierIso: string, newerIso: string): string {
  const deltaMs = Date.parse(newerIso) - Date.parse(earlierIso)
  if (!Number.isFinite(deltaMs) || deltaMs <= 0) return "same timestamp"
  const totalMinutes = Math.round(deltaMs / 60_000)
  if (totalMinutes < 60) return `+${totalMinutes}m later`
  const hours = Math.floor(totalMinutes / 60)
  const minutes = totalMinutes % 60
  if (hours < 24) return minutes === 0 ? `+${hours}h later` : `+${hours}h ${minutes}m later`
  const days = Math.floor(hours / 24)
  const remHours = hours % 24
  return remHours === 0 ? `+${days}d later` : `+${days}d ${remHours}h later`
}

// ---- Cell reads & governed action commits ------------------------------------------------------

/**
 * Read the active import triage batch. Like `listActions`, any 404 (including structured
 * `route.not_found`) or 503 means this host does not serve import triage yet (`unavailable`).
 */
export async function fetchImportTriage(fetcher?: Fetch): Promise<CellResult<ImportTriageWorkspace>> {
  const result = await cellCall<ImportTriageWorkspace>("/v1/imports/triage", {}, fetcher)
  if (
    result.kind === "refused"
    && (result.refusal.status === 404 || result.refusal.status === 503 || result.refusal.code === "route.not_found")
  ) {
    return { kind: "unavailable" }
  }
  return result
}

export function commitAliasBinding(
  actionId: string,
  payload: {
    readonly batchId: string
    readonly aliasId: string
    readonly externalHandle: string
    readonly targetRecordId: string
  },
  idempotencyKey = newIdempotencyKey(),
  fetcher?: Fetch,
): Promise<CallResult> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher)
}

export function commitEntityCreateAndBind(
  actionId: string,
  payload: {
    readonly batchId: string
    readonly aliasId: string
    readonly externalHandle: string
    readonly label: string
    readonly externalKey?: string
  },
  idempotencyKey = newIdempotencyKey(),
  fetcher?: Fetch,
): Promise<CallResult> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher)
}

export function commitAliasIgnore(
  actionId: string,
  payload: {
    readonly batchId: string
    readonly aliasId: string
    readonly externalHandle: string
    readonly note?: string
  },
  idempotencyKey = newIdempotencyKey(),
  fetcher?: Fetch,
): Promise<CallResult> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher)
}

export function commitRowPromotion(
  actionId: string,
  payload: {
    readonly batchId: string
    readonly rowId: string
    readonly artifactId: string
    readonly blobDigest: string
    readonly functionReceiptDigest: string
    readonly targetRecordId: string
    readonly effectiveAt: string
    readonly observedAt: string
    readonly values: Readonly<Record<string, string>>
  },
  idempotencyKey = newIdempotencyKey(),
  fetcher?: Fetch,
): Promise<CallResult> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher)
}

export function commitSupersessionDecision(
  actionId: string,
  payload: {
    readonly batchId: string
    readonly conflictId: string
    readonly choice: SupersessionChoice
    readonly earlierArtifactId: string
    readonly earlierBlobDigest: string
    readonly newerArtifactId: string
    readonly newerBlobDigest: string
    readonly preserveEarlierEvidence: true
    readonly note?: string
  },
  idempotencyKey = newIdempotencyKey(),
  fetcher?: Fetch,
): Promise<CallResult> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher)
}
