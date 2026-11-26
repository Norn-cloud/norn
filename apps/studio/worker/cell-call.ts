/**
 * A call to the cell as one caller: what the Worker's server-side routes (the
 * question catalog and check, the dashboard agent) read through. It names no
 * Worker binding, so the local Studio (`scripts/local-cell.ts`) can serve the
 * same routes over a `norn dev` session.
 */
export type CellAnswer =
  | { readonly kind: "answer"; readonly status: number; readonly body: unknown }
  /** The workspace has no data environment: nothing was asked. */
  | { readonly kind: "no-cell" }

export interface Cell {
  get(path: string): Promise<CellAnswer>
  post(path: string, body: unknown): Promise<CellAnswer>
}
