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
/** L-connect's row-by-row load. */
export interface RecordsLoaded { readonly connectionId: string; readonly objectType: string; readonly definitionsVersion: number }

type Put = (url: string, init: RequestInit) => Promise<Response>

/** How long one step of an upload may take before Studio says the cell or the storage did not answer. */
export const UPLOAD_STEP_MS = 90_000

/** A step that gave up or could not reach its server, in words the reader can act on; anything else as it came. */
export function uploadFailure(step: "storage" | "cell", error: unknown): Error {
  const name = error instanceof Error ? error.name : ""
  if (name === "TimeoutError" || name === "AbortError") return new Error(step === "storage" ? `The storage did not take the file within ${UPLOAD_STEP_MS / 1000} seconds; nothing was loaded.` : `The cell did not answer within ${UPLOAD_STEP_MS / 1000} seconds; nothing was loaded.`)
  if (error instanceof TypeError && step === "storage") return new Error("The browser could not reach the storage (it may not accept uploads from this site); nothing was loaded.")
  return error instanceof Error ? error : new Error(String(error))
}

const step = async <T>(kind: "storage" | "cell", run: (signal: AbortSignal) => Promise<T>): Promise<T> => {
  try { return await run(AbortSignal.timeout(UPLOAD_STEP_MS)) } catch (error) { throw uploadFailure(kind, error) }
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
export const define = async (uploadId: string, typeName: string, key: readonly string[], columns: readonly DefineColumn[], sha256: string): Promise<Defined> =>
  (await cellRead<Defined>(`/v1/uploads/${encodeURIComponent(uploadId)}:define`, "Defining a type from an upload is", { method: "POST", body: typing(typeName, key, columns, sha256) })).document

/**
 * Load the upload's rows into the admitted type: into the analytical engine when the cell has one
 * (`/v1/datasets:load`), or, when it says it has none (`NotServed`, or 501 unconfigured), not at all:
 * the caller then offers the row-by-row load, never silently.
 */
export async function loadDataset(uploadId: string, objectType: string): Promise<DatasetLoaded | "no-engine"> {
  try {
    return (await cellRead<DatasetLoaded>("/v1/datasets:load", "Analysis datasets are", { method: "POST", body: JSON.stringify({ uploadId, objectType }) })).document
  } catch (error) {
    if (error instanceof NotServed || (error instanceof CellRefusal && error.status === 501)) return "no-engine"
    throw error
  }
}

/** Write each row through the cell as a governed record (L-connect's `:load`): checked one by one. */
export const loadRecords = async (uploadId: string, typeName: string, key: readonly string[], columns: readonly DefineColumn[], sha256: string): Promise<RecordsLoaded> =>
  (await cellRead<RecordsLoaded>(`/v1/uploads/${encodeURIComponent(uploadId)}:load`, "Uploads are", { method: "POST", body: typing(typeName, key, columns, sha256) })).document
