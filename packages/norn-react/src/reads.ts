/**
 * How the renderer reads and calls: only through the workspace the server decided, and every answer
 * as a closed state (#1762).
 *
 * A page's data binding is read through its workspace's reader (`WidgetReader`, which Studio's
 * `ScopedReader` and `RecordedReader` implement): the reader is bound to the decided scope, and
 * answers each read already classified as one of the machine's call outcomes. An action binding is
 * called through `WidgetCalls`. The renderer turns those answers into `ReadState` and
 * `ActionCallState` (the closed unions of `state.ts`) and draws each with `matchReadState` /
 * `matchActionState`; nothing here judges permission.
 */
import type { Classified } from "./machine/outcome.ts"
import type { ActionCallState, ReadState, SentCall } from "./state.ts"
import type { DataBinding } from "./ux.ts"

/** One data binding's read, as the reader serves it. */
export type BindingRead =
  | { readonly kind: "object"; readonly objectType: string; readonly id: string }
  | { readonly kind: "list"; readonly objectType: string; readonly size: number }

/** One read as TanStack Query runs it: its key names the workspace it is read in. */
export interface WidgetReaderQuery {
  readonly queryKey: readonly unknown[]
  readonly queryFn: (context: { readonly signal: AbortSignal }) => Promise<Classified>
}

/** A decided workspace's reader, as the renderer reads a page's data bindings through it. */
export interface WidgetReader {
  readonly kind: "live" | "recorded"
  readonly widget: (read: BindingRead) => WidgetReaderQuery
}

/** Calls an action binding's action; resolves with the server's classified answer. */
export interface WidgetCalls {
  readonly call: (actionId: string, input: Readonly<Record<string, unknown>>, options: { readonly signal: AbortSignal; readonly idempotencyKey: string }) => Promise<Classified>
}

/** A live feed of one read (`norn.live.v1`): snapshots, a lapse, or the cell's refusal. */
export interface WidgetLive {
  readonly subscribe: (read: BindingRead, listener: {
    readonly snapshot: (document: unknown) => void
    readonly lapsed: () => void
    readonly refused: (refusal: unknown) => void
  }) => () => void
}

/** A refusal as the server sent it (`norn/refusal@2`): its code and category, and the document. */
export interface Refusal { readonly code: string; readonly category: string | null; readonly document: unknown }

export type BindingReadState = ReadState<unknown, Refusal>
/** A call's state; its receipt, decision and refusal are the server's documents as sent. */
export type BindingCallState = ActionCallState<unknown, never, unknown, { readonly refusal: Refusal }>

const record = (value: unknown): Record<string, unknown> | null => (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null)

/** The refusal an answer carries. */
export function refusalOf(answer: Classified): Refusal {
  const document = record(answer.body)
  return { code: typeof document?.code === "string" ? document.code : "", category: answer.category ?? null, document: answer.body }
}

/** The text a refusal or failure says, or a plain fallback. */
export function messageOf(body: unknown, fallback: string): string {
  const document = record(body)
  return typeof document?.message === "string" && document.message !== "" ? document.message : fallback
}

/**
 * The read a binding needs at this page, or null when the page cannot say it yet (a runtime key
 * the host did not supply). `objectTypeOf` resolves the binding's entity artifact to the object type
 * the cell serves, from the admitted definitions the host read.
 */
export function bindingRead(binding: DataBinding, context: Readonly<Record<string, unknown>>, objectTypeOf: (artifactId: string) => string | undefined): BindingRead | null {
  const objectType = objectTypeOf(binding.object.id)
  if (objectType === undefined) return null
  const query = binding.query
  if (query.kind === "object_list") return { kind: "list", objectType, size: query.page.size }
  if (query.kind === "object_runtime_detail") {
    const id = Object.hasOwn(context, query.parameterId) ? context[query.parameterId] : undefined
    return typeof id === "string" && id !== "" ? { kind: "object", objectType, id } : null
  }
  return null
}

/** The read state an answer to a read leaves, given the one before it and whether it is followed live. */
export function readStateOf(answer: Classified, before: BindingReadState | undefined, subscribed: boolean): BindingReadState {
  switch (answer.outcome) {
    case "accepted":
      return { kind: "ok", document: answer.body, freshness: subscribed ? "live" : "snapshot" }
    case "stale":
      return { kind: "ok", document: answer.body, freshness: "stale" }
    case "subscription_refused":
      return { kind: "ok", document: answer.body, freshness: "snapshot" }
    case "refused":
      return { kind: "refused", refusal: refusalOf(answer) }
    case "not_served":
      return { kind: "not_served", error: new Error(messageOf(answer.body, "The cell does not serve this read.")) }
    case "answer_moved":
      return { kind: "answer_moved", error: new Error(messageOf(answer.body, "The answer moved while it was read.")) }
    case "timeout":
      return { kind: "timed_out", error: new Error("No answer came in time.") }
    case "network_failure":
      return { kind: "unavailable", error: new Error(messageOf(answer.body, "The cell did not answer.")), retry: true }
    case "cancelled":
      return before !== undefined && before.kind !== "pending" ? before : { kind: "unavailable", error: new Error("The read was cancelled."), retry: true }
    case "pending_approval":
      // A read never waits for an approval; a host that answers one so is not answering the read.
      return { kind: "unavailable", error: new Error("The cell answered a read with an approval."), retry: true }
  }
}

/** The call state an answer to a call leaves. The call may have applied unless the cell said otherwise. */
export function callStateOf(answer: Classified, sent: SentCall): BindingCallState {
  switch (answer.outcome) {
    case "accepted":
      return { kind: "committed", receipt: answer.body, undo: null }
    case "pending_approval":
      return { kind: "pending_approval", decision: record(answer.body)?.decision ?? answer.body }
    case "refused":
      return { kind: "refused", refusal: refusalOf(answer) }
    case "not_served":
      return { kind: "not_served" }
    case "timeout":
      return { kind: "timed_out", sent, message: "No answer came in time; the call may have applied. Retry sends it again under the same key." }
    case "cancelled":
      return { kind: "timed_out", sent, message: "Cancelled before the cell answered; the call may have applied. Retry sends it again under the same key." }
    case "network_failure":
    case "answer_moved":
    case "stale":
    case "subscription_refused":
      return { kind: "timed_out", sent, message: messageOf(answer.body, "The cell's answer was lost; the call may have applied. Retry sends it again under the same key.") }
  }
}

/** Whether the machine follows the read of `bindingId` live (it has a subscription event for it). */
export const followed = (events: readonly { readonly source: { readonly kind: string; readonly bindingId?: string } }[], bindingId: string): boolean =>
  events.some((event) => event.source.kind === "subscription" && event.source.bindingId === bindingId)
