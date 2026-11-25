import type { Diagnostic } from "./cell-read.ts"
import type { Read } from "./exact.ts"
import type { RecordsQuery, Served, Total } from "./records-query.ts"
import type { ObjectPage } from "./transport.ts"

/** One diagnostic of a refusal, its pointer into the request (`/sort/0`, `/filter/all/1/value`, `/after`). */
export type QueryDiagnostic = Diagnostic

/**
 * What one page read came back as. Each kind is a state the grid shows; none is guessed. Pages read
 * records through their workspace's reader (`lib/workspace.tsx`); the reads themselves are `lib/raw/records.ts`.
 */
export type RecordsRead =
  | {
    readonly kind: "page"
    readonly read: Read<ObjectPage>
    /** `live` from the decided workspace's cell; `recorded` from the replayed documents when the workspace is the recording. */
    readonly origin: "live" | "recorded"
    /** What the server applied; null when it did not sort or filter. */
    readonly served: Served | null
    readonly next: string | undefined
    readonly total: Total | null
  }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string; readonly diagnostics: readonly QueryDiagnostic[] }
  /** The data moved under the cursor (409 `object.snapshot_moved`): read again from page one. */
  | { readonly kind: "restart" }
  /** There is no read of this type at all (no recording, and the cell has no route for it). */
  | { readonly kind: "none" }
  | { readonly kind: "no-cell" }

/** One page read as a plain function: a workspace reader's records (`readPage`), or a test's stand-in. */
export type PageRead = (objectType: string, query: RecordsQuery, signal?: AbortSignal) => Promise<RecordsRead>

/** The diagnostics that point at one part of the request, e.g. `/filter/all/2`. */
export const diagnosticsAt = (diagnostics: readonly QueryDiagnostic[], prefix: string): readonly QueryDiagnostic[] =>
  diagnostics.filter(({ pointer }) => pointer === prefix || pointer?.startsWith(`${prefix}/`) === true)
