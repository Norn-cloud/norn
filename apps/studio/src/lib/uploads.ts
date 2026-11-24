import { CellUnavailable } from "./definitions.ts"
import { CellRefusal, NotServed, cellRead } from "./live-transport.ts"

/**
 * A CSV into the cell (L-connect's upload routes, L-bulk's dataset load): the bytes go straight to
 * storage under a URL the cell signs; the cell reads them back and proposes each column's kind;
 * `:define` admits a type for them; then either L-bulk's `/v1/datasets:load` puts every row in the
 * tenant's analytical engine, or `:load` writes each row through the cell as a governed record.
 * Every step is the cell's decision; Studio only carries the choices.
 */
export interface Begun { readonly uploadId: string; readonly putUrl: string; readonly putHeaders: Readonly<Record<string, string>>; readonly expiresAt: string }
export type Inferred = "string" | "text" | "decimal" | "integer" | "boolean" | "date" | "datetime"
/** The kinds a column can be defined as. */
export type Kind = "text" | "integer" | "decimal" | "boolean" | "datetime"
export const KINDS: readonly Kind[] = ["text", "integer", "decimal", "boolean", "datetime"]
export interface Column { readonly id?: string; readonly name: string; readonly inferred: Inferred; readonly nullable: boolean; readonly samples: readonly unknown[] }
export interface Completed { readonly uri: string; readonly sha256: string; readonly size: number; readonly rows: number; readonly columns: readonly Column[] }
export interface DefineColumn { readonly id: string; readonly type: Kind; readonly include: boolean; readonly measure: boolean }
export interface Defined { readonly objectType: string; readonly definitionsVersion: number }
/** L-bulk's answer: how many rows landed in the engine and why any were set aside. */
export interface DatasetLoaded {
  readonly objectType: string
  readonly rows: number
  readonly rejected: number
  readonly reasons: Readonly<Record<string, number>>
  readonly bytes: number
  readonly millis: number
}
/** L-connect's row-by-row load: includes landed and quarantined row counts when served. */
export interface RecordsLoaded {
  readonly connectionId: string
  readonly objectType: string
  readonly definitionsVersion: number
  readonly rowsRead?: number
  readonly rowsLanded?: number
  readonly rowsQuarantined?: number
  readonly quarantineReasons?: Readonly<Record<string, number>>
}

type Put = (url: string, init: RequestInit) => Promise<Response>

/** How long one step of an upload may take before Studio says the cell or the storage did not answer. */
export const UPLOAD_STEP_MS = 90_000

/**
 * Raised when a row-loading write (`/v1/datasets:load` or `/v1/uploads/{id}:load`) timed out or
 * lost its connection before the cell's reply arrived: the server may still be processing or may
 * have already finished, so Studio must reconcile against `source.Connection` instead of claiming
 * "nothing was loaded" or blindly re-submitting.
 */
export class UploadOutcomeUnknownError extends Error {
  readonly timedOut: boolean
  constructor(timedOut: boolean, message?: string) {
    super(message ?? (timedOut
      ? `The workspace did not answer within ${UPLOAD_STEP_MS / 1000} seconds; the load outcome is unknown.`
      : "The connection dropped before the workspace replied; the load outcome is unknown."))
    this.name = "UploadOutcomeUnknownError"
    this.timedOut = timedOut
  }
}

/** Whether an error leaves a row-loading write's outcome uncertain (timeout or dropped connection). */
export function isUncertainWriteError(error: unknown): boolean {
  if (error instanceof UploadOutcomeUnknownError) return true
  if (error instanceof CellUnavailable) return error.status === 0 || error.timedOut === true
  if (error instanceof TypeError) return true
  const name = error instanceof Error ? error.name : ""
  return name === "TimeoutError" || name === "AbortError"
}

/** A step that gave up or could not reach its server, in words the reader can act on; anything else as it came. */
export function uploadFailure(step: "storage" | "cell", error: unknown): Error {
  const name = error instanceof Error ? error.name : ""
  if (name === "TimeoutError" || name === "AbortError") return new Error(step === "storage" ? `The storage did not take the file within ${UPLOAD_STEP_MS / 1000} seconds; nothing was loaded.` : `The cell did not answer within ${UPLOAD_STEP_MS / 1000} seconds; nothing was loaded.`)
  if (error instanceof TypeError && step === "storage") return new Error("The browser could not reach the storage (it may not accept uploads from this site); nothing was loaded.")
  return error instanceof Error ? error : new Error(String(error))
}

function withDeadline<T>(timeoutMs: number, run: (signal: AbortSignal) => Promise<T>): Promise<T> {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(new DOMException("Timed out", "TimeoutError")), timeoutMs)
  return run(controller.signal).finally(() => clearTimeout(timer))
}

const step = async <T>(kind: "storage" | "cell", run: (signal: AbortSignal) => Promise<T>, timeoutMs = UPLOAD_STEP_MS): Promise<T> => {
  try { return await withDeadline(timeoutMs, run) } catch (error) { throw uploadFailure(kind, error) }
}

const writeStep = async <T>(run: (signal: AbortSignal) => Promise<T>, timeoutMs = UPLOAD_STEP_MS): Promise<T> => {
  try {
    return await withDeadline(timeoutMs, run)
  } catch (error) {
    if (error instanceof NotServed || error instanceof CellRefusal) throw error
    if (error instanceof CellUnavailable && error.status > 0 && !error.timedOut) throw error
    if (isUncertainWriteError(error)) {
      const name = error instanceof Error ? error.name : ""
      const timedOut = (error instanceof CellUnavailable && error.timedOut === true) || name === "TimeoutError" || name === "AbortError"
      throw new UploadOutcomeUnknownError(timedOut)
    }
    throw error
  }
}

/** The `source.Connection` record id the cell writes for an upload of `typeNameOrObjectType`. */
export const uploadConnectionId = (typeNameOrObjectType: string): string => {
  const stripped = typeNameOrObjectType.trim().replace(/^upload\./i, "")
  return `upload.${stripped.toLowerCase()}`
}

export interface UploadConnectionSnapshot {
  readonly id: string
  readonly version: number
  readonly endedAt: string | null
  readonly watermarkSha256: string | null
  readonly rowsRead: number | null
  readonly rowsLanded: number | null
  readonly rowsQuarantined: number | null
  readonly quarantineReasons: Readonly<Record<string, number>>
}

interface RawConnectionRecord {
  readonly id?: string
  readonly version?: number
  readonly values?: {
    readonly last_sync_ended_at?: string | null
    readonly watermark?: string | null
    readonly rows_read?: number | null
    readonly rows_landed?: number | null
    readonly rows_quarantined?: number | null
    readonly quarantine_reasons?: string | Readonly<Record<string, number>> | null
  }
}

function parseWatermarkSha(raw: string | null | undefined): string | null {
  if (typeof raw !== "string" || raw.trim() === "") return null
  try {
    const parsed = JSON.parse(raw) as { sha256?: unknown } | null
    return typeof parsed?.sha256 === "string" ? parsed.sha256 : null
  } catch {
    return null
  }
}

function parseReasons(raw: string | Readonly<Record<string, number>> | null | undefined): Readonly<Record<string, number>> {
  if (raw === null || raw === undefined) return {}
  let obj: unknown = raw
  if (typeof raw === "string") {
    if (raw.trim() === "") return {}
    try { obj = JSON.parse(raw) } catch { return {} }
  }
  if (obj === null || typeof obj !== "object" || Array.isArray(obj)) return {}
  return Object.fromEntries(
    Object.entries(obj as Record<string, unknown>).filter(([, value]) => typeof value === "number" && Number.isFinite(value)) as [string, number][],
  )
}

/** Best-effort read of `source.Connection/{connectionId}` for reconciliation; returns `null` when absent or unreadable. */
export async function readUploadConnection(connectionId: string): Promise<UploadConnectionSnapshot | null> {
  try {
    const read = await cellRead<RawConnectionRecord>(`/v1/objects/source.Connection/${encodeURIComponent(connectionId)}`, "Upload connections are")
    const doc = read.document
    const values = doc.values ?? {}
    return {
      id: typeof doc.id === "string" ? doc.id : connectionId,
      version: typeof doc.version === "number" ? doc.version : 0,
      endedAt: typeof values.last_sync_ended_at === "string" ? values.last_sync_ended_at : null,
      watermarkSha256: parseWatermarkSha(values.watermark),
      rowsRead: typeof values.rows_read === "number" ? values.rows_read : null,
      rowsLanded: typeof values.rows_landed === "number" ? values.rows_landed : null,
      rowsQuarantined: typeof values.rows_quarantined === "number" ? values.rows_quarantined : null,
      quarantineReasons: parseReasons(values.quarantine_reasons),
    }
  } catch {
    return null
  }
}

/**
 * Reconcile an uncertain upload write against `source.Connection/{connectionId}` without issuing
 * another write: succeeds only when the record advanced past the pre-submit baseline AND its
 * watermark digest matches the uploaded file's `sha256`.
 */
export async function reconcileUploadConnection(params: {
  readonly connectionId: string
  readonly sha256: string
  readonly baselineVersion: number
  readonly baselineEndedAt: string | null
}): Promise<UploadConnectionSnapshot | null> {
  const current = await readUploadConnection(params.connectionId)
  if (current === null) return null
  if (current.watermarkSha256 !== params.sha256) return null
  const advanced = current.version > params.baselineVersion
    || (current.endedAt !== null && current.endedAt !== params.baselineEndedAt)
  return advanced ? current : null
}

/** A column's id as the cell names it: the snake id `:complete` gave, or its header written that way. */
export const columnId = (column: Column): string => column.id ?? column.name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "")

/** The kind a column starts as: what the cell inferred, in the words `:define` takes. */
export const kindOf = (inferred: Inferred): Kind => inferred === "string" ? "text" : inferred === "date" ? "datetime" : inferred

/** Begin an upload, send the file to the URL the cell signed, and have the cell read it back. */
export async function upload(file: File, put: Put = (url, init) => fetch(url, init)): Promise<{ readonly uploadId: string; readonly completed: Completed }> {
  const begun = await step("cell", async (signal) => (await cellRead<Begun>("/v1/uploads:begin", "Uploads are", { method: "POST", body: JSON.stringify({ fileName: file.name, contentType: file.type || "text/csv", size: file.size }), signal })).document)
  const sent = await step("storage", (signal) => put(begun.putUrl, { method: "PUT", headers: begun.putHeaders, body: file, signal }))
  if (!sent.ok) throw new Error(`The storage refused the file (${sent.status}); nothing was loaded.`)
  const completed = await step("cell", async (signal) => (await cellRead<Completed>(`/v1/uploads/${encodeURIComponent(begun.uploadId)}:complete`, "Uploads are", { method: "POST", body: "{}", signal })).document)
  return { uploadId: begun.uploadId, completed }
}

const typing = (typeName: string, key: readonly string[], columns: readonly DefineColumn[], sha256: string) =>
  JSON.stringify({ typeName, key, columns, sha256 })

/** Ask the cell to admit a type for the upload, without writing any row. */
export const define = async (
  uploadId: string,
  typeName: string,
  key: readonly string[],
  columns: readonly DefineColumn[],
  sha256: string,
  timeoutMs = UPLOAD_STEP_MS,
): Promise<Defined> =>
  step("cell", async (signal) =>
    (await cellRead<Defined>(`/v1/uploads/${encodeURIComponent(uploadId)}:define`, "Defining a type from an upload is", { method: "POST", body: typing(typeName, key, columns, sha256), signal })).document,
  timeoutMs)

/**
 * Load the upload's rows into the admitted type: into the analytical engine when the cell has one
 * (`/v1/datasets:load`, keyed with `requestKey` for MANIFEST-04 idempotency when provided), or,
 * when it says it has none (`NotServed`, or 501 unconfigured), not at all: the caller then offers
 * the row-by-row load, never silently.
 */
export async function loadDataset(
  uploadId: string,
  objectType: string,
  requestKey?: string,
  timeoutMs = UPLOAD_STEP_MS,
): Promise<DatasetLoaded | "no-engine"> {
  try {
    const body = requestKey === undefined ? { uploadId, objectType } : { uploadId, objectType, requestKey }
    return await writeStep(async (signal) =>
      (await cellRead<DatasetLoaded>("/v1/datasets:load", "Analysis datasets are", { method: "POST", body: JSON.stringify(body), signal })).document,
    timeoutMs)
  } catch (error) {
    if (error instanceof NotServed || (error instanceof CellRefusal && error.status === 501)) return "no-engine"
    throw error
  }
}

/** Write each row through the cell as a governed record (L-connect's `:load`): checked one by one. */
export const loadRecords = async (
  uploadId: string,
  typeName: string,
  key: readonly string[],
  columns: readonly DefineColumn[],
  sha256: string,
  timeoutMs = UPLOAD_STEP_MS,
): Promise<RecordsLoaded> =>
  writeStep(async (signal) =>
    (await cellRead<RecordsLoaded>(`/v1/uploads/${encodeURIComponent(uploadId)}:load`, "Uploads are", { method: "POST", body: typing(typeName, key, columns, sha256), signal })).document,
  timeoutMs)
