import { cn } from "@/lib/utils"

/**
 * A keyboard shortcut (`⌘K`, `⌘⇧Z`): keys read left to right in every
 * language, so the hint is an isolate in its own direction, never mirrored
 * into `K⌘` by right-to-left text around it.
 */
export function Keys({ children, className }: { readonly children: string; readonly className?: string }) {
  return <kbd dir="ltr" className={cn("font-sans [unicode-bidi:isolate]", className)}>{children}</kbd>
}

/** The same isolation for plain text such as a `title` tooltip: a left-to-right isolate (LRI … PDI). */
export const keysText = (keys: string) => `⁦${keys}⁩`

const EDITOR_SHORTCUTS: readonly (readonly [string, string])[] = [
  ["⌘Z", "undo"], ["⌘⇧Z", "redo"], ["⌘D", "duplicate"], ["⌘C/⌘V", "copy and paste"], ["⌘A", "select all"], ["⌘S", "save draft"],
]

/** The canvas editors' shortcut line, each hint isolated. */
export function EditorShortcuts({ className }: { readonly className?: string }) {
  return (
    <p className={className}>
      {EDITOR_SHORTCUTS.map(([keys, what]) => <span key={keys}><Keys>{keys}</Keys> {what} · </span>)}
      <Keys>Delete</Keys> removes
    </p>
  )
}
