/**
 * Domain-agnostic contract for the Split-Pane Import Triage surface.
 *
 * Reads staged extraction batches from `GET /v1/imports/triage`, and executes every state change
 * (alias binding, entity creation, alias ignore, candidate row promotion, and supersession
 * resolution) strictly through governed cell actions (`POST /v1/actions/{actionId}` with an
 * explicit `idempotencyKey`). Extraction confidence is strictly advisory and never confers write
 * authority or bypasses cell laws.
 */
import type { FactStatus } from "../components/fact-status-badge.tsx"
import { callAction, newIdempotencyKey, type ActionReceipt, type CallResult } from "./actions.ts"
import { cellCall, type CellResult, type Fetch } from "./cell-read.ts"
import { readableTime } from "./format.ts"

export const IMPORT_TRIAGE_UNAVAILABLE = "This cell doesn't serve import triage batches yet."

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
  /** Direct web/data URI or an object-store URI served via `/v1/uploads:read`. */
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

export interface ExtractedField {
  readonly id: string
  readonly name: string
  readonly label: string
  readonly extractedValue: string
  readonly value: string
  readonly confidenceBps: number | null
  readonly box: NormalizedBox | null
  readonly findingIds: readonly string[]
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

export interface CandidateRow {
  readonly id: string
  readonly artifactId: string
  readonly targetObjectType: string
  readonly windowLabel: string
  readonly entityAliasId: string | null
  readonly entityRecordId: string | null
  readonly entityLabel: string
  readonly status: FactStatus
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

/** Whether a candidate row has a resolved canonical entity record. */
export function hasResolvedEntity(
  row: CandidateRow,
  resolvedAliases: ReadonlyMap<string, AliasResolution>,
): boolean {
  if (row.entityRecordId !== null) return true
  if (row.entityAliasId === null) return false
  const resolution = resolvedAliases.get(row.entityAliasId)
  return resolution?.kind === "bound" || resolution?.kind === "created"
}

/**
 * A row is eligible for one-click batch governed promotion ONLY when it is not yet verified, has
 * zero open law/contract violations, is not quarantined, and resolves to a governed entity record.
 * Low-confidence amber fields remain editable for operator review before promotion; red law
 * violations are never eligible for silent promotion.
 */
export function isRowEligibleForBatchPromotion(
  row: CandidateRow,
  resolvedAliases: ReadonlyMap<string, AliasResolution>,
): boolean {
  if (row.status === "verified_fact") return false
  if (row.status === "quarantined") return false
  if (hasOpenLawViolation(row)) return false
  return hasResolvedEntity(row, resolvedAliases)
}

/** Partition candidate rows into clean promotable rows, held/quarantined rows, and verified rows. */
export function partitionPromotionBatch(
  rows: readonly CandidateRow[],
  resolvedAliases: ReadonlyMap<string, AliasResolution>,
): {
  readonly eligible: readonly CandidateRow[]
  readonly quarantined: readonly CandidateRow[]
  readonly blockedByAlias: readonly CandidateRow[]
  readonly verified: readonly CandidateRow[]
} {
  const eligible: CandidateRow[] = []
  const quarantined: CandidateRow[] = []
  const blockedByAlias: CandidateRow[] = []
  const verified: CandidateRow[] = []
  for (const row of rows) {
    if (row.status === "verified_fact") {
      verified.push(row)
    } else if (row.status === "quarantined" || hasOpenLawViolation(row)) {
      quarantined.push(row)
    } else if (!hasResolvedEntity(row, resolvedAliases)) {
      blockedByAlias.push(row)
    } else {
      eligible.push(row)
    }
  }
  return { eligible, quarantined, blockedByAlias, verified }
}

/** Summary counts across all three steps for progressive go-live badges and step counters. */
export interface TriageSummary {
  readonly unresolvedAliases: number
  readonly resolvedAliases: number
  readonly unverifiedRows: number
  readonly quarantinedRows: number
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
  let verifiedRows = 0
  for (const row of workspace.rows) {
    if (row.status === "verified_fact") verifiedRows += 1
    else if (row.status === "quarantined" || hasOpenLawViolation(row)) quarantinedRows += 1
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
    verifiedRows,
    openConflicts,
    resolvedConflicts,
    totalAttention: unresolvedAliases + unverifiedRows + quarantinedRows + openConflicts,
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

export function fetchImportTriage(fetcher?: Fetch): Promise<CellResult<ImportTriageWorkspace>> {
  return cellCall<ImportTriageWorkspace>("/v1/imports/triage", {}, fetcher)
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
