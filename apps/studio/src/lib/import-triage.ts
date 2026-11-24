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
import { callAction, type CallAnswer } from "./actions.ts"
import { createIntentKey, type ActionReceipt } from "../../../../packages/norn-sdk/src/index.ts"
import { cellCall, type CellResult, type Fetch } from "./cell-read.ts"
import { requestedPath, requestedWords, type RequestedDecision } from "./decisions.ts"
import { CellUnavailable, readStudioScope, scopedHostFetch, type StudioScope } from "./definitions.ts"
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

/** Honest client-observed outcome of a governed action call (never fabricated as a cell finding). */
export type RowActionOutcome =
  | { readonly kind: "committed"; readonly receipt: ActionReceipt }
  | { readonly kind: "pending"; readonly decisionId: string; readonly expiresAtMs: number }
  | { readonly kind: "refused"; readonly code: string; readonly message: string }
  | { readonly kind: "stale"; readonly message: string }
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
  /** Optional authoritative fact status reported by the cell after resolution; never guessed client-side. */
  readonly factStatus?: FactStatus | null
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
  /** Optional authoritative cell status for the supersession window. */
  readonly factStatus?: FactStatus | null
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

// ---- Draft state helpers (payload-change-only idempotency key rotation) ------------------------

export type AliasDecisionMode = "match" | "create" | "ignore"

export interface AliasDraftState {
  readonly mode: AliasDecisionMode
  readonly selectedTargetId: string
  readonly createLabel: string
  readonly createExternalKey: string
  readonly ignoreNote: string
  /** Lazily allocated once per logical draft and preserved across retries; rotated only on payload edits. */
  readonly idempotencyKey: string | null
  readonly pending: boolean
  readonly notice: { readonly kind: "pending" | "refused" | "error"; readonly message: string } | null
}

export function initialAliasDraft(alias: AliasCandidate): AliasDraftState {
  return {
    mode: "match",
    // Strictly never pre-commit or auto-select a fuzzy ML guess: the operator picks explicitly.
    selectedTargetId: "",
    createLabel: alias.externalHandle.replace(/^@/, ""),
    createExternalKey: alias.externalHandle,
    ignoreNote: "",
    idempotencyKey: null,
    pending: false,
    notice: null,
  }
}

/**
 * Update an alias draft, rotating `idempotencyKey` ONLY when a logical payload field actually
 * changes. Re-clicking the already-selected mode or updating UI status (`pending`/`notice`) keeps
 * the exact same key so retries are idempotent.
 */
export function patchAliasDraftState(
  prev: AliasDraftState | undefined,
  alias: AliasCandidate,
  update: Partial<AliasDraftState>,
): AliasDraftState {
  const base = prev ?? initialAliasDraft(alias)
  const next: AliasDraftState = { ...base, ...update }
  const payloadChanged = next.mode !== base.mode
    || next.selectedTargetId !== base.selectedTargetId
    || next.createLabel !== base.createLabel
    || next.createExternalKey !== base.createExternalKey
    || next.ignoreNote !== base.ignoreNote
  if (payloadChanged && update.idempotencyKey === undefined) {
    return { ...next, idempotencyKey: null }
  }
  return next
}

export interface SupersessionDraftState {
  readonly choice: SupersessionChoice
  readonly note: string
  /** Lazily allocated once per logical draft and preserved across retries; rotated only on payload edits. */
  readonly idempotencyKey: string | null
  readonly pending: boolean
  readonly notice: { readonly kind: "pending" | "refused" | "error"; readonly message: string } | null
}

export function initialSupersessionDraft(conflict: SupersessionConflict): SupersessionDraftState {
  return {
    choice: conflict.defaultChoice,
    note: "",
    idempotencyKey: null,
    pending: false,
    notice: null,
  }
}

/**
 * Update a supersession draft, rotating `idempotencyKey` ONLY when `choice` or `note` actually
 * changes. Preserves the exact key across retries of the default supersession choice.
 */
export function patchSupersessionDraftState(
  prev: SupersessionDraftState | undefined,
  conflict: SupersessionConflict,
  update: Partial<SupersessionDraftState>,
): SupersessionDraftState {
  const base = prev ?? initialSupersessionDraft(conflict)
  const next: SupersessionDraftState = { ...base, ...update }
  const payloadChanged = next.choice !== base.choice || next.note !== base.note
  if (payloadChanged && update.idempotencyKey === undefined) {
    return { ...next, idempotencyKey: null }
  }
  return next
}

// ---- Canonical payload-bound idempotency key cache --------------------------------------------

function canonicalizeJsonValue(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(canonicalizeJsonValue).join(",")}]`
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, entryValue]) => entryValue !== undefined)
    .sort(([a], [b]) => a.localeCompare(b))
  return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonicalizeJsonValue(v)}`).join(",")}}`
}

/** Deterministic canonical fingerprint of an outgoing governed action call (`actionId` + payload). */
export function canonicalPayloadKey(actionId: string, payload: unknown): string {
  return `${actionId}:${canonicalizeJsonValue(payload)}`
}

export interface PayloadBoundKeyEntry {
  readonly payloadFingerprint: string
  readonly idempotencyKey: string
}

/**
 * Resolve an idempotency key bound strictly to the exact canonical outgoing payload for `scopeKey`:
 * - Retrying the exact same `(actionId, payload)` reuses the exact same `idempotencyKey`.
 * - Any change to the outgoing payload (operator edit OR a cell refresh updating evidence digests,
 *   timestamps, target record, or baseline field values) mints a fresh `idempotencyKey` instead of
 *   risking `action.idempotency_conflict`.
 * - No-op edits or resets that keep the outgoing payload unchanged preserve the exact same key.
 */
export function resolvePayloadIdempotencyKey(
  cache: Map<string, PayloadBoundKeyEntry>,
  scopeKey: string,
  actionId: string,
  payload: unknown,
  mintKey: () => string = createIntentKey,
): string {
  const payloadFingerprint = canonicalPayloadKey(actionId, payload)
  const existing = cache.get(scopeKey)
  if (existing && existing.payloadFingerprint === payloadFingerprint) {
    return existing.idempotencyKey
  }
  const nextKey = mintKey()
  cache.set(scopeKey, { payloadFingerprint, idempotencyKey: nextKey })
  return nextKey
}

/** Evict a cached idempotency key after a confirmed terminal approval failure so re-submission mints a fresh key. */
export function retirePayloadIdempotencyKey(
  cache: Map<string, PayloadBoundKeyEntry>,
  scopeKey: string,
): void {
  cache.delete(scopeKey)
}

// ---- Batch queue execution with live-basis, batch-switch & unmount guards ---------------------

/**
 * Deterministic fingerprint of a candidate row's authoritative cell basis:
 * `artifactId`, artifact `blobDigest` & `functionReceiptDigest`, temporal coordinates
 * (`effectiveAt`, `observedAt`), and canonical baseline field values (excluding withheld fields).
 *
 * Binding local edits and queued promotion items to `rowBasisKey` ensures an edit made against an
 * earlier capture never survives supersession into a newer capture, and a same-batch refresh that
 * changes baseline field values marks a queued promotion stale rather than submitting old values.
 */
export function rowBasisKey(row: CandidateRow, artifact: StagedArtifact | undefined): string {
  const baselineFields: Record<string, string> = {}
  for (const field of row.fields) {
    if (isFieldWithheld(field)) continue
    baselineFields[field.name] = field.value
  }
  return canonicalizeJsonValue({
    artifactId: row.artifactId,
    blobDigest: artifact?.blobDigest ?? null,
    functionReceiptDigest: artifact?.functionReceiptDigest ?? null,
    effectiveAt: row.effectiveAt,
    observedAt: row.observedAt,
    baselineFields,
  })
}

/** Scoped storage key for a local field edit, bound strictly to `(rowId, rowBasisKey, fieldName)`. */
export function fieldEditKey(rowId: string, basisKey: string, fieldName: string): string {
  return `${rowId}\u0000${basisKey}\u0000${fieldName}`
}

/** Whether a row outcome represents a submitted decision awaiting cell resolution (`committed` or `pending`). */
export function isOutcomeLocked(outcome: RowActionOutcome | undefined | null): boolean {
  return outcome?.kind === "committed" || outcome?.kind === "pending"
}

export interface CachedQueryState {
  readonly status?: string
  readonly error?: unknown
}

/**
 * Read the latest live workspace directly from the query cache at dispatch time, falling back to
 * the render-captured ref only when no cache entry exists yet. Fails closed (`undefined`) whenever
 * the cached result is non-`ok` OR the query state reports an error (`status === "error"` or
 * `error != null`, e.g. after a thrown refetch while TanStack Query retains stale `ok` data).
 */
export function readLiveWorkspaceFromCache(
  getCached: () => CellResult<ImportTriageWorkspace> | undefined,
  fallback: ImportTriageWorkspace | undefined,
  getQueryState?: () => CachedQueryState | undefined,
): ImportTriageWorkspace | undefined {
  const state = getQueryState?.()
  if (state && (state.status === "error" || state.error != null)) {
    return undefined
  }
  const cached = getCached()
  if (cached !== undefined) return cached.kind === "ok" ? cached.value : undefined
  return fallback
}

export type RequestedDecisionLookup =
  | { readonly kind: "found"; readonly decision: RequestedDecision }
  | { readonly kind: "absent" }
  | { readonly kind: "malformed" }

const VALID_REQUESTED_STATUSES = new Set(["pending", "approved", "rejected", "expired", "withdrawn"])
const VALID_DECIDED_CALL_STATES = new Set(["committed", "refused", "not_executed"])

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isConsistentDecidedCallState(value: unknown): boolean {
  if (!isPlainObject(value)) return false
  if (typeof value.committed !== "boolean" || typeof value.state !== "string") return false
  if (!VALID_DECIDED_CALL_STATES.has(value.state)) return false
  if (value.code !== undefined && typeof value.code !== "string") return false
  // Server invariant (crates/norn-api/src/decisions.rs): committed is true iff state === "committed".
  return value.committed === (value.state === "committed")
}

/**
 * Strictly validate a `GET /v1/decisions:requested?id=...` HTTP 200 payload for approval
 * reconciliation. Validates EVERY entry in `items` (plain object with non-empty string `id`)
 * before returning, and requires `approved` decisions to carry a valid, internally consistent
 * `call.state`. Any failure returns `{ kind: "malformed" }` so reconciliation fails closed and
 * preserves the `pending` lock.
 */
export function parseRequestedDecisionLookup(body: unknown, decisionId: string): RequestedDecisionLookup {
  if (!isPlainObject(body) || !Array.isArray(body.items)) {
    return { kind: "malformed" }
  }
  const items = body.items as unknown[]
  let matchedRaw: Record<string, unknown> | null = null
  for (const raw of items) {
    if (!isPlainObject(raw)) return { kind: "malformed" }
    if (typeof raw.id !== "string" || raw.id.trim() === "") return { kind: "malformed" }
    if (raw.id === decisionId) {
      if (matchedRaw !== null) return { kind: "malformed" }
      matchedRaw = raw
    }
  }
  if (matchedRaw === null) return { kind: "absent" }

  const status = matchedRaw.status
  const call = matchedRaw.call
  if (
    typeof status !== "string"
    || !VALID_REQUESTED_STATUSES.has(status)
    || typeof matchedRaw.canRun !== "boolean"
    || typeof matchedRaw.expiresAtMs !== "number"
    || !Number.isFinite(matchedRaw.expiresAtMs)
    || !isPlainObject(call)
    || typeof call.action !== "string"
    || typeof call.encodedInput !== "string"
    || typeof call.idempotencyKey !== "string"
  ) {
    return { kind: "malformed" }
  }
  if (status === "approved") {
    if (!isConsistentDecidedCallState(call.state)) return { kind: "malformed" }
  } else if (call.state !== undefined && !isConsistentDecidedCallState(call.state)) {
    return { kind: "malformed" }
  }
  return { kind: "found", decision: matchedRaw as unknown as RequestedDecision }
}

/** Strictly fetch and validate one requested decision for approval reconciliation (fails closed on malformed 200). */
export async function fetchRequestedDecisionForReconciliation(
  decisionId: string,
  fetcher?: Fetch,
): Promise<RequestedDecisionLookup> {
  const result = await cellCall<unknown>(requestedPath(decisionId), {}, fetcher)
  if (result.kind !== "ok") return { kind: "malformed" }
  return parseRequestedDecisionLookup(result.value, decisionId)
}

/**
 * Reconcile a row's local `pending` approval outcome against a strictly validated
 * `GET /v1/decisions:requested?id=...` lookup:
 * - `malformed` / inconclusive read: preserve the `pending` lock safely.
 * - `rejected` | `expired` | `withdrawn` | `approved` with `call.state === "refused"`: unlock row
 *   and set `retireIdempotencyKey: true` so re-promoting the unchanged row mints a fresh key.
 */
export function reconcilePendingRowOutcome(
  outcome: Extract<RowActionOutcome, { readonly kind: "pending" }>,
  lookup: RequestedDecisionLookup,
): {
  readonly nextOutcome: RowActionOutcome
  readonly shouldRefreshWorkspace: boolean
  readonly retireIdempotencyKey: boolean
} {
  if (lookup.kind === "malformed") {
    return { nextOutcome: outcome, shouldRefreshWorkspace: false, retireIdempotencyKey: false }
  }
  if (lookup.kind === "absent") {
    return {
      nextOutcome: {
        kind: "stale",
        message: `Approval decision ${outcome.decisionId} is no longer active on the cell — refresh and re-submit if needed.`,
      },
      shouldRefreshWorkspace: true,
      retireIdempotencyKey: false,
    }
  }
  const { decision } = lookup
  if (decision.status === "pending") {
    return { nextOutcome: outcome, shouldRefreshWorkspace: false, retireIdempotencyKey: false }
  }
  if (decision.status === "rejected" || decision.status === "expired" || decision.status === "withdrawn") {
    return {
      nextOutcome: {
        kind: "refused",
        code: `decision.${decision.status}`,
        message: requestedWords(decision),
      },
      shouldRefreshWorkspace: false,
      retireIdempotencyKey: true,
    }
  }
  const callState = decision.call.state
  if (!isConsistentDecidedCallState(callState) || callState === undefined) {
    // Approved decisions must carry a valid, consistent call execution state; fail closed otherwise.
    return { nextOutcome: outcome, shouldRefreshWorkspace: false, retireIdempotencyKey: false }
  }
  if (callState.state === "refused") {
    return {
      nextOutcome: {
        kind: "refused",
        code: callState.code ?? "decision.call_refused",
        message: requestedWords(decision),
      },
      shouldRefreshWorkspace: false,
      retireIdempotencyKey: true,
    }
  }
  if (callState.state === "committed") {
    return { nextOutcome: outcome, shouldRefreshWorkspace: true, retireIdempotencyKey: false }
  }
  return {
    nextOutcome: {
      kind: "stale",
      message: `${requestedWords(decision)} Review and promote again to execute.`,
    },
    shouldRefreshWorkspace: true,
    retireIdempotencyKey: false,
  }
}

/**
 * Snapshot the operator's currently rendered eligible rows against the rendered artifacts,
 * resolved aliases, and local edits so batch promotion strictly records what the operator saw —
 * never silently substituting unrendered artifacts or aliases from a newer cache entry.
 */
export function snapshotPromotionQueue(
  eligibleRows: readonly CandidateRow[],
  renderedArtifacts: readonly StagedArtifact[],
  renderedAliases: ReadonlyMap<string, AliasResolution>,
  reviewEdits: Readonly<Record<string, string>>,
  lockedRowIds: ReadonlySet<string>,
): readonly QueuedPromotionItem[] {
  return eligibleRows.flatMap((row) => {
    if (lockedRowIds.has(row.id)) return []
    const artifact = renderedArtifacts.find((item) => item.id === row.artifactId)
    const resolved = resolveRowEntity(row, renderedAliases)
    if (!artifact || resolved.kind !== "resolved") return []
    const basisKey = rowBasisKey(row, artifact)
    return [{
      rowId: row.id,
      basisKey,
      artifactId: artifact.id,
      blobDigest: artifact.blobDigest,
      functionReceiptDigest: artifact.functionReceiptDigest,
      targetRecordId: resolved.targetId,
      effectiveAt: row.effectiveAt,
      observedAt: row.observedAt,
      values: extractEditableValues(row, reviewEdits, basisKey),
    }]
  })
}

/** Rows that must stay excluded from batch promotion (in-flight, queued, committed awaiting read, or pending approval). */
export function batchLockedRowIds(
  busyRowIds: ReadonlySet<string>,
  queuedRowIds: ReadonlySet<string>,
  rowOutcomes: Readonly<Record<string, RowActionOutcome>>,
): ReadonlySet<string> {
  const locked = new Set<string>(busyRowIds)
  for (const id of queuedRowIds) locked.add(id)
  for (const [rowId, outcome] of Object.entries(rowOutcomes)) {
    if (isOutcomeLocked(outcome)) {
      locked.add(rowId)
    }
  }
  return locked
}

export interface QueuedPromotionItem {
  readonly rowId: string
  readonly basisKey: string
  readonly artifactId: string
  readonly blobDigest: string
  readonly functionReceiptDigest: string
  readonly targetRecordId: string
  readonly effectiveAt: string
  readonly observedAt: string
  readonly values: Readonly<Record<string, string>>
}

export type LiveBasisCheck =
  | { readonly kind: "ready"; readonly row: CandidateRow; readonly artifact: StagedArtifact; readonly targetRecordId: string }
  | { readonly kind: "stale"; readonly reason: string }

/**
 * Re-validate a queued promotion item against the latest live batch read before dispatching it.
 * Rejects the item as `stale` if the batch changed, the row disappeared, is no longer unverified
 * and unquarantined/unwithheld, its resolved entity changed, or its `rowBasisKey` (artifact,
 * evidence digests, temporal coordinates, or canonical baseline field values) drifted after queuing.
 */
export function verifyQueuedPromotionBasis(
  workspace: ImportTriageWorkspace | undefined,
  requestBatchId: string,
  item: QueuedPromotionItem,
): LiveBasisCheck {
  if (!workspace || workspace.batchId !== requestBatchId) {
    return { kind: "stale", reason: "Active batch changed before this queued row was dispatched." }
  }
  const liveRow = workspace.rows.find((candidate) => candidate.id === item.rowId)
  if (!liveRow) {
    return { kind: "stale", reason: "Row is no longer present in the live batch." }
  }
  const status = effectiveRowStatus(liveRow)
  if (status !== "imported_unverified") {
    return {
      kind: "stale",
      reason: status === "verified_fact"
        ? "Row was already verified by the cell before dispatch."
        : "Row is no longer eligible (quarantined or withheld) before dispatch.",
    }
  }
  const resolvedAliases = new Map<string, AliasResolution>()
  for (const alias of workspace.aliases) {
    if (alias.resolution !== null) resolvedAliases.set(alias.id, alias.resolution)
  }
  const resolved = resolveRowEntity(liveRow, resolvedAliases)
  if (resolved.kind !== "resolved" || resolved.targetId !== item.targetRecordId) {
    return { kind: "stale", reason: "Row entity resolution changed after queuing — review and promote again." }
  }
  const liveArtifact = workspace.artifacts.find((artifact) => artifact.id === liveRow.artifactId)
  if (
    !liveArtifact
    || liveArtifact.id !== item.artifactId
    || liveArtifact.blobDigest !== item.blobDigest
    || liveArtifact.functionReceiptDigest !== item.functionReceiptDigest
    || liveRow.effectiveAt !== item.effectiveAt
    || liveRow.observedAt !== item.observedAt
    || rowBasisKey(liveRow, liveArtifact) !== item.basisKey
  ) {
    return { kind: "stale", reason: "Row evidence, capture digests, or baseline field values changed after queuing — review and promote again." }
  }
  return { kind: "ready", row: liveRow, artifact: liveArtifact, targetRecordId: resolved.targetId }
}

export interface BatchRunReport {
  readonly promoted: number
  readonly refused: number
  readonly pendingApproval: number
  readonly failed: number
  readonly skippedStale: number
  readonly aborted: boolean
  readonly keptQuarantined: number
}

/**
 * Execute a snapshotted promotion queue sequentially, stopping immediately before the next item if
 * the active `batchId` changed or `/imports` unmounted so stale queues never dispatch after navigation.
 */
export async function runBatchPromotionQueue<T>(
  requestBatchId: string,
  queue: readonly T[],
  getActiveBatchId: () => string | undefined,
  keptQuarantined: number,
  dispatchOne: (item: T) => Promise<"promoted" | "refused" | "pending" | "error" | "stale" | "skipped">,
  isMounted: () => boolean = () => true,
): Promise<BatchRunReport> {
  let promoted = 0
  let refused = 0
  let pendingApproval = 0
  let failed = 0
  let skippedStale = 0
  let aborted = false
  for (const item of queue) {
    if (!isMounted() || getActiveBatchId() !== requestBatchId) {
      aborted = true
      break
    }
    const outcome = await dispatchOne(item)
    if (outcome === "promoted") promoted += 1
    else if (outcome === "refused") refused += 1
    else if (outcome === "pending") pendingApproval += 1
    else if (outcome === "error") failed += 1
    else if (outcome === "stale") skippedStale += 1
  }
  return { promoted, refused, pendingApproval, failed, skippedStale, aborted, keptQuarantined }
}

// ---- Action result classification (5xx is retryable error, 4xx is domain refusal) --------------

export type ClassifiedActionResult =
  | { readonly kind: "receipt"; readonly receipt: ActionReceipt }
  | { readonly kind: "pending"; readonly decisionId: string; readonly expiresAtMs: number }
  | { readonly kind: "refused"; readonly code: string; readonly message: string }
  | { readonly kind: "error"; readonly message: string }

/**
 * Classify a governed `callAction` result honestly:
 * - HTTP 5xx (even with a structured JSON error body) and `unavailable` are retryable server errors.
 * - Only HTTP 4xx domain refusals (422 / 409 / 403 / 400) are non-retryable domain refusals.
 */
export function classifyTriageActionResult(result: CallAnswer): ClassifiedActionResult {
  if (result.kind === "receipt") return result
  if (result.kind === "pending") {
    return { kind: "pending", decisionId: result.decision.id, expiresAtMs: result.decision.expiresAtMs }
  }
  if (result.kind === "refused") {
    if (result.status >= 500) {
      return {
        kind: "error",
        message: `Server error (${result.code}): ${result.message}. Retry reuses the same idempotency key.`,
      }
    }
    return { kind: "refused", code: result.code, message: result.message }
  }
  return {
    kind: "error",
    message: "The cell could not evaluate this action right now. Retry reuses the same idempotency key.",
  }
}

// ---- Row, confidence, alias & supersession status helpers --------------------------------------

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
 * Authoritative status for a supersession conflict card.
 * - Unresolved or `keep_disputed`: strictly `quarantined` (open `FEED-CONFLICT`).
 * - Resolved: ONLY returns `verified_fact` when the cell explicitly attests `factStatus: "verified_fact"`;
 *   otherwise returns the cell's reported status (`imported_unverified` / `quarantined` / `withheld`)
 *   or `null` (displayed as a neutral "Resolved" chip rather than claiming "Verified fact").
 */
export function supersessionFactStatus(conflict: SupersessionConflict): FactStatus | null {
  if (conflict.resolution === null || conflict.resolution.choice === "keep_disputed") {
    return "quarantined"
  }
  const reported = conflict.resolution.factStatus ?? conflict.factStatus ?? null
  if (reported === null) return null
  return deriveFactStatus({
    quarantined: reported === "quarantined",
    withheld: reported === "withheld",
    unverifiedImport: reported === "imported_unverified",
    verified: reported === "verified_fact",
  })
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
 * When `basisKey` is provided, only local edits bound to that exact `(rowId, basisKey)` are applied.
 */
export function extractEditableValues(
  row: CandidateRow,
  edits: Readonly<Record<string, string>>,
  basisKey?: string,
): Readonly<Record<string, string>> {
  const values: Record<string, string> = {}
  for (const field of row.fields) {
    if (isFieldWithheld(field)) continue
    const editValue = basisKey !== undefined
      ? edits[fieldEditKey(row.id, basisKey, field.name)]
      : edits[`${row.id}:${field.name}`]
    values[field.name] = editValue ?? field.value
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

// ---- Strict, normalized, cell-scoped artifact media authorization ------------------------------

/**
 * Authorize how an artifact URI is loaded:
 * - `data-uri`: safe inline synthetic image data URI (`data:image/svg+xml|png|webp|jpeg`).
 * - `cell-media`: normalized cell-scoped path (`/v1/blobs/...` or `/v1/uploads/...`) fetched
 *   through Studio's scoped host transport into a revocable Blob URL (never raw unauthenticated `<img>`).
 * - `cell-scan`: object-store URI (`s3://`, `r2://`, `gs://`, `blob://`, `norn-blob://`) exchanged
 *   through the cell's policy-checked `POST /v1/uploads:read` endpoint.
 * - `rejected`: arbitrary external `http(s)://` URLs, protocol-relative URLs, or traversal paths.
 */
export type ArtifactMediaSource =
  | { readonly kind: "data-uri"; readonly url: string }
  | { readonly kind: "cell-media"; readonly path: string }
  | { readonly kind: "cell-scan"; readonly uri: string }
  | { readonly kind: "rejected"; readonly reason: string }

const SAFE_DATA_IMAGE = /^data:image\/(svg\+xml|png|webp|jpeg)(;charset=[^;,]+)?(;base64)?,/i
const OBJECT_STORE_URI = /^(s3|r2|gs|blob|norn-blob):\/\//i
const NORMALIZED_CELL_MEDIA = /^\/(?:api\/host\/)?(v1\/(?:blobs|uploads)\/[^/].*)$/

function hasPathTraversal(rawPath: string): boolean {
  const pathPart = rawPath.split(/[?#]/, 1)[0] ?? ""
  if (pathPart.includes("\\") || /%5c/i.test(pathPart) || /%2f/i.test(pathPart)) return true
  for (const segment of pathPart.split("/")) {
    let decoded = segment
    try { decoded = decodeURIComponent(segment) } catch { return true }
    if (decoded === "." || decoded === ".." || decoded.includes("/") || decoded.includes("\\")) return true
  }
  return false
}

export function resolveArtifactMediaSource(uri: string): ArtifactMediaSource {
  const trimmed = uri.trim()
  if (trimmed === "") {
    return { kind: "rejected", reason: "Artifact carries no media URI." }
  }
  if (SAFE_DATA_IMAGE.test(trimmed)) {
    return { kind: "data-uri", url: trimmed }
  }
  if (trimmed.startsWith("/") && !trimmed.startsWith("//")) {
    if (hasPathTraversal(trimmed)) {
      return { kind: "rejected", reason: "Artifact media path contains invalid traversal segments." }
    }
    let parsed: URL
    try {
      parsed = new URL(trimmed, "https://cell.invalid")
    } catch {
      return { kind: "rejected", reason: "Artifact media path is malformed." }
    }
    if (parsed.origin !== "https://cell.invalid") {
      return { kind: "rejected", reason: "External artifact URLs are blocked; media must come from a cell-authorized blob or upload read." }
    }
    const match = NORMALIZED_CELL_MEDIA.exec(parsed.pathname)
    if (match !== null) {
      return { kind: "cell-media", path: `/${match[1]}${parsed.search}` }
    }
    return { kind: "rejected", reason: "Artifact media path must target a cell blob or upload endpoint." }
  }
  if (OBJECT_STORE_URI.test(trimmed)) {
    return { kind: "cell-scan", uri: trimmed }
  }
  return {
    kind: "rejected",
    reason: "External artifact URLs are blocked; media must come from a cell-authorized blob or upload read.",
  }
}

/** Fetch a normalized cell-scoped media path through Studio's scoped host transport as a Blob. */
export async function fetchScopedArtifactBlob(path: string, fetcher: Fetch = (input, init) => fetch(input, init)): Promise<Blob> {
  const scope = await readStudioScope(fetcher)
  if (scope === null) throw new CellUnavailable(404, "No data cell scope is active for artifact media.")
  const call = scopedHostFetch(fetcher, "/api/host", async () => scope)
  const response = await call(path, { headers: { accept: "image/*,application/octet-stream" } })
  if (!response.ok) throw new CellUnavailable(response.status, `Artifact media read failed (${response.status}).`)
  return response.blob()
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
  idempotencyKey = createIntentKey(),
  fetcher?: Fetch,
  boundScope?: StudioScope | null,
): Promise<CallAnswer> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher, boundScope)
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
  idempotencyKey = createIntentKey(),
  fetcher?: Fetch,
  boundScope?: StudioScope | null,
): Promise<CallAnswer> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher, boundScope)
}

export function commitAliasIgnore(
  actionId: string,
  payload: {
    readonly batchId: string
    readonly aliasId: string
    readonly externalHandle: string
    readonly note?: string
  },
  idempotencyKey = createIntentKey(),
  fetcher?: Fetch,
  boundScope?: StudioScope | null,
): Promise<CallAnswer> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher, boundScope)
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
  idempotencyKey = createIntentKey(),
  fetcher?: Fetch,
  boundScope?: StudioScope | null,
): Promise<CallAnswer> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher, boundScope)
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
  idempotencyKey = createIntentKey(),
  fetcher?: Fetch,
  boundScope?: StudioScope | null,
): Promise<CallAnswer> {
  return callAction(actionId, JSON.stringify(payload), idempotencyKey, fetcher, boundScope)
}
