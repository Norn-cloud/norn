import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate, useParams } from "@tanstack/react-router"
import { ArrowDown, ArrowUp, Bookmark, Copy, Eye, Pencil, Plus, Shapes, Trash2, Undo2, Zap } from "lucide-react"
import { useEffect, useMemo, useRef, useState, type FormEvent } from "react"
import { DirtyGuard, EditorToolbar, ValidationPanel, useEditor } from "@/components/editor"
import { EmptyState, PageHeader, Panel } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { useActions } from "./actions/action-sheet.tsx"
import { FilterEditor, fromPredicate, toPredicate, type DraftClause } from "./grid/filter-editor.tsx"
import { RecordsGrid } from "./grid/records-grid.tsx"
import { openedQuery } from "./grid/views-bar.tsx"
import type { Concept } from "./lib/catalog.ts"
import { NO_DATA_CELL, useStudioScope, type Finding } from "./lib/definitions.ts"
import type { Issue } from "./lib/editor.ts"
import { useMode } from "./lib/live-transport.ts"
import { recordName } from "./lib/objects.ts"
import { LIMITS, sortable, totalText, type SortKey } from "./lib/records-query.ts"
import { readRecords } from "./lib/records-source.ts"
import { offersTimeline } from "./lib/timeline.ts"
import { cellViews, cleanView, DEFAULT_SPACES, duplicateView, newViewId, sortKeys, sortText, viewField, type SavedView, type StoredView, type ViewSaved } from "./lib/views.ts"
import type { MenuEntry } from "./shell.tsx"
import { useCatalog } from "./sources.tsx"

const views = cellViews()
const HIDDEN_FIELDS = new Set(["id", "label", "display_name"])

const failure = (error: unknown) => error instanceof Error ? error.message : String(error)

function settled(result: ViewSaved) {
  if (result.saved) return result
  throw new Error("conflict" in result
    ? "Someone changed this view since it was read; reload and try again."
    : result.findings.map(({ message }) => message).join(" ") || "The cell refused this view.")
}

const issuesOf = (findings: readonly Finding[]): readonly Issue[] =>
  findings.map(({ code, message, pointer }) => ({ code, message, field: viewField(pointer) }))

interface RemovedView {
  readonly id: string
  readonly version: number
  readonly view: SavedView
}

function UndoDeleteView({ removed, pending, error, onUndo }: {
  readonly removed: RemovedView
  readonly pending: boolean
  readonly error?: string
  readonly onUndo: () => void
}) {
  return (
    <div role="status" className="flex flex-wrap items-center gap-2 rounded-xl border bg-card p-3 text-sm shadow-elevation">
      <span>Deleted “{removed.view.title || "Untitled view"}”. Its history stays in the cell.</span>
      <Button type="button" size="sm" variant="outline" className="ms-auto" disabled={pending} onClick={onUndo}>
        <Undo2 />{pending ? "Restoring…" : "Undo"}
      </Button>
      {error && <p role="alert" className="basis-full text-destructive">Not restored. {error}</p>}
    </div>
  )
}

function RenamePopover({ current, onRename }: {
  readonly current: string
  readonly onRename: (title: string) => Promise<string | null>
}) {
  const [open, setOpen] = useState(false)
  const [title, setTitle] = useState(current)
  const [problem, setProblem] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  useEffect(() => { if (open) { setTitle(current); setProblem(null) } }, [open, current])
  const submit = async (event: FormEvent) => {
    event.preventDefault()
    setBusy(true)
    const err = await onRename(title.trim())
    setBusy(false)
    if (err === null) setOpen(false)
    else setProblem(err)
  }
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button type="button" variant="ghost" size="sm"><Pencil />Rename</Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-72 rounded-node border-node-border shadow-float">
        <form onSubmit={(e) => void submit(e)} className="grid gap-2">
          <label className="grid gap-1 text-sm font-medium">
            View name
            <Input value={title} onChange={(e) => setTitle(e.target.value)} autoFocus />
          </label>
          {problem && <p role="alert" className="text-meta text-destructive">{problem}</p>}
          <Button type="submit" size="sm" disabled={busy || title.trim() === ""}>Rename</Button>
        </form>
      </PopoverContent>
    </Popover>
  )
}

/**
 * Saved views for the Workspace sidebar. Pinned views carry `group: space`, so the shell places
 * them directly inside their Space; unpinned views sit under the Views destination.
 */
export function useViewsMenu(): readonly MenuEntry[] {
  const live = useMode().data?.kind === "live"
  const scope = useStudioScope()
  const enabled = live && scope.isSuccess && scope.data !== null
  const activeScope = scope.data ?? null
  const list = useQuery({
    queryKey: ["views", "all", activeScope?.orgId, activeScope?.environmentId],
    queryFn: () => views.listAll(activeScope),
    enabled,
    retry: false,
    staleTime: 30_000,
  })
  if (!enabled || !list.data) return []
  return list.data.map((stored) => ({
    href: `/views/${encodeURIComponent(stored.id)}`,
    label: stored.view.title || stored.id,
    icon: Bookmark,
    ...(stored.view.space ? { group: stored.view.space } : {}),
  }))
}

/** `/views`: all saved operator views, grouped by Space, with create, edit, rename, duplicate, delete and undo. */
export function ViewsPage() {
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const { catalog } = useCatalog()
  const navigate = useNavigate()
  const client = useQueryClient()
  const activeScope = scope.data ?? null
  const list = useQuery({
    queryKey: ["views", "all", activeScope?.orgId, activeScope?.environmentId],
    queryFn: () => views.listAll(activeScope),
    enabled: cellAvailable,
    retry: false,
  })
  const refresh = () => client.invalidateQueries({ queryKey: ["views"] })
  const [removed, setRemoved] = useState<RemovedView>()

  const duplicate = useMutation({
    mutationFn: async (stored: StoredView) => {
      const copy = duplicateView(stored.view)
      const id = newViewId(copy.objectType, copy.title)
      return settled(await views.save(id, copy))
    },
    onSuccess: refresh,
  })

  const remove = useMutation({
    mutationFn: async (stored: StoredView) => {
      const retired = settled(await views.retire(stored.id, stored.version))
      return retired.saved ? { id: stored.id, version: retired.version, view: stored.view } : undefined
    },
    onSuccess: setRemoved,
    onSettled: refresh,
  })

  const undo = useMutation({
    mutationFn: async (gone: RemovedView) => settled(await views.save(gone.id, gone.view, gone.version)),
    onSuccess: () => setRemoved(undefined),
    onSettled: refresh,
  })

  const rename = async (stored: StoredView, title: string): Promise<string | null> => {
    try {
      const res = await views.save(stored.id, { ...stored.view, title }, stored.version)
      if (!res.saved) return "conflict" in res ? "Someone changed this view; reload and try again." : res.findings.map((f) => f.message).join(" ")
      await refresh()
      return null
    } catch (error) {
      return failure(error)
    }
  }

  const grouped = useMemo(() => {
    const bySpace = new Map<string, StoredView[]>()
    for (const stored of list.data ?? []) {
      const space = stored.view.space?.trim() || "Unpinned"
      const bucket = bySpace.get(space) ?? []
      bucket.push(stored)
      bySpace.set(space, bucket)
    }
    return [...bySpace.entries()].sort(([a], [b]) => a === "Unpinned" ? 1 : b === "Unpinned" ? -1 : a.localeCompare(b))
  }, [list.data])

  return (
    <section className="space-y-6">
      <PageHeader
        title="Views"
        description="Declarative projections over admitted record types, pinned into Spaces. Actions always come from the cell."
        primary={<Button disabled={!cellAvailable} onClick={() => void navigate({ to: "/views/new" })}><Plus />New view</Button>}
      />
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {cellAvailable && list.isPending && <p className="text-muted-foreground">Reading views…</p>}
      {list.isError && <EmptyState icon={Bookmark}>{failure(list.error)}</EmptyState>}
      {duplicate.isError && <p role="alert" className="text-destructive">Not duplicated. {failure(duplicate.error)}</p>}
      {remove.isError && <p role="alert" className="text-destructive">Not deleted. {failure(remove.error)}</p>}
      {removed && <UndoDeleteView removed={removed} pending={undo.isPending} error={undo.isError ? failure(undo.error) : undefined} onUndo={() => undo.mutate(removed)} />}
      {list.data?.length === 0 && (
        <EmptyState
          icon={Bookmark}
          title="No views yet"
          next={cellAvailable && <Button size="sm" variant="outline" onClick={() => void navigate({ to: "/views/new" })}><Plus />New view</Button>}
        >
          Build a filtered view over any record type, pick its columns and offered actions, and pin it into a Space.
        </EmptyState>
      )}
      {grouped.map(([space, items]) => (
        <div key={space} className="space-y-3">
          <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">{space}</h2>
          <div className="grid gap-3">
            {items.map((stored) => {
              const concept = catalog?.byName.get(stored.view.objectType)
              const filterCount = stored.view.filter?.all.length ?? 0
              const rowActionCount = stored.view.rowActions?.length ?? 0
              const bulkActionCount = stored.view.bulkActions?.length ?? 0
              return (
                <Panel
                  key={stored.id}
                  icon={Bookmark}
                  label={stored.view.title || "Untitled view"}
                  action={
                    <div className="flex flex-wrap items-center gap-1">
                      <Button asChild variant="ghost" size="sm"><Link to="/views/$id" params={{ id: stored.id }}><Eye />Open</Link></Button>
                      <Button asChild variant="ghost" size="sm"><Link to="/views/$id/edit" params={{ id: stored.id }}><Pencil />Edit</Link></Button>
                      <RenamePopover current={stored.view.title} onRename={(title) => rename(stored, title)} />
                      <Button variant="ghost" size="sm" disabled={duplicate.isPending} onClick={() => duplicate.mutate(stored)}><Copy />Duplicate</Button>
                      <Button variant="ghost" size="sm" onClick={() => remove.mutate(stored)}><Trash2 />Delete</Button>
                    </div>
                  }
                >
                  <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
                    <Badge variant="secondary">{concept?.title ?? stored.view.objectType}</Badge>
                    {stored.view.space && <Badge variant="outline">Space: {stored.view.space}</Badge>}
                    <span>{filterCount === 0 ? "All records" : `${filterCount} filter${filterCount === 1 ? "" : "s"}`}</span>
                    {rowActionCount > 0 && <span>· {rowActionCount} row action{rowActionCount === 1 ? "" : "s"}</span>}
                    {bulkActionCount > 0 && <span>· {bulkActionCount} bulk action{bulkActionCount === 1 ? "" : "s"}</span>}
                    <span>· Version {stored.version}</span>
                  </div>
                  {stored.view.description && <p className="mt-1.5 text-sm text-muted-foreground">{stored.view.description}</p>}
                </Panel>
              )
            })}
          </div>
        </div>
      ))}
    </section>
  )
}

/** `/views/$id`: open a saved view with its filtered list, row & bulk actions, record drawer and action sheet. */
export function ViewDetailPage() {
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const { id = "" } = useParams({ strict: false }) as { id?: string }
  const { catalog } = useCatalog()
  const navigate = useNavigate()
  const client = useQueryClient()
  const stored = useQuery({
    queryKey: ["view", id, scope.data?.orgId, scope.data?.environmentId],
    queryFn: async () => (await views.get(id)) ?? null,
    enabled: cellAvailable && id !== "",
    retry: false,
  })
  const [removed, setRemoved] = useState<RemovedView>()
  const invalidate = () => {
    void client.invalidateQueries({ queryKey: ["views"] })
    void client.invalidateQueries({ queryKey: ["view", id] })
  }

  const duplicate = useMutation({
    mutationFn: async (from: StoredView) => {
      const copy = duplicateView(from.view)
      const nextId = newViewId(copy.objectType, copy.title)
      settled(await views.save(nextId, copy))
      return nextId
    },
    onSuccess: (copied) => {
      void client.invalidateQueries({ queryKey: ["views"] })
      void navigate({ to: "/views/$id", params: { id: copied } })
    },
  })

  const remove = useMutation({
    mutationFn: async (from: StoredView) => {
      const retired = settled(await views.retire(from.id, from.version))
      return retired.saved ? { id: from.id, version: retired.version, view: from.view } : undefined
    },
    onSuccess: (gone) => { setRemoved(gone); void client.invalidateQueries({ queryKey: ["views"] }) },
  })

  const undo = useMutation({
    mutationFn: async (gone: RemovedView) => settled(await views.save(gone.id, gone.view, gone.version)),
    onSuccess: () => { setRemoved(undefined); invalidate(); void stored.refetch() },
  })

  const rename = async (from: StoredView, title: string): Promise<string | null> => {
    try {
      const res = await views.save(from.id, { ...from.view, title }, from.version)
      if (!res.saved) return "conflict" in res ? "Someone changed this view; reload and try again." : res.findings.map((f) => f.message).join(" ")
      invalidate()
      return null
    } catch (error) {
      return failure(error)
    }
  }

  const active = removed ? undefined : stored.data
  const concept = active ? catalog?.byName.get(active.view.objectType) : undefined
  const timeline = catalog !== undefined && concept !== undefined && offersTimeline(catalog, concept)

  return (
    <section className="space-y-6">
      <div className="flex flex-wrap items-start gap-2">
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="text-title font-semibold tracking-tight">{active?.view.title || "View"}</h1>
            {active?.view.space && <Badge variant="outline">Space: {active.view.space}</Badge>}
            {active && <Badge variant="secondary">{concept?.title ?? active.view.objectType}</Badge>}
            {active && <Badge variant="outline">Version {active.version}</Badge>}
          </div>
          {active?.view.description && <p className="text-sm text-muted-foreground">{active.view.description}</p>}
        </div>
        {active && (
          <div className="flex flex-wrap items-center gap-1">
            <Button asChild variant="outline" size="sm">
              <Link to="/automations/new" search={{ objectType: active.view.objectType } as never}><Zap />Automate</Link>
            </Button>
            <Button asChild size="sm"><Link to="/views/$id/edit" params={{ id: active.id }}><Pencil />Edit</Link></Button>
            <RenamePopover current={active.view.title} onRename={(title) => rename(active, title)} />
            <Button variant="ghost" size="sm" disabled={duplicate.isPending} onClick={() => duplicate.mutate(active)}><Copy />Duplicate</Button>
            <Button variant="ghost" size="sm" disabled={remove.isPending} onClick={() => remove.mutate(active)}><Trash2 />Delete</Button>
          </div>
        )}
      </div>

      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {duplicate.isError && <p role="alert" className="text-destructive">Not duplicated. {failure(duplicate.error)}</p>}
      {remove.isError && <p role="alert" className="text-destructive">Not deleted. {failure(remove.error)}</p>}
      {(scope.isPending || (cellAvailable && stored.isPending)) && <p className="text-muted-foreground">Reading view…</p>}
      {stored.isError && <EmptyState icon={Bookmark}>{failure(stored.error)}</EmptyState>}
      {removed && <UndoDeleteView removed={removed} pending={undo.isPending} error={undo.isError ? failure(undo.error) : undefined} onUndo={() => undo.mutate(removed)} />}
      {removed && <Button asChild variant="ghost" size="sm"><Link to="/views">Back to views</Link></Button>}
      {stored.isSuccess && active === null && <EmptyState icon={Bookmark}>No view {id} is saved.</EmptyState>}
      {active && concept === undefined && catalog && (
        <EmptyState icon={Shapes}>Record type {active.view.objectType} is not installed in this cell.</EmptyState>
      )}
      {active && concept !== undefined && (
        <RecordsGrid
          key={`${active.id}:${active.version}`}
          concept={concept}
          initial={openedQuery(active)}
          rowActions={active.view.rowActions}
          bulkActions={active.view.bulkActions}
          timeline={timeline}
        />
      )}
    </section>
  )
}

function defaultViewFor(concept: Concept | undefined): SavedView {
  const cols = (concept?.fields ?? []).filter(({ id }) => !HIDDEN_FIELDS.has(id)).slice(0, 6).map(({ id }) => id)
  return {
    title: "",
    description: "",
    space: DEFAULT_SPACES[0],
    objectType: concept?.name ?? "",
    columns: cols,
    rowActions: [],
    bulkActions: [],
  }
}

function ViewEditor({ concepts, stored, cellAvailable, onSaved, onReload }: {
  readonly concepts: readonly Concept[]
  readonly stored?: StoredView
  readonly cellAvailable: boolean
  readonly onSaved: (saved: { readonly id: string; readonly version: number }) => void
  readonly onReload?: () => void
}) {
  const [at, setAt] = useState(stored ? { id: stored.id, version: stored.version } : undefined)
  const [findings, setFindings] = useState<readonly Finding[]>([])
  const [restatement, setRestatement] = useState<string | null>(null)
  const [conflict, setConflict] = useState(false)
  const initial = useMemo(() => stored?.view ?? defaultViewFor(concepts[0]), [stored, concepts])
  const saving = useRef(false)
  const [pendingNav, setPendingNav] = useState<{ readonly id: string; readonly version: number } | null>(null)
  const save = useMutation({
    mutationFn: async ({ view, overwrite }: { readonly view: SavedView; readonly overwrite?: boolean }) => {
      const id = at?.id ?? newViewId(view.objectType, view.title)
      return { view, result: await views.save(id, view, overwrite ? undefined : at?.version) }
    },
    onSuccess: ({ view, result }) => {
      setConflict("conflict" in result)
      setFindings("findings" in result ? result.findings : [])
      if (result.saved) {
        setAt({ id: result.id, version: result.version })
        editor.markSaved(view)
        setPendingNav({ id: result.id, version: result.version })
      }
    },
  })
  const requestSave = (next: SavedView, overwrite = false) => {
    if (!cellAvailable || saving.current || (!overwrite && !editor.dirty)) return
    saving.current = true
    save.mutate({ view: next, overwrite }, { onSettled: () => { saving.current = false } })
  }
  const editor = useEditor<SavedView>({ initial, onSave: (v) => requestSave(v) })
  const { value: view, set } = editor
  // Navigate only after React commits `editor.dirty === false` so DirtyGuard never blocks a completed save.
  useEffect(() => {
    if (pendingNav !== null && !editor.dirty) {
      const next = pendingNav
      setPendingNav(null)
      onSaved(next)
    }
  }, [pendingNav, editor.dirty, onSaved])

  const concept = concepts.find(({ name }) => name === view.objectType) ?? concepts[0]
  const fields = useMemo(() => (concept?.fields ?? []).filter(({ id }) => !HIDDEN_FIELDS.has(id)), [concept])
  const sortableFields = useMemo(() => fields.filter(sortable), [fields])
  const actionsQuery = useActions(view.objectType, undefined, cellAvailable && view.objectType !== "")
  const offeredActions = actionsQuery.data?.kind === "catalog" ? actionsQuery.data.items : []

  const [draftFilter, setDraftFilter] = useState<readonly DraftClause[]>(() => fromPredicate(view.filter ?? null))
  const lastEmittedFilter = useRef<SavedView["filter"]>(view.filter)
  const prevObjectType = useRef(view.objectType)
  useEffect(() => {
    if (prevObjectType.current !== view.objectType) {
      prevObjectType.current = view.objectType
      lastEmittedFilter.current = view.filter
      setDraftFilter(fromPredicate(view.filter ?? null))
      return
    }
    if (view.filter === lastEmittedFilter.current) return
    lastEmittedFilter.current = view.filter
    setDraftFilter(fromPredicate(view.filter ?? null))
  }, [view.objectType, view.filter])

  const emitFilter = (next: readonly DraftClause[]) => {
    const predicate = toPredicate(next, fields) ?? undefined
    lastEmittedFilter.current = predicate
    set((cur) => ({ ...cur, filter: predicate }))
  }

  const [spaceMode, setSpaceMode] = useState<"preset" | "custom">(() =>
    !view.space || (DEFAULT_SPACES as readonly string[]).includes(view.space) ? "preset" : "custom",
  )

  // Live server admission check (side-effect free).
  useEffect(() => {
    if (!cellAvailable || view.title.trim() === "" || view.objectType === "") {
      setFindings([])
      setRestatement(null)
      return
    }
    let cancelled = false
    const timer = setTimeout(() => {
      const id = at?.id ?? newViewId(view.objectType, view.title, () => "check")
      views.admit(id, view).then(
        (admission) => {
          if (cancelled) return
          if (admission.admitted) {
            setFindings([])
            setRestatement(admission.restatement)
          } else {
            setFindings(admission.findings)
            setRestatement(null)
          }
        },
        () => {},
      )
    }, 350)
    return () => { cancelled = true; clearTimeout(timer) }
  }, [cellAvailable, at?.id, view])

  const sort = sortKeys(view.sort)
  const columns = view.columns ?? fields.slice(0, 6).map(({ id }) => id)
  const rowActions = view.rowActions ?? []
  const bulkActions = view.bulkActions ?? []

  // Live bounded preview of matching records.
  const previewQuery = useQuery({
    queryKey: ["view-preview", view.objectType, JSON.stringify(view.filter ?? null), view.sort ?? ""],
    queryFn: () => readRecords(view.objectType, { filter: view.filter ?? null, sort, limit: 5 }),
    enabled: cellAvailable && view.objectType !== "",
    retry: false,
  })
  const previewPage = previewQuery.data?.kind === "page" ? previewQuery.data : undefined

  const issues = issuesOf(findings)
  const invalid = (field: string) => issues.some((i) => i.field === field)
  const selectCls = "h-9 rounded-md border bg-background px-2.5 text-sm"

  const pickType = (nextType: string) => {
    const nextConcept = concepts.find(({ name }) => name === nextType)
    const nextCols = (nextConcept?.fields ?? []).filter(({ id }) => !HIDDEN_FIELDS.has(id)).slice(0, 6).map(({ id }) => id)
    setDraftFilter([])
    set((cur) => ({ ...cur, objectType: nextType, filter: undefined, sort: undefined, columns: nextCols, rowActions: [], bulkActions: [] }))
  }

  const toggleColumn = (fieldId: string, checked: boolean) => {
    const next = checked ? (columns.includes(fieldId) ? columns : [...columns, fieldId]) : columns.filter((c) => c !== fieldId)
    if (next.length > 0) set((cur) => ({ ...cur, columns: next }))
  }

  const moveColumn = (index: number, delta: number) => {
    const target = index + delta
    if (target < 0 || target >= columns.length) return
    const next = [...columns]
    const [moved] = next.splice(index, 1)
    next.splice(target, 0, moved!)
    set((cur) => ({ ...cur, columns: next }))
  }

  const toggleAction = (kind: "rowActions" | "bulkActions", actionId: string, checked: boolean) => {
    const list = kind === "rowActions" ? rowActions : bulkActions
    const next = checked ? (list.includes(actionId) ? list : [...list, actionId]) : list.filter((a) => a !== actionId)
    set((cur) => ({ ...cur, [kind]: next }))
  }

  const setSort = (next: readonly SortKey[]) => set((cur) => ({ ...cur, sort: sortText(next) }))

  return (
    <div className="space-y-6">
      <DirtyGuard dirty={editor.dirty} />
      <EditorToolbar editor={editor} onSave={() => requestSave(view)} saving={save.isPending || !cellAvailable}>
        {at && <Badge variant="outline">Version {at.version}</Badge>}
      </EditorToolbar>
      {save.isError && <p role="alert" className="text-destructive">Not saved. {failure(save.error)}</p>}
      {conflict && (
        <div role="alert" className="flex flex-wrap items-center gap-2 rounded-xl border border-destructive/40 bg-card p-3 text-sm">
          <span>Someone saved a newer version of this view since you opened it.</span>
          <Button type="button" size="sm" variant="outline" onClick={() => onReload?.()}>Reload theirs</Button>
          <Button type="button" size="sm" variant="destructive" disabled={!cellAvailable} onClick={() => requestSave(view, true)}>Overwrite with mine</Button>
        </div>
      )}
      <ValidationPanel issues={issues} title="The cell refused this view" />
      {restatement && (
        <p role="status" className="rounded-lg border border-node-border bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
          {restatement}
        </p>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        <label className="grid gap-1.5 text-sm" data-field="title">
          <span className={invalid("title") ? "font-medium text-destructive" : "font-medium"}>View title</span>
          <Input
            aria-label="View title"
            aria-invalid={invalid("title") || undefined}
            placeholder="e.g. Items needing review"
            value={view.title}
            onChange={(e) => set((cur) => ({ ...cur, title: e.target.value }))}
          />
        </label>

        <label className="grid gap-1.5 text-sm" data-field="objectType">
          <span className={invalid("objectType") ? "font-medium text-destructive" : "font-medium"}>Record type</span>
          <select
            aria-label="Record type"
            className={selectCls}
            value={view.objectType}
            onChange={(e) => pickType(e.target.value)}
          >
            {concepts.map((c) => <option key={c.name} value={c.name}>{c.title} ({c.name})</option>)}
          </select>
        </label>

        <label className="grid gap-1.5 text-sm" data-field="description">
          <span className="font-medium">Description <span className="font-normal text-muted-foreground">(optional)</span></span>
          <Input
            aria-label="Description"
            placeholder="What this view helps an operator act on"
            value={view.description ?? ""}
            onChange={(e) => set((cur) => ({ ...cur, description: e.target.value }))}
          />
        </label>

        <div className="grid gap-1.5 text-sm" data-field="space">
          <span className="font-medium">Pin to Space</span>
          <div className="flex flex-wrap gap-2">
            <select
              aria-label="Space"
              className={`${selectCls} min-w-44 flex-1`}
              value={spaceMode === "custom" ? "__custom__" : (view.space ?? "")}
              onChange={(e) => {
                if (e.target.value === "__custom__") {
                  setSpaceMode("custom")
                  if (!view.space || (DEFAULT_SPACES as readonly string[]).includes(view.space)) {
                    set((cur) => ({ ...cur, space: "" }))
                  }
                } else {
                  setSpaceMode("preset")
                  set((cur) => ({ ...cur, space: e.target.value || undefined }))
                }
              }}
            >
              {DEFAULT_SPACES.map((sp) => <option key={sp} value={sp}>{sp}</option>)}
              <option value="">Unpinned (Views list only)</option>
              <option value="__custom__">Custom space…</option>
            </select>
            {spaceMode === "custom" && (
              <Input
                aria-label="Custom space name"
                placeholder="Space name"
                className="min-w-40 flex-1"
                value={view.space ?? ""}
                onChange={(e) => set((cur) => ({ ...cur, space: e.target.value }))}
              />
            )}
          </div>
        </div>
      </div>

      <div data-field="filter">
      <Panel label="Filters">
        <div className="border-t px-4 py-3">
        <FilterEditor
          fields={fields}
          draft={draftFilter}
          onDraft={(next) => {
            setDraftFilter(next)
            emitFilter(next)
          }}
          onApply={() => emitFilter(draftFilter)}
          onClear={() => {
            setDraftFilter([])
            lastEmittedFilter.current = undefined
            set((cur) => ({ ...cur, filter: undefined }))
          }}
          applied={draftFilter}
          diagnostics={[]}
        />
        </div>
      </Panel>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <div data-field="columns">
        <Panel label="Columns & order">
          <div className="space-y-3 border-t px-4 py-3">
            <div className="flex flex-wrap gap-3">
              {fields.map((field) => (
                <label key={field.id} className="inline-flex items-center gap-1.5 text-sm">
                  <input
                    type="checkbox"
                    checked={columns.includes(field.id)}
                    disabled={columns.includes(field.id) && columns.length === 1}
                    onChange={(e) => toggleColumn(field.id, e.target.checked)}
                  />
                  <span>{field.id.replaceAll("_", " ")}</span>
                </label>
              ))}
            </div>
            {columns.length > 1 && (
              <ol className="divide-y rounded-lg border text-sm" aria-label="Column order">
                {columns.map((col, idx) => (
                  <li key={col} className="flex items-center justify-between px-2.5 py-1">
                    <span>{col.replaceAll("_", " ")}</span>
                    <span className="flex items-center gap-0.5">
                      <Button type="button" variant="ghost" size="icon" className="size-7" aria-label={`Move column ${col} up`} disabled={idx === 0} onClick={() => moveColumn(idx, -1)}><ArrowUp className="size-3.5" /></Button>
                      <Button type="button" variant="ghost" size="icon" className="size-7" aria-label={`Move column ${col} down`} disabled={idx === columns.length - 1} onClick={() => moveColumn(idx, 1)}><ArrowDown className="size-3.5" /></Button>
                    </span>
                  </li>
                ))}
              </ol>
            )}
          </div>
        </Panel>
        </div>

        <div data-field="sort">
        <Panel label="Sort order">
          <div className="space-y-2 border-t px-4 py-3">
            {sort.length === 0 && <p className="text-sm text-muted-foreground">Default record order.</p>}
            {sort.map((key, index) => (
              <div key={`${key.property}:${index}`} className="flex flex-wrap items-center gap-2">
                <select
                  aria-label={`Sort ${index + 1} property`}
                  className={selectCls}
                  value={key.property}
                  onChange={(e) => setSort(sort.map((k, i) => i === index ? { ...k, property: e.target.value } : k))}
                >
                  {sortableFields.map((f) => <option key={f.id} value={f.id}>{f.id.replaceAll("_", " ")}</option>)}
                </select>
                <select
                  aria-label={`Sort ${index + 1} direction`}
                  className={selectCls}
                  value={key.direction}
                  onChange={(e) => setSort(sort.map((k, i) => i === index ? { ...k, direction: e.target.value as "asc" | "desc" } : k))}
                >
                  <option value="asc">Ascending</option>
                  <option value="desc">Descending</option>
                </select>
                <Button type="button" variant="ghost" size="sm" onClick={() => setSort(sort.filter((_, i) => i !== index))}>Remove</Button>
              </div>
            ))}
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={sortableFields.length === 0 || sort.length >= LIMITS.sortKeys}
              onClick={() => {
                const next = sortableFields.find((f) => !sort.some((k) => k.property === f.id)) ?? sortableFields[0]
                if (next) setSort([...sort, { property: next.id, direction: "asc" }])
              }}
            >
              <Plus />Add sort key
            </Button>
          </div>
        </Panel>
        </div>
      </div>

      <div data-field="rowActions">
      <Panel label="Offered actions">
        <div className="border-t px-4 py-3">
        <p className="mb-3 text-meta text-muted-foreground">
          Choose which server-offered actions appear as inline row buttons or bulk selection buttons. Each button renders only on records where the cell offers that action.
        </p>
        {offeredActions.length === 0 ? (
          <p className="text-sm text-muted-foreground">The cell offers no actions on this record type.</p>
        ) : (
          <div className="grid gap-4 md:grid-cols-2">
            <div className="space-y-2">
              <h3 className="text-sm font-medium">Row action buttons</h3>
              {offeredActions.map((action) => (
                <label key={`row:${action.actionId}`} className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    aria-label={`Row action ${action.title}`}
                    checked={rowActions.includes(action.actionId)}
                    onChange={(e) => toggleAction("rowActions", action.actionId, e.target.checked)}
                  />
                  <span>{action.title}</span>
                  <span className="font-mono text-xs text-muted-foreground">{action.actionId}</span>
                </label>
              ))}
            </div>
            <div className="space-y-2" data-field="bulkActions">
              <h3 className="text-sm font-medium">Bulk action buttons</h3>
              {offeredActions.map((action) => (
                <label key={`bulk:${action.actionId}`} className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    aria-label={`Bulk action ${action.title}`}
                    checked={bulkActions.includes(action.actionId)}
                    onChange={(e) => toggleAction("bulkActions", action.actionId, e.target.checked)}
                  />
                  <span>{action.title}</span>
                  <span className="font-mono text-xs text-muted-foreground">{action.actionId}</span>
                </label>
              ))}
            </div>
          </div>
        )}
        </div>
      </Panel>
      </div>

      <Panel label={`Live preview${previewPage?.total ? ` · ${totalText(previewPage.total)}` : ""}`}>
        <div className="border-t px-4 py-3">
        {previewQuery.isPending && <p className="text-sm text-muted-foreground">Checking matching records…</p>}
        {previewPage && previewPage.read.document.records.length === 0 && (
          <p className="text-sm text-muted-foreground">No records currently match this filter.</p>
        )}
        {previewPage && previewPage.read.document.records.length > 0 && (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b text-start text-xs text-muted-foreground">
                  <th className="py-1.5 pe-3 text-start font-medium">Record</th>
                  {columns.slice(0, 5).map((col) => <th key={col} className="px-3 py-1.5 text-start font-medium">{col.replaceAll("_", " ")}</th>)}
                </tr>
              </thead>
              <tbody className="divide-y">
                {previewPage.read.document.records.map((rec) => (
                  <tr key={rec.id}>
                    <td className="py-1.5 pe-3 font-medium">{recordName(rec)}</td>
                    {columns.slice(0, 5).map((col) => (
                      <td key={col} className="px-3 py-1.5 text-muted-foreground">{String(rec.values[col] ?? "—")}</td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        </div>
      </Panel>
    </div>
  )
}

/** `/views/new` and `/views/$id/edit`: build or update a server-admitted view. */
export function ViewEditorPage() {
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const { id } = useParams({ strict: false }) as { id?: string }
  const { read, catalog } = useCatalog()
  const navigate = useNavigate()
  const client = useQueryClient()
  const concepts = useMemo(() => (catalog?.concepts ?? []).filter((c) => c.definition === "object type"), [catalog])
  const stored = useQuery({
    queryKey: ["view-edit", id, scope.data?.orgId, scope.data?.environmentId],
    queryFn: async () => (id === undefined ? null : (await views.get(id)) ?? null),
    enabled: id !== undefined && cellAvailable,
    retry: false,
    staleTime: Infinity,
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })
  const [opened, setOpened] = useState(0)
  const fresh = id === undefined

  return (
    <section className="space-y-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="text-title font-semibold tracking-tight">{fresh ? "New view" : (stored.data?.view.title || "Edit view")}</h1>
        {id !== undefined && (
          <Button asChild variant="outline" size="sm" className="ms-auto">
            <Link to="/views/$id" params={{ id }}><Eye />Open view</Link>
          </Button>
        )}
      </div>
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {(scope.isPending || read.isPending || (!fresh && cellAvailable && stored.isPending)) && <p className="text-muted-foreground">Reading view…</p>}
      {read.isError && <EmptyState icon={Bookmark}>{read.error.message}</EmptyState>}
      {stored.isError && <EmptyState icon={Bookmark}>{failure(stored.error)}</EmptyState>}
      {!fresh && stored.isSuccess && stored.data === null && <EmptyState icon={Bookmark}>No view {id} is saved.</EmptyState>}
      {!scope.isPending && concepts.length === 0 && catalog && (
        <EmptyState icon={Shapes}>No record types are installed in this cell yet.</EmptyState>
      )}
      {!scope.isPending && concepts.length > 0 && (fresh || (stored.data && stored.isFetchedAfterMount)) && (
        <ViewEditor
          key={`${scope.data?.orgId ?? "none"}:${scope.data?.environmentId ?? "none"}:${stored.data?.id ?? "new"}:${opened}`}
          concepts={concepts}
          stored={stored.data ?? undefined}
          cellAvailable={cellAvailable}
          onReload={() => void stored.refetch().then((r) => { if (r.isSuccess && r.data) setOpened((n) => n + 1) })}
          onSaved={(saved) => {
            void client.invalidateQueries({ queryKey: ["views"] })
            void client.invalidateQueries({ queryKey: ["view", saved.id] })
            void navigate({ to: "/views/$id", params: { id: saved.id }, replace: fresh })
          }}
        />
      )}
    </section>
  )
}

export { cleanView }
