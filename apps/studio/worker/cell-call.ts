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

export interface Finding {
  readonly code: string
  readonly message: string
  readonly pointer: string | null
}

/** Decode checker diagnostics from a cell refusal/admission payload into typed findings. */
export const findingsOf = (diagnostics: unknown): Finding[] =>
  (Array.isArray(diagnostics) ? diagnostics : []).map((d: { code?: unknown; message?: unknown; span?: { pointer?: unknown } | null }) => ({
    code: String(d.code ?? ""),
    message: String(d.message ?? ""),
    pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null,
  }))
