import { expect, test } from "bun:test"
import { QueryClient } from "@tanstack/react-query"
import { carriesProtectedValues, cellCacheKey, cellPersistence, forgetOnScopeChange, scopePrefix, storableFor, type CellCacheStorage, type StoredDocument } from "../src/lib/cell-cache.ts"
import { noStorage } from "../src/lib/cell-cache-store.ts"
import { attachLive, cellPath, cellQueryOptions, restoreDocument, type CellDocument, type LiveSource } from "../src/lib/cell-query.ts"
import type { StudioScope } from "../src/lib/definitions.ts"

const ROUTE = "/v1/objects/{type}/{id}"
const params = { path: { type: "demo.Item", id: "i1" } }
const scope = { orgId: "org_1", environmentId: "dev" }
const other = { orgId: "org_2", environmentId: "dev" }
const record = (version: number, extra = "") =>
  `{"objectType":"demo.Item","id":"i1","scope":"org_1/dev","version":${version},"values":{"amount":10.50}${extra},"withheld":false,"withheldProperties":[]}`

/** A storage adapter in memory, recording what it was asked to clear. */
function memory(): CellCacheStorage & { readonly entries: Map<string, StoredDocument>; readonly cleared: (StudioScope | "all")[] } {
  const entries = new Map<string, StoredDocument>()
  const cleared: (StudioScope | "all")[] = []
  return {
    entries,
    cleared,
    get: async (key) => entries.get(key),
    set: async (key, value) => { entries.set(key, value) },
    delete: async (key) => { entries.delete(key) },
    clear: async (at) => {
      cleared.push(at ?? "all")
      for (const key of [...entries.keys()]) if (at === undefined || key.startsWith(scopePrefix(at))) entries.delete(key)
    },
  }
}

async function read(storage: CellCacheStorage, body: string, cacheControl?: string, client = new QueryClient()) {
  const persistence = cellPersistence(storage, restoreDocument)
  const options = cellQueryOptions(client, ROUTE, params, {
    scope, persist: true, persistence,
    fetch: async () => new Response(body, { headers: cacheControl === undefined ? {} : { "cache-control": cacheControl } }),
  })
  await client.fetchQuery(options)
  return { client, options, persistence }
}

test("only what the server marks storable is kept: a positive max-age, without no-store or no-cache", () => {
  expect(storableFor(null)).toBeNull()
  expect(storableFor("no-store")).toBeNull()
  expect(storableFor("private, max-age=60, no-cache")).toBeNull()
  expect(storableFor("private, max-age=0")).toBeNull()
  expect(storableFor("private")).toBeNull()
  expect(storableFor("private, max-age=60")).toBe(60_000)
  expect(storableFor("Max-Age=5, Public")).toBe(5_000)
})

test("marked or withheld values are never kept, in the contract's own words", () => {
  expect(carriesProtectedValues(JSON.parse(record(1)))).toBe(false)
  expect(carriesProtectedValues({ withheld: true })).toBe(true)
  expect(carriesProtectedValues({ rows: [{ withheldProperties: ["secret"] }] })).toBe(true)
  expect(carriesProtectedValues({ masked: { amount: { mode: "redacted", markings: ["m"], revealable: false } } })).toBe(true)
  expect(carriesProtectedValues({ withheldRows: 2 })).toBe(true)
  expect(carriesProtectedValues({ redaction: "partial" })).toBe(true)
  expect(carriesProtectedValues({ masked: {}, markings: [], withheldRows: 0, redaction: null })).toBe(false)
})

test("the persister skips documents the server did not mark storable", async () => {
  const storage = memory()
  await read(storage, record(1))
  expect(storage.entries.size).toBe(0)
  await read(storage, record(1), "no-store")
  expect(storage.entries.size).toBe(0)
  await read(storage, record(1, ',"masked":{"amount":{"mode":"redacted","markings":["m"],"revealable":false}}'), "private, max-age=60")
  expect(storage.entries.size).toBe(0)
})

test("a storable document is kept byte for byte and shown at once on the next load, then read again", async () => {
  const storage = memory()
  const first = await read(storage, record(1), "private, max-age=60")
  const key = cellCacheKey(scope, first.options.queryKey)
  expect(key.startsWith(scopePrefix(scope))).toBe(true)
  expect(storage.entries.get(key)?.text).toBe(record(1))
  // The next page load: the kept document shows while the cell is read again.
  const client = new QueryClient()
  let release: (response: Response) => void = () => {}
  const options = cellQueryOptions(client, ROUTE, params, {
    scope, persist: true, persistence: cellPersistence(storage, restoreDocument),
    fetch: () => new Promise<Response>((resolve) => { release = resolve }),
  })
  const reading = client.fetchQuery(options)
  while (client.getQueryData(options.queryKey) === undefined) await Bun.sleep(1)
  const shown = client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)
  expect(shown?.document.version).toBe(1)
  expect(shown?.numbers.get("/values/amount")).toBe("10.50")
  release(new Response(record(2)))
  expect((await reading).document.version).toBe(2)
  // The fresh answer carried no mark: what was kept is forgotten.
  expect(storage.entries.size).toBe(0)
})

test("an expired document is never shown", async () => {
  const storage = memory()
  const { options } = await read(storage, record(1), "private, max-age=60")
  const key = cellCacheKey(scope, options.queryKey)
  const kept = storage.entries.get(key)
  if (kept === undefined) throw new Error("expected a kept document")
  storage.entries.set(key, { ...kept, expiresAt: Date.now() - 1 })
  const client = new QueryClient()
  const shown: unknown[] = []
  client.getQueryCache().subscribe((event) => { if (event.type === "updated") shown.push(event.query.state.data) })
  await read(storage, record(3), undefined, client)
  expect(shown.filter((data) => (data as CellDocument<{ version: number }> | undefined)?.document.version === 1)).toEqual([])
})

test("a pushed document carries no mark, so it forgets the kept one rather than leave it stale", async () => {
  const storage = memory()
  const { client, options, persistence } = await read(storage, record(1), "private, max-age=60")
  expect(storage.entries.size).toBe(1)
  let listener: Parameters<LiveSource["subscribe"]>[1] = () => {}
  const live: LiveSource = { subscribe: (_, next) => { listener = next; return () => {} } }
  attachLive({ client, live, scope, route: ROUTE, queryKey: options.queryKey, path: cellPath(ROUTE, scope, params), persistence })
  listener({ status: 200, digest: "d2", body: JSON.parse(record(2)), numbers: new Map([["/values/amount", "10.50"]]) })
  while (storage.entries.size > 0) await Bun.sleep(1)
  expect(client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)?.document.version).toBe(2)
})

test("a scope change forgets the previous scope's documents; a sign-out forgets them all", async () => {
  const storage = memory()
  const client = new QueryClient()
  forgetOnScopeChange(client, storage)
  forgetOnScopeChange(client, storage)
  client.setQueryData(["studio-scope"], scope)
  client.setQueryData(["studio-scope"], scope)
  expect(storage.cleared).toEqual([])
  client.setQueryData(["studio-scope"], other)
  expect(storage.cleared).toEqual([scope])
  client.setQueryData(["studio-scope"], null)
  expect(storage.cleared).toEqual([scope, "all"])
  client.setQueryData(["studio-scope"], scope)
  client.removeQueries({ queryKey: ["studio-scope"] })
  expect(storage.cleared).toEqual([scope, "all", "all"])
})

test("a runtime without IndexedDB keeps nothing", async () => {
  await noStorage.set("k", { route: ROUTE, status: 200, text: "{}", savedAt: 0, expiresAt: Number.MAX_SAFE_INTEGER })
  expect(await noStorage.get("k")).toBeUndefined()
})
