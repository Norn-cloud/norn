import { hashKey, type Query, type QueryClient, type QueryKey, type QueryPersister } from "@tanstack/react-query"
import { CellUnavailable, sameStudioScope, studioScopeKey, type StudioScope } from "./definitions.ts"

/**
 * The persisted cell cache (#1374): documents kept across reloads so a page can show its last
 * answer at once and read the cell again behind it.
 *
 * The server decides what may be kept. A document is stored only when the server marked it
 * storable (`Cache-Control` with a positive `max-age`, and neither `no-store` nor `no-cache`) and
 * it carries no marked or withheld value; anything else deletes what was kept for that read. No
 * cell route marks a document today, so nothing is persisted until the server says so (the
 * server-side cache-policy flag is the follow-up on #1374). A live push carries no mark, so a
 * pushed document also deletes the kept one rather than leaving it stale.
 *
 * Every key starts with its scope's prefix; a scope change forgets the previous scope's documents
 * and a sign-out forgets them all.
 */

/** One stored document: the server's text byte for byte, so its exact decimals survive. */
export interface StoredDocument {
  /** The route it was read from (`/v1/runs/{id}`). */
  readonly route: string
  readonly status: number
  readonly text: string
  readonly savedAt: number
  /** When the server's `max-age` ends; after it the document is never shown. */
  readonly expiresAt: number
}

/**
 * Where persisted documents live. This interface is the whole seam: an adapter is async
 * key → JSON-serializable value, with keys built by {@link cellCacheKey} so that
 * `clear(scope)` removes exactly the keys starting with {@link scopePrefix}`(scope)`, and
 * `clear()` removes every key. The IndexedDB adapter (`cell-cache-store.ts`) ships first; an
 * OPFS-backed one (Turso in the browser, or WellDB's wasm32 build) replaces it by changing only
 * that module.
 */
export interface CellCacheStorage {
  get(key: string): Promise<StoredDocument | undefined>
  set(key: string, value: StoredDocument): Promise<void>
  delete(key: string): Promise<void>
  /** Forget one scope's documents; with no scope, forget every document (sign-out). */
  clear(scope?: StudioScope): Promise<void>
}

/** The key prefix every document of `scope` is stored under. */
export const scopePrefix = (scope: StudioScope): string => `${studioScopeKey(scope)}\u0000`

/** The key of one read of `scope`. */
export const cellCacheKey = (scope: StudioScope, queryKey: QueryKey): string => `${scopePrefix(scope)}${hashKey(queryKey)}`

/** How long the server lets a client keep a document, from its `Cache-Control`; null when it may not be kept. */
export function storableFor(cacheControl: string | null): number | null {
  if (cacheControl === null) return null
  // Directive names compare without their arguments: `no-cache="Set-Cookie"` is still `no-cache`.
  const directives = cacheControl.toLowerCase().split(",").map((directive) => {
    const [name = "", argument] = directive.split("=", 2)
    return { name: name.trim(), argument: argument?.trim() }
  })
  if (directives.some(({ name }) => name === "no-store" || name === "no-cache")) return null
  const maxAge = directives.find(({ name }) => name === "max-age")?.argument
  const seconds = maxAge !== undefined && /^\d+$/.test(maxAge) ? Number(maxAge) : 0
  return seconds > 0 ? seconds * 1000 : null
}

const nonEmpty = (value: unknown): boolean =>
  Array.isArray(value) ? value.length > 0
    : typeof value === "number" ? value > 0
      : value !== null && typeof value === "object" ? Object.keys(value).length > 0
        : value === true

/**
 * Whether `value` carries a marked or withheld value anywhere, in the contract's own words
 * (`withheld`, `withheldProperties`, `withheldRows`, `masked`, `mask`, `markings`, `redaction`).
 * Such a document is never persisted, whatever its `Cache-Control` says.
 */
export function carriesProtectedValues(value: unknown): boolean {
  if (Array.isArray(value)) return value.some(carriesProtectedValues)
  if (value === null || typeof value !== "object") return false
  for (const [key, member] of Object.entries(value)) {
    if ((key === "withheld" || key === "withheldProperties" || key === "withheldRows" || key === "masked" || key === "mask" || key === "markings") && nonEmpty(member)) return true
    if (key === "redaction" && member !== null && member !== undefined) return true
    if (carriesProtectedValues(member)) return true
  }
  return false
}

/** A document as the cell query cache holds it; `text` is null for a pushed document. */
export interface PersistableDocument {
  readonly status: number
  readonly document: unknown
  readonly text: string | null
  readonly cacheControl: string | null
}

/** What may be stored for `doc`, or null when the server did not mark it storable or it carries protected values. */
export function storedFor(route: string, doc: PersistableDocument, now = Date.now()): StoredDocument | null {
  const maxAgeMs = storableFor(doc.cacheControl)
  if (maxAgeMs === null || doc.text === null || carriesProtectedValues(doc.document)) return null
  return { route, status: doc.status, text: doc.text, savedAt: now, expiresAt: now + maxAgeMs }
}

export interface CellPersistence {
  /** The TanStack Query persister a query opts into (`persister`): it shows the kept document while the cell is read again. */
  readonly persister: QueryPersister
  /** Keep or forget what `doc` allows for `queryKey` (a fetched or pushed document). */
  readonly write: (scope: StudioScope, queryKey: QueryKey, route: string, doc: PersistableDocument) => Promise<void>
  readonly storage: CellCacheStorage
}

/**
 * Whether a failed read says nothing about the document: the cell could not be reached, timed out
 * or failed (the kept copy may still be shown). Any other failure is the cell's answer (a refusal,
 * a route it does not serve), and the kept copy is forgotten.
 */
const transient = (error: unknown): boolean =>
  error instanceof CellUnavailable && (error.status === 0 || error.status >= 500 || error.timedOut === true)

/** The query key's scope: cell query keys are `[route, orgId, environmentId, params]`. */
function scopeOfKey(queryKey: QueryKey): StudioScope | null {
  const [route, orgId, environmentId] = queryKey
  return typeof route === "string" && typeof orgId === "string" && typeof environmentId === "string" ? { orgId, environmentId } : null
}

/**
 * The persister over `storage`: it shows a kept document while the cell is read again, then keeps
 * or forgets what the fresh answer allows. `restore` turns a stored document back into what the
 * query holds. A failing storage never fails a read (the cell is read as if nothing were kept).
 */
export function cellPersistence(storage: CellCacheStorage, restore: (stored: StoredDocument) => unknown, now: () => number = Date.now): CellPersistence {
  const write: CellPersistence["write"] = async (scope, queryKey, route, doc) => {
    const key = cellCacheKey(scope, queryKey)
    const stored = storedFor(route, doc, now())
    try {
      if (stored === null) await storage.delete(key)
      else await storage.set(key, stored)
    } catch {
      // A cache that cannot write keeps nothing; the read itself stands.
    }
  }
  const persister = (async (queryFn, context, query: Query) => {
    const scope = scopeOfKey(context.queryKey)
    // Only a read that never had an answer (a fresh page) shows a kept copy, and only when nothing
    // (a push, a refusal) answered while storage was read.
    const untouched = () => query.state.dataUpdateCount === 0 && query.state.errorUpdateCount === 0 && query.state.data === undefined && query.state.error == null
    if (scope !== null && untouched()) {
      const stored = await storage.get(cellCacheKey(scope, context.queryKey)).catch(() => undefined)
      if (stored !== undefined && stored.expiresAt > now() && untouched()) {
        try {
          query.setState({ data: restore(stored), dataUpdatedAt: stored.savedAt, status: "success", error: null })
        } catch {
          await storage.delete(cellCacheKey(scope, context.queryKey)).catch(() => undefined)
        }
      }
    }
    let result: unknown
    try {
      result = await queryFn(context)
    } catch (error) {
      if (scope !== null && !transient(error)) await storage.delete(cellCacheKey(scope, context.queryKey)).catch(() => undefined)
      throw error
    }
    const route = context.queryKey[0]
    if (scope !== null && typeof route === "string") await write(scope, context.queryKey, route, result as PersistableDocument)
    return result
  }) as QueryPersister
  return { persister, write, storage }
}

const watched = new WeakSet<QueryClient>()

/**
 * Forget kept documents when the reader or the scope changes: the previous scope's on a scope
 * change, every one on sign-out (the session's scope read forgotten or answered with none).
 * Installed once per QueryClient.
 */
export function forgetOnScopeChange(client: QueryClient, storage: CellCacheStorage): void {
  if (watched.has(client)) return
  watched.add(client)
  let last: StudioScope | null | undefined = client.getQueryData<StudioScope | null>(["studio-scope"])
  client.getQueryCache().subscribe((event) => {
    if (event.query.queryKey.length !== 1 || event.query.queryKey[0] !== "studio-scope") return
    if (event.type === "removed") {
      last = undefined
      void storage.clear().catch(() => undefined)
      return
    }
    if (event.type !== "updated" || event.action.type !== "success") return
    const next = event.query.state.data as StudioScope | null | undefined
    if (next === undefined) return
    if (next === null) void storage.clear().catch(() => undefined)
    else if (last != null && !sameStudioScope(last, next)) void storage.clear(last).catch(() => undefined)
    last = next
  })
}
