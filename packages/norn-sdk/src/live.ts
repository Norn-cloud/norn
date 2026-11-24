/**
 * The live-read protocol `norn.live.v1` (UI-LIVE-1), typed from the contract
 * (`LiveClientMessage`, `LiveServerMessage`, generated from
 * `crates/norn-api/src/live.rs`). A client subscribes to the cell's GET routes
 * on one WebSocket; the cell answers each like the GET, as the subscriber, and
 * again whenever a commit changes the answer.
 */
import type { components } from "./generated/norn-api.ts"
import { numbersUnder, numberTexts } from "./exact.ts"

export type LiveClientMessage = components["schemas"]["LiveClientMessage"]
export type LiveServerMessage = components["schemas"]["LiveServerMessage"]
export type LiveSnapshotMessage = Extract<LiveServerMessage, { readonly type: "snapshot" }>
export type LiveRefusedMessage = Extract<LiveServerMessage, { readonly type: "refused" }>

/** The WebSocket subprotocol. */
export const LIVE_PROTOCOL = "norn.live.v1"
/** The largest client frame, in UTF-8 bytes; the cell closes the socket (1009) on a larger one. */
export const LIVE_MAX_MESSAGE = 4096
/** Subscriptions one socket may hold. */
export const LIVE_MAX_SUBSCRIPTIONS = 8
/** The codes a `refused` frame carries. */
export const LIVE_REFUSALS = {
  invalidId: "live.invalid_id",
  notSubscribable: "live.not_subscribable",
  tooManySubscriptions: "live.too_many_subscriptions",
  unknownMessage: "live.unknown_message",
} as const

/** One frame from the cell, with the exact text of every number in a snapshot's body. */
export type LiveFrame =
  | { readonly message: LiveSnapshotMessage; readonly numbers: ReadonlyMap<string, string> }
  | { readonly message: LiveRefusedMessage }

/** The frame `text` holds, or null when it is no `norn.live.v1` frame. */
export function parseLiveFrame(text: string): LiveFrame | null {
  let value: unknown
  try { value = JSON.parse(text) } catch { return null }
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null
  const frame = value as Record<string, unknown>
  if (frame["type"] === "refused") {
    if (typeof frame["code"] !== "string" || typeof frame["message"] !== "string") return null
    const id = typeof frame["id"] === "string" ? frame["id"] : null
    return { message: { type: "refused", id, code: frame["code"], message: frame["message"] } }
  }
  if (frame["type"] !== "snapshot" || typeof frame["id"] !== "string" || typeof frame["status"] !== "number" || typeof frame["digest"] !== "string") return null
  const message: LiveSnapshotMessage = { type: "snapshot", id: frame["id"], status: frame["status"], digest: frame["digest"], body: frame["body"] ?? null }
  return { message, numbers: numbersUnder(numberTexts(text), "/body") }
}

/** The wire text of a client frame. */
export const encodeLiveMessage = (message: LiveClientMessage): string => JSON.stringify(message)
