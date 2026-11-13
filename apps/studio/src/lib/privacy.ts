import { cellCall, defaultFetch, type Fetch } from "./cell-read.ts"

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

/** Ask the cell to reveal one marked property, with an optional purpose kept in the receipt. */
export async function revealProperty(
  objectType: string, id: string, property: string, purpose: string,
  idempotencyKey: string = globalThis.crypto.randomUUID(), fetcher: Fetch = defaultFetch,
): Promise<RevealResult> {
  const body = { property, ...purpose.trim() === "" ? {} : { purpose: purpose.trim() }, idempotencyKey }
  const result = await cellCall<{ value: unknown; receipt: RevealReceipt }>(
    `/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}/reveal`,
    { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) },
    fetcher,
  )
  switch (result.kind) {
    case "ok": return { kind: "revealed", value: result.value.value, receipt: result.value.receipt }
    case "refused": return { kind: "refused", message: result.refusal.message }
    default: return { kind: "unavailable" }
  }
}

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
