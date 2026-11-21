/** Why a version names no writer, as the cell decides it: the long form behind the short label. */
export const HIDDEN_WRITER_WHY = "Written by a rule, an import, or a call not shown to you"

/**
 * A version the cell names no writer for: a short label, its longer reason in a tooltip and, for a
 * screen reader, read after it. It works inside a button (a history row), so it holds no control.
 */
export function HiddenWriter() {
  return <span title={HIDDEN_WRITER_WHY} data-hidden-writer="">Change not shown to you<span className="sr-only">. {HIDDEN_WRITER_WHY}.</span></span>
}
