import type { CallAnswer, CatalogRead } from "../actions.ts"
import type { ActionReceipt, ActionSummary, Diagnostic, PendingDecision, RefusalReason } from "../../../../../packages/norn-sdk/src/index.ts"
import type { ActionCallState, SentCall } from "../state/index.ts"

/**
 * The renderer core for one governed call, framework-neutral (#1220, L-0):
 *
 *   served documents ─decode→ State ─update(msg)→ [State, Cmd[]] ─view→ pixels
 *
 * `update` is pure: no clock, no randomness, no IO. Every value it needs (an idempotency key, the
 * encoded input, the cell's answer) arrives in a message, and the only things it can ask for are
 * the two commands below: call an action the server offered, or read the documents again. A view
 * (React today, possibly Foldkit or Exact later) dispatches messages and runs commands; it never
 * decides what may be called. The cell does.
 *
 * Idempotency keys, because the cell refuses a key reused with a different body (409
 * `action.idempotency_conflict`):
 * - While a call is out the form is locked: the view holds its fields read-only and shows "Sending…",
 *   and an edit message then changes nothing. No pending edit is tracked.
 * - A retry after a failure re-sends the original input under the same key, so a call whose answer
 *   was lost is never committed twice.
 * - After a failure the user may edit; that is a new call, under a new key.
 * - A call that parked for approval keeps its input and key (`sent`). Once the cell's own document
 *   says it is approved and offers to run it, `run-approved` sends exactly the call the cell serves
 *   back (APPROVAL-REQUESTED-1), under its key: the requester's retry. The cell decides it again.
 */

/** What the server offered to undo a committed call with: the action's declared compensation and its input. */
export interface UndoOffer { readonly actionId: string; readonly input: string }

/** The action this machine calls, as the catalog served it. */
export interface Target { readonly actionId: string; readonly risky: boolean }

/** The fields of the cell's refusal of a call, as the form shows them. */
export interface CallRefusal { readonly status: number; readonly code: string; readonly message: string; readonly diagnostics: readonly Diagnostic[]; readonly explanation?: string; readonly reason?: RefusalReason; readonly decidedAt?: string; readonly incidentId?: string }

/** One governed call's state (`lib/state`'s {@link ActionCallState}): a draft, a call out, its answer, its undo. */
export type Phase = ActionCallState<ActionReceipt, UndoOffer, PendingDecision, CallRefusal>

/** A call that never reached a deciding cell: a Retry sends it again, under its key. */
export type Unreached = Extract<Phase, { readonly kind: "not_served" | "unavailable" | "timed_out" }>
export const unreached = (phase: Phase): phase is Unreached => phase.kind === "not_served" || phase.kind === "unavailable" || phase.kind === "timed_out"

export interface State {
  readonly target: Target
  readonly phase: Phase
  /** The draft's idempotency key: replaced by an edit to the draft. While a call is out the form is locked and an edit changes nothing. */
  readonly key: string
  /** The call last sent, input and key, so a retry sends exactly that call again: never its body under a new key, or a new body under its key. */
  readonly sent: SentCall | null
}

export type Msg =
  /** The draft changed: a different call, so a different key. */
  | { readonly kind: "edit"; readonly key: string }
  | { readonly kind: "submit"; readonly input: string }
  | { readonly kind: "back" }
  | { readonly kind: "answered"; readonly result: CallAnswer }
  /** The call threw (network, a body the cell did not send): nothing is known to have committed. */
  | { readonly kind: "threw"; readonly message: string | null }
  | { readonly kind: "retry" }
  /**
   * Run the approved call, exactly as the cell's requested-call document serves it: the same action,
   * input and key the call parked with. Only from the pending phase of that decision. `next` is the
   * draft's new key: only the served call is ever sent under the parked key, so a draft sent after a
   * refusal is a new call.
   */
  | { readonly kind: "run-approved"; readonly decisionId: string; readonly actionId: string; readonly input: string; readonly key: string; readonly next: string }
  /** Start a fresh draft after a commit. */
  | { readonly kind: "again"; readonly key: string }
  | { readonly kind: "undo"; readonly key: string }

export type Cmd =
  | { readonly kind: "call"; readonly actionId: string; readonly input: string; readonly key: string }
  | { readonly kind: "refetch" }

export const init = (target: Target, key: string): State => ({ target, phase: { kind: "editing" }, key, sent: null })

/** A call that parked for approval, as it was sent: its decision, its encoded input and its key. */
export interface ParkedCall { readonly decision: PendingDecision; readonly input: string; readonly key: string }

/** A machine back on a parked call (the sheet reopened on it): pending, with the call it sent. */
export const resume = (target: Target, parked: ParkedCall): State =>
  ({ target, phase: { kind: "pending_approval", decision: parked.decision }, key: parked.key, sent: { input: parked.input, key: parked.key } })

/** The parked call a state holds, to keep while its decision is open; null in any other phase. */
export const parkedOf = (state: State): ParkedCall | null =>
  state.phase.kind === "pending_approval" && state.sent !== null ? { decision: state.phase.decision, input: state.sent.input, key: state.sent.key } : null

const call = (actionId: string, input: string, key: string): Cmd => ({ kind: "call", actionId, input, key })
const REFETCH: Cmd = { kind: "refetch" }

/** Whether a call is on its way: no second call starts until it answers. */
export const inFlight = (state: State) => state.phase.kind === "sending" || state.phase.kind === "undoing"

export function update(state: State, msg: Msg): readonly [State, readonly Cmd[]] {
  const { phase } = state
  switch (msg.kind) {
    case "edit": {
      // The view holds the fields still while a call is out (and the draft is gone after a commit), so an
      // edit then is not one: the call that is out keeps its body and key, and nothing unsent is lost.
      if (inFlight(state) || phase.kind === "committed" || phase.kind === "pending_approval" || phase.kind === "undo_failed" || phase.kind === "undone") return [state, []]
      // A refusal stays visible while the user fixes the fields it names; a confirm or an outage clears.
      const next: Phase = phase.kind === "confirming" || unreached(phase) ? { kind: "editing" } : phase
      return [{ ...state, phase: next, key: msg.key }, []]
    }
    case "submit": {
      if (inFlight(state) || phase.kind === "committed" || phase.kind === "pending_approval" || phase.kind === "undoing" || phase.kind === "undo_failed" || phase.kind === "undone") return [state, []]
      if (state.target.risky && phase.kind !== "confirming") return [{ ...state, phase: { kind: "confirming" } }, []]
      return [{ ...state, phase: { kind: "sending" }, sent: { input: msg.input, key: state.key } }, [call(state.target.actionId, msg.input, state.key)]]
    }
    case "back":
      return phase.kind === "confirming" ? [{ ...state, phase: { kind: "editing" } }, []] : [state, []]
    case "retry":
      if (unreached(phase) && state.sent !== null) return [{ ...state, phase: { kind: "sending" } }, [call(state.target.actionId, state.sent.input, state.sent.key)]]
      // Undo keeps its own key across a retry, like any call.
      if (phase.kind === "undo_failed") return [{ ...state, phase: { kind: "undoing", receipt: phase.receipt, undo: phase.undo } }, [call(phase.undo.actionId, phase.undo.input, state.key)]]
      return [state, []]
    case "run-approved":
      // Only the decision this machine waits on, only its own action: never another call's key.
      if (phase.kind !== "pending_approval" || phase.decision.id !== msg.decisionId || msg.actionId !== state.target.actionId) return [state, []]
      return [{ ...state, phase: { kind: "sending" }, key: msg.next, sent: { input: msg.input, key: msg.key } }, [call(state.target.actionId, msg.input, msg.key)]]
    case "again":
      // A new draft is a new call; an undo left unconfirmed keeps its own Retry until then.
      if (phase.kind !== "committed" && phase.kind !== "undone" && phase.kind !== "undo_failed" && phase.kind !== "pending_approval") return [state, []]
      return [{ ...state, phase: { kind: "editing" }, key: msg.key, sent: null }, []]
    case "undo":
      if (phase.kind !== "committed" || phase.undo === null) return [state, []]
      return [{ ...state, phase: { kind: "undoing", receipt: phase.receipt, undo: phase.undo }, key: msg.key }, [call(phase.undo.actionId, phase.undo.input, msg.key)]]
    case "threw":
      if (phase.kind === "sending") return [{ ...state, phase: { kind: "timed_out", sent: sentOf(state), message: msg.message } }, []]
      if (phase.kind === "undoing") return [{ ...state, phase: { kind: "undo_failed", receipt: phase.receipt, undo: phase.undo, message: msg.message, known: false } }, []]
      return [state, []]
    case "answered":
      if (phase.kind === "sending") return [{ ...state, phase: answered(msg.result, sentOf(state)) }, msg.result.kind === "receipt" ? [REFETCH] : []]
      if (phase.kind === "undoing") {
        const result = msg.result
        if (result.kind === "receipt") return [{ ...state, phase: { kind: "undone", receipt: phase.receipt, reversal: result.receipt } }, [REFETCH]]
        const message = result.kind === "refused" ? result.message : result.kind === "pending" ? "The undo was sent for approval; it runs once a decider approves it." : null
        return [{ ...state, phase: { kind: "undo_failed", receipt: phase.receipt, undo: phase.undo, message, known: true } }, []]
      }
      // An answer nobody is waiting for (a late reply after the state moved on) changes nothing.
      return [state, []]
  }
}

/** The call a sending machine sent: `sent` is set by every message that starts a call. */
const sentOf = (state: State): SentCall => state.sent ?? { input: "", key: state.key }

function answered(result: CallAnswer, sent: SentCall): Phase {
  switch (result.kind) {
    case "receipt": return { kind: "committed", receipt: result.receipt, undo: undoOf(result.receipt) }
    case "pending": return { kind: "pending_approval", decision: result.decision }
    case "refused": return {
      kind: "refused",
      status: result.status,
      code: result.code,
      message: result.message,
      diagnostics: result.diagnostics,
      ...result.explanation === undefined ? {} : { explanation: result.explanation },
      ...result.reason === undefined ? {} : { reason: result.reason },
      ...result.decidedAt === undefined ? {} : { decidedAt: result.decidedAt },
      ...result.incidentId === undefined ? {} : { incidentId: result.incidentId },
    }
    case "no-cell": return { kind: "not_served" }
    case "unavailable": return { kind: "unavailable", sent }
  }
}

// ---- Decoding what the server offers --------------------------------------------------------

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)

/** Every number is a safe integer, so re-encoding the parsed input sends exactly what the cell sent. */
function exact(value: unknown): boolean {
  if (typeof value === "number") return Number.isSafeInteger(value)
  if (Array.isArray(value)) return value.every(exact)
  if (isRecord(value)) return Object.values(value).every(exact)
  return true
}

/**
 * PROVISIONAL CONTRACT (#1208, PR #1631 S2): the undo the receipt names (`undo: { actionId, input }`,
 * from the action's declared compensation), or null. Studio never works out an inverse itself; the
 * field shape remains provisional until `norn-api` emits compensation offers and pins them with
 * route-produced fixtures. An offer whose input it cannot send back exactly (a number JSON parsing
 * may have rounded) is not offered.
 */
export function undoOf(receipt: ActionReceipt): UndoOffer | null {
  const undo = (receipt as unknown as { readonly undo?: unknown }).undo
  if (!isRecord(undo) || typeof undo.actionId !== "string" || undo.actionId === "" || !isRecord(undo.input) || !exact(undo.input)) return null
  return { actionId: undo.actionId, input: JSON.stringify(undo.input) }
}

/** The state of a live record's narrowed catalog read, as its query reports it. */
export interface NarrowedRead {
  readonly data: CatalogRead | undefined
  /** A read is in flight: the first one, or a refetch over cached data. */
  readonly isFetching: boolean
  /** The latest read threw; `data` may still hold an earlier answer. */
  readonly isError: boolean
}

/** What one record's drawer offers: the catalog, a read still running, or a read that failed. */
export type RecordOffer =
  | { readonly kind: "catalog"; readonly items: readonly ActionSummary[] }
  | { readonly kind: "reading" }
  | { readonly kind: "unreadable" }

/**
 * The catalog to offer on one record: for a live record, only the cell's catalog narrowed to the
 * record's state (ACTION-CATALOG-STATE-1), and only while that answer is the current one. A read in
 * flight (the first, or a refetch over a cached answer the record's state may have outrun) offers
 * nothing yet; a read that failed offers nothing, even with an earlier answer cached. So a
 * type-level action, or one the record's state no longer takes, is never shown. A recorded record
 * has no state to narrow by and keeps the type's catalog.
 */
export function recordCatalog(live: boolean, narrowed: NarrowedRead, typeLevel: readonly ActionSummary[]): RecordOffer {
  if (!live) return { kind: "catalog", items: typeLevel }
  if (narrowed.isFetching) return { kind: "reading" }
  if (narrowed.isError) return { kind: "unreadable" }
  if (narrowed.data === undefined) return { kind: "reading" }
  return narrowed.data.kind === "catalog" ? { kind: "catalog", items: narrowed.data.items } : { kind: "unreadable" }
}

/**
 * The actions to show on one record: the ids the record's own `offers` lists (what this actor may run
 * on it in its current state), in the catalog's order, when the cell serves that list; otherwise the
 * whole catalog for the type, and the cell decides at call time. Studio never narrows it on its own.
 */
export function offeredFor(catalog: readonly ActionSummary[], record: unknown): readonly ActionSummary[] {
  const offers = isRecord(record) ? record.offers : undefined
  if (!Array.isArray(offers)) return catalog
  const ids = new Set(offers.filter((id): id is string => typeof id === "string"))
  return catalog.filter((action) => ids.has(action.actionId))
}
