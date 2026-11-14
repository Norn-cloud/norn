import { CellUnavailable, readStudioScope, scopedHostFetch } from "./definitions.ts"

/**
 * One scoped call to a cell route, read into the shapes every RUNS route shares: a document, a
 * Norn refusal (`{ code, message, diagnostics[].span.pointer, … }`), "not served" (a 404 or 503
 * without a Norn code), or a workspace without a cell. Studio decides nothing from these: it shows
 * them.
 */

export type Fetch = (input: string, init?: RequestInit) => Promise<Response>
export const defaultFetch: Fetch = (input, init) => fetch(input, init)

export interface Diagnostic { readonly code: string; readonly message: string; readonly pointer: string | null }

export interface Refusal {
  readonly status: number
  readonly code: string
  readonly message: string
  readonly diagnostics: readonly Diagnostic[]
  /** The whole refusal body, for route-specific members (a decision's `resolution`). */
  readonly body: Readonly<Record<string, unknown>>
}

export type CellResult<T> =
  | { readonly kind: "ok"; readonly status: number; readonly value: T; readonly etag: string | null }
  | { readonly kind: "not-modified" }
  | { readonly kind: "refused"; readonly refusal: Refusal }
  | { readonly kind: "unavailable" }
  | { readonly kind: "no-cell" }

function refusalOf(status: number, text: string): Refusal | null {
  let body: unknown
  try { body = JSON.parse(text) } catch { return null }
  if (body === null || typeof body !== "object") return null
  const record = body as Record<string, unknown>
  if (typeof record["code"] !== "string") return null
  const list = Array.isArray(record["diagnostics"]) ? record["diagnostics"] as readonly Record<string, unknown>[] : []
  const diagnostics = list.map((d) => {
    const span = d["span"] as { pointer?: unknown } | null | undefined
    // RUNS puts the pointer under `span`; a flat `pointer` is read too, so either shape lands on its field.
    const pointer = typeof span?.pointer === "string" ? span.pointer : typeof d["pointer"] === "string" ? d["pointer"] : null
    return { code: String(d["code"] ?? ""), message: String(d["message"] ?? ""), pointer }
  })
  const message = typeof record["message"] === "string" ? record["message"] : record["code"]
  return { status, code: record["code"], message, diagnostics, body: record }
}

/** A scoped cell call. A refusal with a Norn code is a refusal at any status, 404 included; a bare 404 or 503 is "not served". */
export async function cellCall<T>(path: string, init: RequestInit = {}, fetcher: Fetch = defaultFetch): Promise<CellResult<T>> {
  let scope
  try { scope = await readStudioScope(fetcher) } catch (error) { if (error instanceof CellUnavailable) return { kind: "unavailable" }; throw error }
  if (scope === null) return { kind: "no-cell" }
  const active = scope
  const call = scopedHostFetch(fetcher, "/api/host", async () => active)
  const response = await call(path, { ...init, headers: { accept: "application/json", ...init.headers } })
  if (response.status === 304) return { kind: "not-modified" }
  const text = await response.text()
  if (response.ok) {
    let value: T
    try { value = JSON.parse(text) as T } catch { throw new CellUnavailable(response.status, "The cell's answer was not a document.") }
    return { kind: "ok", status: response.status, value, etag: response.headers.get("etag") }
  }
  const refusal = refusalOf(response.status, text)
  if (refusal !== null) return { kind: "refused", refusal }
  if (response.status === 404 || response.status === 503) return { kind: "unavailable" }
  throw new CellUnavailable(response.status)
}

/** An id or name in words when the cell sends no title: `canon.restock_flow` → "Restock flow". */
export function humanize(id: string): string {
  const last = id.slice(id.lastIndexOf(".") + 1).replace(/([a-z])([A-Z])/g, "$1 $2").replaceAll(/[_-]+/g, " ").trim()
  return last.length === 0 ? id : last.charAt(0).toUpperCase() + last.slice(1).toLowerCase()
}
