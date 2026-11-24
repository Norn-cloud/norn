import { scopePrefix, type CellCacheStorage, type StoredDocument } from "./cell-cache.ts"
import type { StudioScope } from "./definitions.ts"

/**
 * The persisted cell cache's storage: IndexedDB, one object store keyed by `cellCacheKey`. This is
 * the one module an OPFS-backed adapter replaces (see `CellCacheStorage`). Without IndexedDB (a
 * private window, a test runtime) nothing is kept and every read goes to the cell.
 */
const DATABASE = "norn-cell-cache"
const STORE = "documents"

const done = <T,>(request: IDBRequest<T>): Promise<T> =>
  new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result)
    request.onerror = () => reject(request.error)
  })

function open(factory: IDBFactory): Promise<IDBDatabase> {
  const request = factory.open(DATABASE, 1)
  request.onupgradeneeded = () => { request.result.createObjectStore(STORE) }
  return done(request)
}

/** The IndexedDB adapter, or a store that keeps nothing when the runtime has no IndexedDB. */
export function indexedDbStorage(factory: IDBFactory | undefined = globalThis.indexedDB): CellCacheStorage {
  if (factory === undefined) return noStorage
  let database: Promise<IDBDatabase> | null = null
  const store = async (mode: IDBTransactionMode) => (await (database ??= open(factory))).transaction(STORE, mode).objectStore(STORE)
  return {
    get: async (key) => (await done((await store("readonly")).get(key))) as StoredDocument | undefined,
    set: async (key, value) => { await done((await store("readwrite")).put(value, key)) },
    delete: async (key) => { await done((await store("readwrite")).delete(key)) },
    clear: async (scope?: StudioScope) => {
      const documents = await store("readwrite")
      if (scope === undefined) { await done(documents.clear()); return }
      const prefix = scopePrefix(scope)
      await done(documents.delete(IDBKeyRange.bound(prefix, `${prefix}￿`)))
    },
  }
}

/** Keeps nothing. */
export const noStorage: CellCacheStorage = {
  get: async () => undefined,
  set: async () => {},
  delete: async () => {},
  clear: async () => {},
}

let opened: CellCacheStorage | null = null

/** The storage Studio persists cell documents to. */
export const cellCacheStorage = (): CellCacheStorage => opened ??= indexedDbStorage()
