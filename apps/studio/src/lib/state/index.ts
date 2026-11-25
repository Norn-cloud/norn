/**
 * Studio's states as closed unions (#1762, Milestone A): every state a workspace, a read and a governed
 * call can be in, each a plain discriminated union on `kind` with its data and no flag that changes
 * what a state means, so they can later be generated from the backend's definitions. Each union has an
 * exhaustive matcher: a page that leaves a state out does not compile (`test/state-types.test.ts`).
 *
 * This module imports no app code (`test/workspace-reads-lint.test.ts`): what a state carries (a
 * scope, a reader, a document, a receipt) is a type parameter, so it can move to its own package as is.
 */

/** A state no union member describes: unreachable when the matcher's arms are exhaustive. */
function unknownState(state: never): never {
  throw new Error(`Unknown state: ${JSON.stringify(state)}`)
}

// ---- The workspace ------------------------------------------------------------------------------

/** The workspace a page reads in, as the server's mode answer decided it. */
export type WorkspaceState<Scope, Reader, Recorded> =
  /** The mode is being read: nothing is read, and nothing cached is shown. */
  | { readonly kind: "deciding" }
  /** The mode read failed (including a refetch after an earlier answer): Workspace unavailable, with Retry. */
  | { readonly kind: "unavailable"; readonly error: Error }
  /** No cell is connected: the recording, read only through its own reader. */
  | { readonly kind: "recorded"; readonly reader: Recorded }
  /** A cell answered at `scope`: every live read goes through `reader`, bound to that scope. */
  | { readonly kind: "live"; readonly scope: Scope; readonly reader: Reader }

/** One arm per workspace state, each required. */
export interface WorkspaceArms<Scope, Reader, Recorded, R> {
  readonly deciding: (state: { readonly kind: "deciding" }) => R
  readonly unavailable: (state: { readonly kind: "unavailable"; readonly error: Error }) => R
  readonly recorded: (state: { readonly kind: "recorded"; readonly reader: Recorded }) => R
  readonly live: (state: { readonly kind: "live"; readonly scope: Scope; readonly reader: Reader }) => R
}

export function matchWorkspaceState<Scope, Reader, Recorded, R>(state: WorkspaceState<Scope, Reader, Recorded>, arms: WorkspaceArms<Scope, Reader, Recorded, R>): R {
  switch (state.kind) {
    case "deciding": return arms.deciding(state)
    case "unavailable": return arms.unavailable(state)
    case "recorded": return arms.recorded(state)
    case "live": return arms.live(state)
    default: return unknownState(state)
  }
}

// ---- One read -----------------------------------------------------------------------------------

/**
 * How current a document is: `live` while the cell's answers to it are followed, `snapshot` when it was
 * read once and nothing follows it, `stale` when they were followed and no longer are (the page says so
 * and offers to read again).
 */
export type Freshness = "live" | "snapshot" | "stale"

/** One read of a document, as a page shows it. `Refusal` is the shape of the cell's refusal. */
export type ReadState<T, Refusal = Error> =
  | { readonly kind: "pending" }
  | { readonly kind: "ok"; readonly document: T; readonly freshness: Freshness }
  /** The cell answered with a refusal: asking again gets the same answer. */
  | { readonly kind: "refused"; readonly refusal: Refusal }
  /** The cell (or the deployment) does not serve this read: no route, or no host bound. */
  | { readonly kind: "not_served"; readonly error: Error }
  /** The cell did not answer. `retry`: asking again can change that (a gateway's 503, no answer at all). */
  | { readonly kind: "unavailable"; readonly error: Error; readonly retry: boolean }
  /** The data moved under the read (a cursor's snapshot): read it again from the start. */
  | { readonly kind: "answer_moved"; readonly error: Error }
  /** No answer came before the read's deadline: Retry asks again. */
  | { readonly kind: "timed_out"; readonly error: Error }

/** One arm per read state, each required. */
export interface ReadArms<T, Refusal, R> {
  readonly pending: (state: { readonly kind: "pending" }) => R
  readonly ok: (state: { readonly kind: "ok"; readonly document: T; readonly freshness: Freshness }) => R
  readonly refused: (state: { readonly kind: "refused"; readonly refusal: Refusal }) => R
  readonly not_served: (state: { readonly kind: "not_served"; readonly error: Error }) => R
  readonly unavailable: (state: { readonly kind: "unavailable"; readonly error: Error; readonly retry: boolean }) => R
  readonly answer_moved: (state: { readonly kind: "answer_moved"; readonly error: Error }) => R
  readonly timed_out: (state: { readonly kind: "timed_out"; readonly error: Error }) => R
}

export function matchReadState<T, Refusal, R>(state: ReadState<T, Refusal>, arms: ReadArms<T, Refusal, R>): R {
  switch (state.kind) {
    case "pending": return arms.pending(state)
    case "ok": return arms.ok(state)
    case "refused": return arms.refused(state)
    case "not_served": return arms.not_served(state)
    case "unavailable": return arms.unavailable(state)
    case "answer_moved": return arms.answer_moved(state)
    case "timed_out": return arms.timed_out(state)
    default: return unknownState(state)
  }
}

// ---- One governed call --------------------------------------------------------------------------

/** The call last sent, its encoded input and idempotency key: a retry sends exactly this again. */
export interface SentCall { readonly input: string; readonly key: string }

/**
 * One governed call, from its draft to its receipt and its undo. `Out` is the receipt, `Undo` what the
 * server offered to undo it with, `Decision` the approval it waits on, and `Refusal` the cell's
 * refusal, whose fields the `refused` state carries as they are.
 */
export type ActionCallState<Out, Undo = never, Decision = unknown, Refusal extends object = object> =
  | { readonly kind: "editing" }
  /** A risky call asks first. */
  | { readonly kind: "confirming" }
  /** The call `sent` is out: the form is locked until it answers. */
  | { readonly kind: "sending"; readonly sent: SentCall }
  /** Sent for approval: nothing committed; the decision waits in the inbox. */
  | { readonly kind: "pending_approval"; readonly decision: Decision }
  | { readonly kind: "committed"; readonly receipt: Out; readonly undo: Undo | null }
  | ({ readonly kind: "refused" } & Refusal)
  /** The workspace serves no cell to call: nothing was sent. */
  | { readonly kind: "not_served" }
  /** The cell answered without applying the call: nothing committed. Retry sends `sent` again. */
  | { readonly kind: "unavailable"; readonly sent: SentCall }
  /** The answer was lost (no answer, or one the cell did not send): the call may have committed. Retry sends `sent` again, under its key, so it never applies twice. */
  | { readonly kind: "timed_out"; readonly sent: SentCall; readonly message: string | null }
  | { readonly kind: "undoing"; readonly receipt: Out; readonly undo: Undo }
  /**
   * The undo did not come back with a receipt. `known`: the cell answered without committing (a refusal,
   * or no route), so nothing was undone. Otherwise the answer was lost and the undo may have committed:
   * a retry re-sends it under the same key, so it never applies twice.
   */
  | { readonly kind: "undo_failed"; readonly receipt: Out; readonly undo: Undo; readonly message: string | null; readonly known: boolean }
  | { readonly kind: "undone"; readonly receipt: Out; readonly reversal: Out }

/** One arm per call state, each required. */
export interface ActionArms<Out, Undo, Decision, Refusal extends object, R> {
  readonly editing: (state: { readonly kind: "editing" }) => R
  readonly confirming: (state: { readonly kind: "confirming" }) => R
  readonly sending: (state: { readonly kind: "sending"; readonly sent: SentCall }) => R
  readonly pending_approval: (state: { readonly kind: "pending_approval"; readonly decision: Decision }) => R
  readonly committed: (state: { readonly kind: "committed"; readonly receipt: Out; readonly undo: Undo | null }) => R
  readonly refused: (state: { readonly kind: "refused" } & Refusal) => R
  readonly not_served: (state: { readonly kind: "not_served" }) => R
  readonly unavailable: (state: { readonly kind: "unavailable"; readonly sent: SentCall }) => R
  readonly timed_out: (state: { readonly kind: "timed_out"; readonly sent: SentCall; readonly message: string | null }) => R
  readonly undoing: (state: { readonly kind: "undoing"; readonly receipt: Out; readonly undo: Undo }) => R
  readonly undo_failed: (state: { readonly kind: "undo_failed"; readonly receipt: Out; readonly undo: Undo; readonly message: string | null; readonly known: boolean }) => R
  readonly undone: (state: { readonly kind: "undone"; readonly receipt: Out; readonly reversal: Out }) => R
}

export function matchActionState<Out, Undo, Decision, Refusal extends object, R>(state: ActionCallState<Out, Undo, Decision, Refusal>, arms: ActionArms<Out, Undo, Decision, Refusal, R>): R {
  switch (state.kind) {
    case "editing": return arms.editing(state)
    case "confirming": return arms.confirming(state)
    case "sending": return arms.sending(state)
    case "pending_approval": return arms.pending_approval(state)
    case "committed": return arms.committed(state)
    case "refused": return arms.refused(state)
    case "not_served": return arms.not_served(state)
    case "unavailable": return arms.unavailable(state)
    case "timed_out": return arms.timed_out(state)
    case "undoing": return arms.undoing(state)
    case "undo_failed": return arms.undo_failed(state)
    case "undone": return arms.undone(state)
    default: return unknownState(state)
  }
}
