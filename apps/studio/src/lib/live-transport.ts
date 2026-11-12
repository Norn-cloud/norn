/**
 * Studio over a connected cell: which documents the pages read (the cell's, or
 * the recording when no cell is connected) and the live reads themselves. Kept
 * apart from transport.ts, which finds recordings with Vite's import.meta.glob.
 */
import { CellUnavailable, readStudioScope, scopedHostFetch, type StudioScope } from "./definitions.ts"
import { readDocument, type Read } from "./exact.ts"
import { readsOf } from "./objects.ts"
import type { Artifact, Envelope, LinkPage, ObjectPage, ObjectRecord, SourceLineage, Transport } from "./transport.ts"

/** A read this cell does not serve. While a cell is connected nothing recorded stands in for it. */
export class NotServed extends Error {
  constructor(what: string) { super(`${what} not served by this cell yet.`) }
}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/** The documents under `prefix` (an RFC 6901 pointer) of one read, with their exact numbers. */
export function subRead<T>(read: Read<unknown>, prefix: string, document: T): Read<T> {
  const numbers = new Map<string, string>()
  for (const [pointer, text] of read.numbers) if (pointer === prefix || pointer.startsWith(`${prefix}/`)) numbers.set(pointer.slice(prefix.length), text)
  return { document, numbers, origin: read.origin }
}

/** The cell's link page (`next` cursor) in the shape the pages read (`hasMore`). */
function linkPage(read: Read<{ readonly objectType: string; readonly id: string; readonly items: LinkPage["items"]; readonly next?: string | null }>): Read<LinkPage> {
  const { objectType, id, items, next } = read.document
  return { ...read, document: { objectType, id, items, hasMore: typeof next === "string" } }
}

/** Where Studio's documents come from: the connected cell, or the recording when no cell is connected. */
export type Mode = { readonly kind: "live"; readonly catalog: Read<readonly Artifact[]> } | { readonly kind: "recorded" }

/**
 * Decide once where documents come from. A workspace whose server reports a scope and whose cell
 * answers `GET /v1/definitions` is live, and every read then goes to that cell: a read it does not
 * serve says so ({@link NotServed}) and is never replaced by a recording. With no session, no cell
 * or no host connected (a 503, a 404, or a page that is not a document) Studio shows the recording,
 * and every recorded read says it is one. Any other answer is the cell's and is shown as an error.
 */
export async function readMode(fetcher: Fetch): Promise<Mode> {
  let scope: StudioScope | null
  try { scope = await readStudioScope(fetcher) } catch (error) {
    if (error instanceof CellUnavailable) return { kind: "recorded" }
    throw error
  }
  if (scope === null) return { kind: "recorded" }
  const active = scope
  const response = await scopedHostFetch(fetcher, "/api/host", async () => active)("/v1/definitions", { headers: { accept: "application/json" } })
  if (response.status === 404 || response.status === 503) return { kind: "recorded" }
  const text = await response.text()
  let parsed: unknown
  try { parsed = JSON.parse(text) } catch { return { kind: "recorded" } }
  if (!response.ok) throw new CellUnavailable(response.status, (parsed as { message?: unknown } | null)?.message as string | undefined)
  const artifacts = (parsed as { artifacts?: unknown } | null)?.artifacts
  if (!Array.isArray(artifacts)) throw new CellUnavailable(response.status, "The cell's definitions read carried no artifacts.")
  const read = readDocument<{ readonly artifacts: readonly Artifact[] }>(text, "live")
  return { kind: "live", catalog: subRead(read, "/artifacts", read.document.artifacts) }
}

/** The live transport over one connected cell's routes. */
export function liveTransport(catalog: Read<readonly Artifact[]>, fetcher: Fetch): Transport {
  const call = scopedHostFetch(fetcher, "/api/host", () => readStudioScope(fetcher))
  /** One read; `null` when the cell answers 404 (no such object, or none this principal may read). */
  const read = async <T,>(path: string): Promise<Read<T> | null> => {
    const response = await call(path, { headers: { accept: "application/json" } })
    const text = await response.text()
    if (response.status === 404) return null
    if (!response.ok) {
      let message: string | undefined
      try { message = (JSON.parse(text) as { message?: string }).message } catch { /* not a refusal document */ }
      throw new CellUnavailable(response.status, message)
    }
    return readDocument<T>(text, "live")
  }
  const objectPath = (objectType: string, id: string, leaf?: string) => `/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}${leaf ? `/${leaf}` : ""}`
  return {
    catalog: async () => catalog,
    definitions: async () => catalog.document.map((artifact, index) => subRead(catalog, `/${index}`, artifact as Envelope)),
    questionBoard: async () => { throw new NotServed("Saved questions are") },
    restatement: async () => { throw new NotServed("Restated answers are") },
    guarantees: async () => { throw new NotServed("Model-checked guarantees are") },
    objectPage: async (objectType, after) => read<ObjectPage>(`/v1/objects/${encodeURIComponent(objectType)}${after === undefined ? "" : `?after=${encodeURIComponent(after)}`}`),
    object: async (objectType, id) => {
      const [object, links, source] = await Promise.all([
        read<ObjectRecord>(objectPath(objectType, id)),
        read<{ objectType: string; id: string; items: LinkPage["items"]; next?: string | null }>(objectPath(objectType, id, "links")),
        // 404: no mapping produced this object (or its source is not readable): there is no lineage to show.
        read<SourceLineage>(objectPath(objectType, id, "lineage")),
      ])
      if (object === null || links === null) return null
      const page = linkPage(links)
      return readsOf(objectType, id, object.document, page.document) ? { object, links: page, source } : null
    },
  }
}

let mode: Promise<Mode> | undefined
const browserFetch: Fetch = (input, init) => fetch(input, init)

/** The mode this page load reads in; a failed decision is tried again on the next read. */
export function currentMode(fetcher: Fetch = browserFetch): Promise<Mode> {
  mode ??= readMode(fetcher).catch((error: unknown) => { mode = undefined; throw error })
  return mode
}
