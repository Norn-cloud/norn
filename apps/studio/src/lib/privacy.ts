import { cellCall, defaultFetch, type Fetch } from "./cell-read.ts"
import { CELL_FETCH_TIMEOUT_MS, StudioScopeError, type StudioScope } from "./definitions.ts"

/**
 * PRIVACY-1/2. A marked property arrives already projected by the server: a pseudonym, `•••`,
 * null, a tail, or left out. The record's `masked` map says which, and whether this reader's
 * policy allows a reveal. Studio reads that map and never infers it; the stored value reaches the
 * browser only as the answer to a reveal, which the server records as a receipt first.
 */

export type MaskMode = "pseudonym" | "redacted" | "null" | "last-n" | "refuse"

export interface Masked {
  readonly mode: MaskMode
  readonly markings: readonly string[]
  readonly revealable: boolean
}

const MODES: readonly MaskMode[] = ["pseudonym", "redacted", "null", "last-n", "refuse"]

/** How the server projected `property` of `record` for this reader, or null when it is not marked. */
export function maskedOf(record: unknown, property: string): Masked | null {
  const masked = (record as { masked?: unknown } | null)?.masked
  if (masked === null || typeof masked !== "object") return null
  const entry = (masked as Record<string, unknown>)[property] as { mode?: unknown; markings?: unknown; revealable?: unknown } | undefined
  if (entry === undefined || entry === null || !MODES.includes(entry.mode as MaskMode)) return null
  const markings = Array.isArray(entry.markings) ? entry.markings.filter((m): m is string => typeof m === "string") : []
  return { mode: entry.mode as MaskMode, markings, revealable: entry.revealable === true }
}

/** The receipt the server committed before it answered a reveal. */
export interface RevealReceipt {
  readonly commitSeq: number
  readonly receiptHash: string
  readonly revealedAt: number
  readonly actor: string
  readonly property: string
  readonly purpose: string
  readonly markings: readonly string[]
}

export type RevealResult =
  | { readonly kind: "revealed"; readonly value: unknown; readonly receipt: RevealReceipt }
  | { readonly kind: "refused"; readonly message: string }
  | { readonly kind: "unavailable" }

/**
 * Ask the cell to reveal one marked property, with an optional purpose kept in the receipt. The
 * call goes to `boundScope` (the workspace the row was read from) with its expected-scope headers,
 * so the server refuses it if the session moved elsewhere. A transport failure or a cell that does
 * not answer in time is `unavailable`, never a thrown error: the caller can retry with the same key.
 */
export async function revealProperty(
  objectType: string, id: string, property: string, purpose: string,
  idempotencyKey: string = globalThis.crypto.randomUUID(), fetcher: Fetch = defaultFetch,
  boundScope?: StudioScope | null,
  timeoutMs = CELL_FETCH_TIMEOUT_MS,
): Promise<RevealResult> {
  const body = { property, ...purpose.trim() === "" ? {} : { purpose: purpose.trim() }, idempotencyKey }
  let result: Awaited<ReturnType<typeof cellCall<{ value: unknown; receipt: RevealReceipt }>>>
  try {
    result = await cellCall<{ value: unknown; receipt: RevealReceipt }>(
      `/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}/reveal`,
      { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body), signal: AbortSignal.timeout(timeoutMs) },
      fetcher,
      boundScope,
    )
  } catch (error) {
    if (error instanceof StudioScopeError) return { kind: "refused", message: error.message }
    return { kind: "unavailable" }
  }
  switch (result.kind) {
    case "ok": return { kind: "revealed", value: result.value.value, receipt: result.value.receipt }
    case "refused": return { kind: "refused", message: result.refusal.message }
    default: return { kind: "unavailable" }
  }
}

/**
 * The idempotency key of one reveal request. A retry of the same request (same purpose) after the
 * cell did not answer reuses it, so the cell records at most one receipt for it; a new purpose, or
 * a request the cell decided (revealed or refused), starts a new key.
 */
export function revealKeys(newKey: () => string = () => globalThis.crypto.randomUUID()) {
  let held: { readonly purpose: string; readonly key: string } | null = null
  return {
    for(purpose: string): string {
      const trimmed = purpose.trim()
      if (held === null || held.purpose !== trimmed) held = { purpose: trimmed, key: newKey() }
      return held.key
    },
    settle(): void { held = null },
  }
}

/**
 * The server-issued context a reveal is decided under: the record's version and the projection the
 * cell sent for the property (its mode, its markings and whether this reader may reveal it). A
 * revealed value, and an answer still on its way, belong to the context they were asked under: when
 * the cell sends another (a newer version, a grant revoked, a marking added) they are dropped. This
 * decides nothing; it only forgets what the cell's newer answer no longer covers.
 */
export const revealContext = (version: number | string, masked: Masked): string =>
  JSON.stringify([version, masked.mode, [...masked.markings].sort(), masked.revealable])

/** A value the cell revealed, with its receipt and the context it was revealed under. */
export interface Revealed {
  readonly value: unknown
  readonly receipt: RevealReceipt
  readonly context: string
}

/**
 * What to show for a marked property now: the revealed value only while the cell's current
 * projection still allows a reveal and is the one it was revealed under; otherwise nothing.
 */
export const shownReveal = (revealed: Revealed | null, masked: Masked, context: string): Revealed | null =>
  revealed !== null && masked.revealable && revealed.context === context ? revealed : null

/** What a mode says to the reader, for titles and the chip's label. */
export function maskHint(masked: Masked): string {
  const what = masked.markings.length > 0 ? masked.markings.join(", ") : "an undeclared marking"
  switch (masked.mode) {
    case "pseudonym": return `A stable pseudonym (${what}): equal values share it, so counts and joins still hold. The server never sent the value.`
    case "refuse": return `Withheld (${what}). The server never sent the value.`
    case "last-n": return `Only the last characters are shown (${what}). The server never sent the rest.`
    default: return `Masked (${what}). The server never sent the value.`
  }
}
