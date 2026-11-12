import { cellRead } from "./live-transport.ts"

/** The cell's upload routes (L-connect): the bytes go straight to storage under a signed URL; the cell reads them back. */
export interface Begun { readonly uploadId: string; readonly putUrl: string; readonly putHeaders: Readonly<Record<string, string>>; readonly expiresAt: string }
export type Inferred = "string" | "decimal" | "integer" | "boolean" | "date" | "datetime"
export interface Column { readonly name: string; readonly inferred: Inferred; readonly nullable: boolean; readonly samples: readonly unknown[] }
export interface Completed { readonly uri: string; readonly sha256: string; readonly size: number; readonly rows: number; readonly columns: readonly Column[] }
export interface Loaded { readonly connectionId: string; readonly objectType: string; readonly definitionsVersion: number }
export interface LoadColumn { readonly name: string; readonly type: Inferred; readonly include: boolean }

type Put = (url: string, init: RequestInit) => Promise<Response>

/** Begin an upload, send the file to the URL the cell signed, and have the cell read it back. */
export async function upload(file: File, put: Put = (url, init) => fetch(url, init)): Promise<{ readonly uploadId: string; readonly completed: Completed }> {
  const begun = (await cellRead<Begun>("/v1/uploads:begin", "Uploads are", { method: "POST", body: JSON.stringify({ fileName: file.name, contentType: file.type || "text/csv", size: file.size }) })).document
  const sent = await put(begun.putUrl, { method: "PUT", headers: begun.putHeaders, body: file })
  if (!sent.ok) throw new Error(`The storage refused the file (${sent.status}); nothing was loaded.`)
  const completed = (await cellRead<Completed>(`/v1/uploads/${encodeURIComponent(begun.uploadId)}:complete`, "Uploads are", { method: "POST", body: "{}" })).document
  return { uploadId: begun.uploadId, completed }
}

/** Ask the cell to admit a type for the upload and load its rows through the mapping path. */
export const load = async (uploadId: string, typeName: string, key: readonly string[], columns: readonly LoadColumn[]): Promise<Loaded> =>
  (await cellRead<Loaded>(`/v1/uploads/${encodeURIComponent(uploadId)}:load`, "Uploads are", { method: "POST", body: JSON.stringify({ typeName, key, columns }) })).document
