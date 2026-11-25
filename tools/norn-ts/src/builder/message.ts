/**
 * Source messages (UI-TEXT-1, #1762): user-facing text written once, in the source language, and
 * shaped as GT shapes it. `msg("Read again")` lowers to the text and a stable id, the hash of the
 * text and its optional context; a host draws it through its translations, keyed by that id, and
 * falls back to the source text. The checker refuses an id that is not this hash
 * (`norn_ir::message_id`), so an id is never hand-written or left stale.
 *
 * This module imports nothing: renderers (`@norn/react`) and hosts key their translations with the
 * same `messageId`.
 */

/** A source message as a Widget UX contract carries it. */
export interface Message {
  readonly msg: { readonly id: string; readonly source: string; readonly context?: string }
}

const OFFSET = 0xcbf29ce484222325n
const PRIME = 0x100000001b3n
const MASK = 0xffffffffffffffffn
const encoder = new TextEncoder()

/**
 * A message's stable id: `m` and the 64-bit FNV-1a hash, in lowercase hex, of the UTF-8 bytes of its
 * context (empty when it has none), U+0000, and its source text (`norn_ir::message_id`).
 */
export function messageId(source: string, context?: string): string {
  let hash = OFFSET
  const bytes = encoder.encode(`${context ?? ""}\u0000${source}`)
  for (const byte of bytes) hash = ((hash ^ BigInt(byte)) * PRIME) & MASK
  return `m${hash.toString(16).padStart(16, "0")}`
}

/** A source message: `msg("Retry")`, or `msg("Close", { context: "a dialog's close control" })`. */
export function msg(source: string, options: { readonly context?: string } = {}): Message {
  const context = options.context
  return { msg: { id: messageId(source, context), source, ...(context === undefined ? {} : { context }) } }
}

/** Whether `text` is a source message rather than locale-keyed text. */
export const isMessage = (text: unknown): text is Message =>
  typeof text === "object" && text !== null && "msg" in text && typeof (text as { msg: unknown }).msg === "object"
