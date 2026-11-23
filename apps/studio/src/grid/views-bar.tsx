import { useQuery, useQueryClient } from "@tanstack/react-query"
import { Bookmark, Pencil, Save, Trash2, Undo2 } from "lucide-react"
import { useState, type FormEvent } from "react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { cn } from "@/lib/utils"
import { cellViews, newViewId, sortKeys, sortText, type SavedView, type StoredView, type ViewSaved } from "../lib/views.ts"
import type { GridQuery } from "./records-grid.tsx"

const views = cellViews()

/** The view a grid query saves as. */
const asView = (title: string, objectType: string, query: GridQuery): SavedView => ({
  title,
  objectType,
  ...query.filter && query.filter.all.length > 0 ? { filter: query.filter } : {},
  ...sortText(query.sort) ? { sort: sortText(query.sort) } : {},
  columns: query.columns,
})

/** Whether the grid shows something other than the view as saved. */
const differs = (view: SavedView, query: GridQuery) =>
  JSON.stringify(view.filter ?? null) !== JSON.stringify(query.filter && query.filter.all.length > 0 ? query.filter : null)
  || (view.sort ?? "") !== (sortText(query.sort) ?? "")
  || JSON.stringify(view.columns ?? []) !== JSON.stringify(query.columns)

/** What the cell said, in a line: its checker's first finding, or that someone saved first. */
function problemOf(result: ViewSaved): string | null {
  if (result.saved) return null
  if ("conflict" in result) return "Someone changed this view since you opened it. Open it again to see their version."
  return result.findings.map(({ message }) => message).join(" ") || "The cell refused this view."
}

/** A title asked for in a small popover; the cell checks it like the rest of the view. */
function TitleForm({ initial, submit, onDone }: { readonly initial: string; readonly submit: string; readonly onDone: (title: string) => Promise<string | null> }) {
  const [title, setTitle] = useState(initial)
  const [problem, setProblem] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const send = async (event: FormEvent) => {
    event.preventDefault()
    setBusy(true)
    setProblem(await onDone(title.trim()))
    setBusy(false)
  }
  return (
    <form onSubmit={(event) => void send(event)} className="grid gap-2">
      <label className="grid gap-1 text-sm font-medium">View name<Input value={title} onChange={(event) => setTitle(event.target.value)} autoFocus /></label>
      {problem && <p role="alert" className="text-meta text-destructive">{problem}</p>}
      <Button type="submit" size="sm" disabled={busy || title.trim() === ""}>{submit}</Button>
    </form>
  )
}

/**
 * A type's saved views, kept by the cell as definitions (VIEW-DEF-1): open one, save the grid's
 * query as a new one, update or rename the open one, or delete it, with an undo that installs it
 * again. Whether the reader may change them is the cell's call; its refusal is shown as it is.
 */
export function ViewsBar({ objectType, open, query, onOpen }: {
  readonly objectType: string
  readonly open: StoredView | null
  readonly query: GridQuery | undefined
  readonly onOpen: (view: StoredView | null) => void
}) {
  const client = useQueryClient()
  const list = useQuery({ queryKey: ["views", objectType], queryFn: () => views.list(objectType), retry: false })
  const [notice, setNotice] = useState<{ readonly text: string; readonly undo?: () => Promise<void> } | null>(null)
  const [creating, setCreating] = useState(false)
  const [renaming, setRenaming] = useState(false)
  const reread = () => client.invalidateQueries({ queryKey: ["views", objectType] })
  const create = async (title: string) => {
    if (query === undefined) return "The grid has not answered yet."
    const id = newViewId(objectType, title)
    const view = asView(title, objectType, query)
    const result = await views.save(id, view).catch((error: unknown) => error instanceof Error ? error : new Error(String(error)))
    if (result instanceof Error) return result.message
    if (!result.saved) return problemOf(result)
    await reread()
    setCreating(false)
    onOpen({ id, version: result.version, view })
    setNotice({ text: `Saved “${title}”.` })
    return null
  }
  const update = async (view: SavedView) => {
    if (open === null) return "No view is open."
    const result = await views.save(open.id, view, open.version).catch((error: unknown) => error instanceof Error ? error : new Error(String(error)))
    if (result instanceof Error) return result.message
    if (!result.saved) return problemOf(result)
    await reread()
    onOpen({ id: open.id, version: result.version, view })
    return null
  }
  const remove = async () => {
    if (open === null) return
    const gone = open
    const result = await views.retire(gone.id, gone.version).catch((error: unknown) => error instanceof Error ? error : new Error(String(error)))
    if (result instanceof Error) { setNotice({ text: result.message }); return }
    if (!result.saved) { setNotice({ text: problemOf(result) ?? "Not deleted." }); return }
    await reread()
    onOpen(null)
    // A retired view is installed again as its next version: the undo.
    setNotice({
      text: `Deleted “${gone.view.title}”.`,
      undo: async () => {
        const back = await views.save(gone.id, gone.view, result.version)
        if (!back.saved) { setNotice({ text: problemOf(back) ?? "Not restored." }); return }
        await reread()
        onOpen({ id: gone.id, version: back.version, view: gone.view })
        setNotice({ text: `Restored “${gone.view.title}”.` })
      },
    })
  }
  const changed = open !== null && query !== undefined && differs(open.view, query)
  const tab = (active: boolean) => cn("inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-sm transition-colors", active ? "bg-card font-medium shadow-node ring-1 ring-node-border" : "text-muted-foreground hover:text-foreground")
  return (
    <div className="grid gap-2">
      <div className="flex flex-wrap items-center gap-1.5" role="tablist" aria-label="Views">
        <button type="button" role="tab" aria-selected={open === null} className={tab(open === null)} onClick={() => onOpen(null)}>All records</button>
        {(list.data ?? []).map((stored) => (
          <button key={stored.id} type="button" role="tab" aria-selected={open?.id === stored.id} className={tab(open?.id === stored.id)} onClick={() => onOpen(stored)}>
            <Bookmark className="size-3.5" aria-hidden />{stored.view.title}
          </button>
        ))}
        {list.isError && <span className="text-meta text-muted-foreground">{list.error.message}</span>}
        <span className="flex-1" />
        {open !== null && <>
          <Button type="button" variant="outline" size="sm" className="h-8" disabled={!changed} title={changed ? "Save the grid's filter, sort and columns into this view" : "The grid shows this view as saved"}
            onClick={() => { if (query) void update(asView(open.view.title, objectType, query)).then((problem) => problem && setNotice({ text: problem })) }}><Save />Update view</Button>
          <Popover open={renaming} onOpenChange={setRenaming}>
            <PopoverTrigger asChild><Button type="button" variant="ghost" size="sm" className="h-8"><Pencil />Rename</Button></PopoverTrigger>
            <PopoverContent align="end" className="w-72 rounded-node border-node-border shadow-float">
              <TitleForm initial={open.view.title} submit="Rename" onDone={async (title) => { const problem = await update({ ...open.view, title }); if (problem === null) setRenaming(false); return problem }} />
            </PopoverContent>
          </Popover>
          <Button type="button" variant="ghost" size="sm" className="h-8 text-destructive hover:text-destructive" onClick={() => void remove()}><Trash2 />Delete</Button>
        </>}
        <Popover open={creating} onOpenChange={setCreating}>
          <PopoverTrigger asChild><Button type="button" variant="outline" size="sm" className="h-8" disabled={query === undefined}><Bookmark />Save as view</Button></PopoverTrigger>
          <PopoverContent align="end" className="w-72 rounded-node border-node-border shadow-float">
            <TitleForm initial="" submit="Save view" onDone={create} />
          </PopoverContent>
        </Popover>
      </div>
      {notice && (
        <p role="status" className="flex items-center gap-2 text-meta text-muted-foreground">
          {notice.text}
          {notice.undo && <Button type="button" variant="link" size="sm" className="h-auto p-0 text-meta" onClick={() => void notice.undo?.()}><Undo2 className="size-3" />Undo</Button>}
        </p>
      )}
    </div>
  )
}

/** A stored view's query, to open the grid with. */
export const openedQuery = (stored: StoredView): Partial<GridQuery> => ({
  filter: stored.view.filter ?? null,
  sort: sortKeys(stored.view.sort),
  ...stored.view.columns ? { columns: stored.view.columns } : {},
})
