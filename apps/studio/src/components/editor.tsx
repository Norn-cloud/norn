import { useBlocker } from "@tanstack/react-router"
import { ArrowDown, ArrowUp, CircleAlert, Copy, Redo2, Save, Trash2, Undo2 } from "lucide-react"
import { useCallback, useEffect, useRef, useState } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { history, modKey, type History, type Issue } from "@/lib/editor"

/**
 * One editor's state with undo and redo, and whether it differs from what was
 * last saved or opened. Shortcuts: mod+Z undo, mod+Shift+Z or mod+Y redo,
 * mod+S save, mod+Enter run.
 */
export function useEditor<T>({ initial, onSave, onRun }: { readonly initial: T; readonly onSave?: (value: T) => void; readonly onRun?: (value: T) => void }) {
  const [state, setState] = useState<History<T>>(() => history.start(initial))
  const [saved, setSaved] = useState<T>(initial)
  const value = state.present
  const set = useCallback((next: T | ((current: T) => T)) => setState((h) => history.set(h, typeof next === "function" ? (next as (current: T) => T)(h.present) : next)), [])
  const undo = useCallback(() => setState(history.undo), [])
  const redo = useCallback(() => setState(history.redo), [])
  /** Open a stored object: it becomes the clean state, with no history behind it. */
  const open = useCallback((next: T) => { setState(history.start(next)); setSaved(next) }, [])
  const markSaved = useCallback((at: T) => setSaved(at), [])
  const actions = useRef({ onSave, onRun, value })
  actions.current = { onSave, onRun, value }
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return
      const key = event.key.toLowerCase()
      const act = key === "z" && !event.shiftKey ? undo : (key === "z" && event.shiftKey) || key === "y" ? redo
        : key === "s" && actions.current.onSave ? () => actions.current.onSave?.(actions.current.value)
        : key === "enter" && actions.current.onRun ? () => actions.current.onRun?.(actions.current.value) : undefined
      if (act === undefined) return
      event.preventDefault()
      act()
    }
    globalThis.addEventListener?.("keydown", onKey)
    return () => globalThis.removeEventListener?.("keydown", onKey)
  }, [undo, redo])
  return { value, set, undo, redo, open, markSaved, canUndo: state.past.length > 0, canRedo: state.future.length > 0, dirty: JSON.stringify(value) !== JSON.stringify(saved) }
}

/** Asks before leaving an editor with unsaved changes, in the app and when closing the tab. */
export function DirtyGuard({ dirty }: { readonly dirty: boolean }) {
  useBlocker({ shouldBlockFn: () => dirty && !globalThis.confirm("Leave without saving? Your changes will be lost."), enableBeforeUnload: () => dirty })
  return null
}

export function EditorToolbar({ editor, onSave, saving, children }: { readonly editor: Pick<ReturnType<typeof useEditor>, "undo" | "redo" | "canUndo" | "canRedo" | "dirty">; readonly onSave?: () => void; readonly saving?: boolean; readonly children?: React.ReactNode }) {
  const mod = modKey()
  return (
    <div className="flex flex-wrap items-center gap-2" role="toolbar" aria-label="Editor">
      <Button type="button" variant="ghost" size="icon" aria-label="Undo" title={`Undo (${mod}Z)`} disabled={!editor.canUndo} onClick={editor.undo}><Undo2 /></Button>
      <Button type="button" variant="ghost" size="icon" aria-label="Redo" title={`Redo (${mod}Shift+Z)`} disabled={!editor.canRedo} onClick={editor.redo}><Redo2 /></Button>
      {onSave && <Button type="button" variant="outline" size="sm" title={`Save (${mod}S)`} disabled={!editor.dirty || saving} onClick={onSave}><Save />{saving ? "Saving…" : "Save"}</Button>}
      {editor.dirty ? <Badge variant="outline">Unsaved changes</Badge> : <span className="text-meta text-muted-foreground">All changes saved</span>}
      {children}
    </div>
  )
}

/** Duplicate, move and remove for one row of an ordered list. Alt+↑ and Alt+↓ move the focused row. */
export function RowActions({ index, count, label, onDuplicate, onMove, onRemove }: { readonly index: number; readonly count: number; readonly label: string; readonly onDuplicate: () => void; readonly onMove: (to: number) => void; readonly onRemove: () => void }) {
  return (
    <div className="flex items-center" onKeyDown={(event) => {
      if (!event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return
      event.preventDefault()
      onMove(event.key === "ArrowUp" ? index - 1 : index + 1)
    }}>
      <Button type="button" variant="ghost" size="icon" aria-label={`Move ${label} up`} title="Move up (Alt+↑)" disabled={index === 0} onClick={() => onMove(index - 1)}><ArrowUp /></Button>
      <Button type="button" variant="ghost" size="icon" aria-label={`Move ${label} down`} title="Move down (Alt+↓)" disabled={index === count - 1} onClick={() => onMove(index + 1)}><ArrowDown /></Button>
      <Button type="button" variant="ghost" size="icon" aria-label={`Duplicate ${label}`} title="Duplicate" onClick={onDuplicate}><Copy /></Button>
      <Button type="button" variant="ghost" size="icon" aria-label={`Remove ${label}`} title="Remove" onClick={onRemove}><Trash2 /></Button>
    </div>
  )
}

/** The server's findings, each a link to the field it concerns. */
export function ValidationPanel({ issues, title = "The server found problems" }: { readonly issues: readonly Issue[]; readonly title?: string }) {
  if (issues.length === 0) return null
  const jump = (field: string) => {
    const target = globalThis.document?.querySelector<HTMLElement>(`[data-field="${CSS.escape(field)}"]`)
    target?.scrollIntoView({ block: "center", behavior: "smooth" })
    ;(target?.matches("input,select,textarea,button") ? target : target?.querySelector<HTMLElement>("input,select,textarea,button"))?.focus()
  }
  return (
    <div role="alert" className="space-y-2 rounded-xl border border-destructive/40 bg-card p-3 text-sm">
      <div className="flex items-center gap-2 font-medium"><CircleAlert className="size-4 text-destructive" />{title}</div>
      <ul className="space-y-1">
        {issues.map((issue, index) => (
          <li key={`${issue.code}:${index}`}>
            {issue.field === null ? <span>{issue.message}</span>
              : <button type="button" className="text-left underline decoration-dotted underline-offset-4 hover:text-foreground" onClick={() => jump(issue.field as string)}>{issue.message}</button>}
          </li>
        ))}
      </ul>
    </div>
  )
}
