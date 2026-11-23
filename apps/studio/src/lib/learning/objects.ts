import { CellUnavailable, readStudioScope, scopedHostFetch } from "../definitions.ts"
import { recordsPath, rowWithheld, servedQuery, snapshotMoved, totalOf, type RecordsQuery, type Served, type Total } from "../records-query.ts"
import type { QueryDiagnostic } from "../records-source.ts"
import type { ObjectRecord } from "../transport.ts"

/**
 * Shared fail-closed decoding helpers and generic `/v1/objects/{type}` readers for the learning
 * loop views (#1220). Every read goes through Norn's generic object routes with the session's
 * explicit scope; no view-specific endpoint exists.
 */

export class DecodeError extends Error {}

export type JsonObj = Record<string, unknown>

export const obj = (v: unknown, at: string): JsonObj => {
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new DecodeError(`${at} is not an object`)
  return v as JsonObj
}

export const str = (v: unknown, at: string): string => {
  if (typeof v !== "string" || v === "") throw new DecodeError(`${at} is not text`)
  return v
}

export const text = (v: unknown, at: string): string | null =>
  v === null || v === undefined ? null : str(v, at)

export const int = (v: unknown, at: string): number => {
  if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0) throw new DecodeError(`${at} is not a whole number`)
  return v
}

export const bool = (v: unknown, at: string): boolean => {
  if (typeof v !== "boolean") throw new DecodeError(`${at} is not a boolean`)
  return v
}

export const list = <T,>(v: unknown, at: string, each: (x: unknown, at: string) => T): readonly T[] => {
  if (!Array.isArray(v)) throw new DecodeError(`${at} is not a list`)
  return v.map((x, i) => each(x, `${at}/${i}`))
}

export const oneOf = <T extends string>(v: unknown, at: string, values: readonly T[]): T => {
  if (!values.includes(v as T)) throw new DecodeError(`${at} is not one of ${values.join(", ")}`)
  return v as T
}

export interface ReceiptRef {
  readonly receiptHash: string
  readonly commitSeq: number
}

export const receipt = (o: JsonObj, at: string): ReceiptRef => ({
  receiptHash: str(o.receiptHash, `${at}/receiptHash`),
  commitSeq: int(o.commitSeq, `${at}/commitSeq`),
})

export type Fetch = (input: string, init?: RequestInit) => Promise<Response>
export const defaultFetch: Fetch = (input, init) => fetch(input, init)

export type DecodedRecord<T> =
  | { readonly kind: "ok"; readonly item: T; readonly record: ObjectRecord }
  | { readonly kind: "malformed"; readonly problem: string }

export type ObjectItemRead<T> =
  | { readonly kind: "ok"; readonly item: T; readonly record: ObjectRecord }
  | { readonly kind: "malformed"; readonly problem: string }
  /** 404 with a Norn code: absent, or not readable by this actor (the cell does not say which). */
  | { readonly kind: "not-found"; readonly message: string }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string }
  | { readonly kind: "unavailable" }
  | { readonly kind: "no-cell" }

export type ObjectListRow<T> =
  | { readonly kind: "ok"; readonly item: T; readonly record: ObjectRecord }
  | { readonly kind: "withheld"; readonly id: string }
  | { readonly kind: "malformed"; readonly id: string; readonly problem: string }

export type ObjectListRead<T> =
  | {
    readonly kind: "page"
    readonly rows: readonly ObjectListRow<T>[]
    readonly served: Served | null
    readonly next: string | undefined
    readonly total: Total | null
    readonly quarantineCount: number
  }
  | { readonly kind: "restart" }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string; readonly diagnostics: readonly QueryDiagnostic[] }
  | { readonly kind: "unavailable" }
  | { readonly kind: "no-cell" }

function diagnosticsOf(body: unknown): readonly QueryDiagnostic[] {
  const raw = (body as { diagnostics?: unknown } | null)?.diagnostics
  if (!Array.isArray(raw)) return []
  return raw.map((d: { code?: unknown; message?: unknown; pointer?: unknown; span?: { pointer?: unknown } | null }) => ({
    code: String(d.code ?? ""),
    message: String(d.message ?? ""),
    pointer: typeof d.span?.pointer === "string" ? d.span.pointer : typeof d.pointer === "string" ? d.pointer : null,
  }))
}

/** Read one record from `GET /v1/objects/{type}/{id}` and decode it fail-closed. */
export async function readLearningObject<T>(
  objectType: string,
  id: string,
  decode: (record: ObjectRecord) => DecodedRecord<T>,
  fetcher: Fetch = defaultFetch,
): Promise<ObjectItemRead<T>> {
  let scope
  try { scope = await readStudioScope(fetcher) } catch (error) { if (error instanceof CellUnavailable) return { kind: "unavailable" }; throw error }
  if (scope === null) return { kind: "no-cell" }
  const active = scope
  const response = await scopedHostFetch(fetcher, "/api/host", async () => active)(
    `/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}`,
    { headers: { accept: "application/json" } },
  )
  const raw = await response.text()
  let body: unknown = null
  try { body = JSON.parse(raw) } catch { /* decided by status below */ }
  const code = (body as { code?: unknown } | null)?.code
  const message = (body as { message?: unknown } | null)?.message
  if (response.ok) {
    if (body === null) throw new CellUnavailable(response.status, `The cell's ${objectType} record was not JSON.`)
    return decode(body as ObjectRecord)
  }
  if (typeof code === "string" && response.status !== 503) {
    const words = typeof message === "string" ? message : code
    return response.status === 404 ? { kind: "not-found", message: words } : { kind: "refused", status: response.status, code, message: words }
  }
  if (response.status === 404 || response.status === 503) return { kind: "unavailable" }
  throw new CellUnavailable(response.status)
}

/**
 * Read one page of `objectType` records from `GET /v1/objects/{type}` with server-side sort,
 * filter and cursor pagination. Withheld rows remain visible as withheld; malformed records are
 * flagged individually without guessing.
 */
export async function listLearningObjects<T>(
  objectType: string,
  query: RecordsQuery,
  decode: (record: ObjectRecord) => DecodedRecord<T>,
  fetcher: Fetch = defaultFetch,
): Promise<ObjectListRead<T>> {
  let scope
  try { scope = await readStudioScope(fetcher) } catch (error) { if (error instanceof CellUnavailable) return { kind: "unavailable" }; throw error }
  if (scope === null) return { kind: "no-cell" }
  const active = scope
  const response = await scopedHostFetch(fetcher, "/api/host", async () => active)(
    recordsPath(objectType, query),
    { headers: { accept: "application/json" } },
  )
  const raw = await response.text()
  let body: unknown = null
  try { body = JSON.parse(raw) } catch { /* decided by status below */ }
  const code = (body as { code?: unknown } | null)?.code
  if (response.ok) {
    if (body === null || typeof body !== "object" || !Array.isArray((body as { records?: unknown }).records)) {
      throw new CellUnavailable(response.status, `The cell's ${objectType} page was not a list of records.`)
    }
    const page = body as { records: readonly unknown[]; next?: unknown; scannedThrough?: unknown; quarantineCount?: unknown }
    const rows: ObjectListRow<T>[] = page.records.map((entry, index) => {
      const id = typeof (entry as { id?: unknown } | null)?.id === "string" ? (entry as { id: string }).id : `#${index + 1}`
      if (rowWithheld(entry)) return { kind: "withheld", id }
      const decoded = decode(entry as ObjectRecord)
      return decoded.kind === "ok"
        ? { kind: "ok", item: decoded.item, record: decoded.record }
        : { kind: "malformed", id, problem: decoded.problem }
    })
    const next = typeof page.next === "string" ? page.next : typeof page.scannedThrough === "string" ? page.scannedThrough : undefined
    const quarantineCount = typeof page.quarantineCount === "number" && Number.isSafeInteger(page.quarantineCount) && page.quarantineCount >= 0 ? page.quarantineCount : 0
    return { kind: "page", rows, served: servedQuery(page), next, total: totalOf(page), quarantineCount }
  }
  if (snapshotMoved(response.status, typeof code === "string" ? code : undefined)) return { kind: "restart" }
  if (response.status !== 404 && response.status !== 503 && typeof code === "string") {
    const message = (body as { message?: unknown }).message
    return { kind: "refused", status: response.status, code, message: typeof message === "string" ? message : code, diagnostics: diagnosticsOf(body) }
  }
  if (response.status === 404 || response.status === 503) {
    // A 404 with `object.type_unknown` or bare 404/503 means the learning pack isn't installed/served yet.
    return { kind: "unavailable" }
  }
  throw new CellUnavailable(response.status)
}
