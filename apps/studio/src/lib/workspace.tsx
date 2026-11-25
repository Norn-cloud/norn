/**
 * The page's workspace, decided once from the server's mode answer (#1762).
 *
 * `WorkspaceProvider` (at Studio's shell) turns the mode read (`useMode`: the session's scope and the
 * cell's answer to `GET /v1/definitions`) into one of four states, a plain discriminated union:
 *
 * - `deciding`: the mode is being read; nothing is read and nothing cached is shown.
 * - `unavailable`: the mode read failed (including a refetch that failed after an earlier answer); the
 *   page shows Workspace unavailable with the mode's Retry (`retryMode`), never a read without a workspace.
 * - `live`: a cell answered at `scope`, a {@link DecidedScope}; every record read goes through `reader`.
 * - `recorded`: no cell is connected; `reader` reads the recording only, never a live cell.
 *
 * Studio decides nothing itself: the states only reflect the server's answer, and the cell still
 * decides every read. A {@link DecidedScope} is made only here, so a live record read at the session's
 * scope, or at none, does not compile (`test/workspace-types.test.ts`), and pages cannot reach the raw
 * reads (`lib/raw/`, `test/workspace-reads-lint.test.ts`).
 */
import { createContext, use, useMemo, type ReactNode } from "react"
import { defaultFetch, readAtScope, readStudioScope, StudioScopeError, studioScopeKey, type StudioScope } from "./definitions.ts"
import type { Read } from "./exact.ts"
import { useMode, type Fetch, type Mode } from "./live-transport.ts"
import { readFindings, readObjectAtScope, readRecordedObject } from "./raw/record.ts"
import { readRecordedRecords, readRecordsAtScope } from "./raw/records.ts"
import { recordsQueryParams, recordsQueryPrefix, type RecordsQuery } from "./records-query.ts"
import type { PageRead, RecordsRead } from "./records-source.ts"
import type { ObjectReads } from "./transport.ts"
import type { ObjectFindings } from "../evidence/review.ts"
import type { WorkspaceState } from "./state/index.ts"

declare const decided: unique symbol

/**
 * A workspace scope the server's mode answer decided: the workspace whose cell answered the page's
 * definitions read. Only {@link workspaceOf} makes one; every live record read takes one.
 */
export type DecidedScope = StudioScope & { readonly [decided]: true }

/** One read as TanStack Query runs it: its key always names the workspace it is read in. */
export interface ReaderQuery<T> {
  readonly queryKey: readonly unknown[]
  readonly queryFn: (context: { readonly signal: AbortSignal }) => Promise<T>
  readonly staleTime?: number
}

/**
 * The live reads of one decided workspace. Each read is sent to `scope` and only while the session is
 * still there, checked before and after (`readAtScope`), and cached under `scope`: the same id in another
 * workspace is another record.
 */
export interface ScopedReader {
  readonly kind: "live"
  readonly scope: DecidedScope
  /** One page of a type's records: `["records", type, orgId, environmentId, query]`. */
  readonly records: (objectType: string, query: RecordsQuery) => ReaderQuery<RecordsRead>
  /** One record with its links and lineage, `null` when the cell does not serve it: `["object", type, id, orgId, environmentId]`. */
  readonly object: (objectType: string, id: string) => ReaderQuery<ObjectReads | null>
  /** One record's findings: `["findings", type, id, orgId, environmentId]`. */
  readonly findings: (objectType: string, id: string) => ReaderQuery<Read<ObjectFindings>>
}

/** The reads of the recording, under keys no live answer reaches. The recording keeps no findings. */
export interface RecordedReader {
  readonly kind: "recorded"
  /** One recorded page: `["records", type, "recorded", query]`. It never sorts or filters. */
  readonly records: (objectType: string, query: RecordsQuery) => ReaderQuery<RecordsRead>
  /** One recorded record: `["object", "recorded", type, id]`. */
  readonly object: (objectType: string, id: string) => ReaderQuery<ObjectReads | null>
}

/** The workspace a page reads in, as the server's mode answer decided it (`lib/state`'s {@link WorkspaceState}). */
export type Workspace = WorkspaceState<DecidedScope, ScopedReader, RecordedReader>

/** A decided workspace: one a page may read in. */
export type DecidedWorkspace = Extract<Workspace, { readonly kind: "live" | "recorded" }>

/** The mode read as `useMode` answers it. */
export type ModeAnswer =
  | { readonly status: "pending" }
  | { readonly status: "error"; readonly error: Error }
  | { readonly status: "success"; readonly data: Mode }

/**
 * How long a record's findings stay fresh: the record page's header and its Findings panel share one
 * read, so the panel mounting (or a tab switch) does not read them again (#1754: each read is three
 * requests, `/api/me` before and after). Read again, a correction and Retry read them at once.
 */
export const FINDINGS_STALE_MS = 30_000

const RECORDED: RecordedReader = Object.freeze({
  kind: "recorded" as const,
  records: (objectType: string, query: RecordsQuery) => ({
    queryKey: ["records", objectType, "recorded", recordsQueryParams(query).toString()],
    queryFn: () => readRecordedRecords(objectType, query),
  }),
  object: (objectType: string, id: string) => ({
    queryKey: ["object", "recorded", objectType, id],
    queryFn: () => readRecordedObject(objectType, id),
  }),
})

function scopedReader(scope: DecidedScope, fetcher: Fetch): ScopedReader {
  const atScope = <T,>(signal: AbortSignal, read: () => Promise<T>) => readAtScope(scope, (s) => readStudioScope(fetcher, s), read, signal)
  return Object.freeze({
    kind: "live" as const,
    scope,
    records: (objectType: string, query: RecordsQuery) => ({
      queryKey: [...recordsQueryPrefix(objectType, scope), recordsQueryParams(query).toString()],
      queryFn: ({ signal }: { readonly signal: AbortSignal }) => atScope(signal, () => readRecordsAtScope(objectType, query, scope, fetcher, signal)),
    }),
    object: (objectType: string, id: string) => ({
      queryKey: ["object", objectType, id, scope.orgId, scope.environmentId],
      queryFn: ({ signal }: { readonly signal: AbortSignal }) => readObjectAtScope(objectType, id, scope, signal, fetcher),
    }),
    findings: (objectType: string, id: string) => ({
      queryKey: ["findings", objectType, id, scope.orgId, scope.environmentId],
      queryFn: ({ signal }: { readonly signal: AbortSignal }) => readFindings(objectType, id, scope, signal, fetcher),
      staleTime: FINDINGS_STALE_MS,
    }),
  })
}

const DECIDING: Workspace = Object.freeze({ kind: "deciding" as const })
const RECORDED_WORKSPACE: Workspace = Object.freeze({ kind: "recorded" as const, reader: RECORDED })
/** One live workspace per decided scope, so a page's reader keeps its identity across renders. */
const live = new Map<string, Workspace>()

function liveWorkspace(answered: StudioScope, fetcher: Fetch): Workspace {
  const key = studioScopeKey(answered)
  const kept = fetcher === defaultFetch ? live.get(key) : undefined
  if (kept !== undefined) return kept
  // The only place a DecidedScope is made: from the scope a cell's definitions read answered at.
  const scope = Object.freeze({ orgId: answered.orgId, environmentId: answered.environmentId }) as DecidedScope
  const workspace: Workspace = Object.freeze({ kind: "live" as const, scope, reader: scopedReader(scope, fetcher) })
  if (fetcher === defaultFetch) live.set(key, workspace)
  return workspace
}

/** The workspace a mode answer decides. `fetcher` is the live reader's (tests pass a stand-in cell). */
export function workspaceOf(mode: ModeAnswer, fetcher: Fetch = defaultFetch): Workspace {
  if (mode.status === "pending") return DECIDING
  if (mode.status === "error") return { kind: "unavailable", error: mode.error }
  return mode.data.kind === "live" ? liveWorkspace(mode.data.scope, fetcher) : RECORDED_WORKSPACE
}

/** Outside a provider nothing is decided: every read waits, and nothing is read. */
const WorkspaceContext = createContext<Workspace>(DECIDING)

function ModeWorkspace({ children }: { readonly children: ReactNode }) {
  const mode = useMode()
  const workspace = useMemo(() => workspaceOf(mode), [mode.status, mode.data, mode.error])
  return <WorkspaceContext value={workspace}>{children}</WorkspaceContext>
}

/**
 * Decide the workspace once, from the mode read, for every page under it. `workspace` provides one
 * {@link workspaceOf} already decided instead (component tests, which read no mode).
 */
export function WorkspaceProvider({ workspace, children }: { readonly workspace?: Workspace; readonly children: ReactNode }) {
  return workspace === undefined ? <ModeWorkspace>{children}</ModeWorkspace> : <WorkspaceContext value={workspace}>{children}</WorkspaceContext>
}

/** The page's workspace, as its {@link WorkspaceProvider} decided it. */
export const useWorkspace = (): Workspace => use(WorkspaceContext)

/** A reader's records read as a plain function: the Timeline's nearest-record reads, a record picker's pages. */
export const readPage = (reader: ScopedReader | RecordedReader): PageRead =>
  (objectType, query, signal) => reader.records(objectType, query).queryFn({ signal: signal ?? new AbortController().signal })

const undecidedRead = (): Promise<never> => Promise.reject(new StudioScopeError("No workspace is decided for this read."))

/**
 * A read a page asks for before its workspace may be decided: the read itself once it is (`query`),
 * and until then nothing, under a key no answer reaches (`["undecided", …key]`), so nothing cached in
 * another workspace, or before the mode failed, can show.
 */
export function untilDecided<T>(query: ReaderQuery<T> | undefined, key: readonly unknown[]): ReaderQuery<T> & { readonly enabled: boolean } {
  return query === undefined ? { queryKey: ["undecided", ...key], queryFn: undecidedRead, enabled: false } : { ...query, enabled: true }
}
