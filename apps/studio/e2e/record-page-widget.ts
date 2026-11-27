import { readFileSync } from "node:fs"

/**
 * Studio's record page as a cell serves it (#1762 C2): the admitted widget the demo workspace's
 * definitions carry (`registry/studio/artifacts`). A stand-in that serves definitions of its own
 * serves this beside them, as a cell that serves Studio's record page does.
 */
export const RECORD_PAGE_WIDGET = JSON.parse(readFileSync(new URL("../../../registry/studio/artifacts/norn.studio.record.widget.json", import.meta.url), "utf8")) as { readonly artifactId: string }

/** `artifacts` with the record page among them. */
export const withRecordPage = <T,>(artifacts: readonly T[]): readonly (T | typeof RECORD_PAGE_WIDGET)[] =>
  artifacts.some((artifact) => (artifact as { readonly artifactId?: unknown }).artifactId === RECORD_PAGE_WIDGET.artifactId) ? artifacts : [...artifacts, RECORD_PAGE_WIDGET]
