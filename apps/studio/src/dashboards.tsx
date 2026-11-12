import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate, useParams } from "@tanstack/react-router"
import { Copy, Eye, LayoutDashboard, Pencil, Plus, Sparkles, Trash2, Undo2 } from "lucide-react"
import { useEffect, useRef, useState, type ReactNode } from "react"
import { DirtyGuard, EditorToolbar, RowActions, ValidationPanel, useEditor } from "@/components/editor"
import { EmptyState, Panel } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { list, type Issue } from "@/lib/editor"
import { dashboardField, dashboardId, duplicateDashboard, saveDashboard, useDefinitions, useStudioScope, NO_DATA_CELL, type Dashboard, type DefinitionStore, type DashboardSummary, type Finding, type Installed, type StoredDashboard, type Tile } from "@/lib/definitions"
import { QuestionBuilder, UNEDITABLE, draftOf, questionFitsDraft, readCatalog, type QuestionCatalog } from "./builder.tsx"
import { TileAnswer, VIEWS, ViewIcon, defaultView, viewIcon, viewKey, viewOf } from "./dashboards/tile-view.tsx"

const EMPTY: Dashboard = { title: "", tiles: [] }
const questionTitle = (tile: Tile) => tile.title || "Untitled question"

/** Tiles laid out in reading order, two columns: a tile is one or two columns wide. */
export function layoutTiles(tiles: readonly Tile[]): readonly Tile[] {
  let x = 0
  let y = 0
  return tiles.map((tile) => {
    const w = Math.min(Math.max(tile.layout.w, 1), 2)
    if (x + w > 2) { x = 0; y += 1 }
    const placed = { ...tile, layout: { x, y, w, h: 1 } }
    x += w
    if (x >= 2) { x = 0; y += 1 }
    return placed
  })
}

export const issuesOfFindings = (findings: readonly Finding[]): readonly Issue[] => findings.map(({ code, message, pointer }) => ({ code, message, field: dashboardField(pointer) }))

const failure = (error: unknown) => error instanceof Error ? error.message : String(error)
/** A retire or install the cell refused, as an error the list can show. */
const settled = (result: Installed) => {
  if (result.installed) return result
  throw new Error("conflict" in result ? "Someone changed this dashboard since the list was read; reload and try again." : result.findings.map(({ message }) => message).join(" ") || "The cell refused the change.")
}

/** A deleted dashboard: retiring keeps its history, so undoing installs the same definition over the retired version. */
interface Removed { readonly id: string; readonly version: number; readonly dashboard: Dashboard }

function UndoDelete({ removed, pending, error, onUndo }: { readonly removed: Removed; readonly pending: boolean; readonly error?: string; readonly onUndo: () => void }) {
  return (
    <div role="status" className="flex flex-wrap items-center gap-2 rounded-xl border bg-card p-3 text-sm shadow-elevation">
      <span>Deleted “{removed.dashboard.title || "Untitled dashboard"}”. Its history stays in the cell.</span>
      <Button type="button" size="sm" variant="outline" className="ms-auto" disabled={pending} onClick={onUndo}><Undo2 />{pending ? "Restoring…" : "Undo"}</Button>
      {error && <p role="alert" className="basis-full text-destructive">Not restored. {error}</p>}
    </div>
  )
}

/** Every saved dashboard, with open, duplicate and delete. */
export function DashboardsPage() {
  const store = useDefinitions()
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const client = useQueryClient()
  const navigate = useNavigate()
  const dashboards = useInfiniteQuery({ queryKey: ["dashboards", scope.data?.orgId, scope.data?.environmentId], queryFn: ({ pageParam }) => store.list(pageParam), initialPageParam: null as string | null, getNextPageParam: (page) => page.cursor, enabled: cellAvailable, retry: false })
  const rows = dashboards.data?.pages.flatMap(({ items }) => items)
  const refresh = () => client.invalidateQueries({ queryKey: ["dashboards"] })
  const duplicate = useMutation({
    mutationFn: async (summary: DashboardSummary) => {
      const stored = await store.get(summary.id)
      if (stored === undefined) throw new Error(`"${summary.title}" is no longer saved.`)
      const copy = duplicateDashboard(stored.dashboard)
      return settled(await store.install(dashboardId(copy.title), copy))
    },
    onSuccess: refresh,
  })
  const [removed, setRemoved] = useState<Removed>()
  const remove = useMutation({
    mutationFn: async (summary: DashboardSummary) => {
      const stored = await store.get(summary.id)
      const retired = settled(await store.retire(summary.id, summary.version))
      return stored === undefined || !retired.installed ? undefined : { id: summary.id, version: retired.version, dashboard: stored.dashboard }
    },
    onSuccess: setRemoved,
    onSettled: refresh,
  })
  const undo = useMutation({ mutationFn: async (gone: Removed) => settled(await store.install(gone.id, gone.dashboard, gone.version)), onSuccess: () => setRemoved(undefined), onSettled: refresh })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Dashboards</h1>
        <p className="max-w-2xl text-muted-foreground">Saved sets of questions. Each is a definition the cell checks before it is kept.</p>
        <Button size="sm" disabled={!cellAvailable} onClick={() => navigate({ to: "/dashboards/new" })}><Plus />New dashboard</Button>
      </div>
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {cellAvailable && dashboards.isPending && <p className="text-muted-foreground">Reading dashboards…</p>}
      {dashboards.isError && <EmptyState icon={LayoutDashboard}>{failure(dashboards.error)}</EmptyState>}
      {duplicate.isError && <p role="alert" className="text-destructive">Not duplicated. {failure(duplicate.error)}</p>}
      {remove.isError && <p role="alert" className="text-destructive">Not deleted. {failure(remove.error)}</p>}
      {removed && <UndoDelete removed={removed} pending={undo.isPending} error={undo.isError ? failure(undo.error) : undefined} onUndo={() => undo.mutate(removed)} />}
      {rows?.length === 0 && <EmptyState icon={LayoutDashboard}>No dashboards saved yet.</EmptyState>}
      {rows?.map((stored) => (
        <Panel key={stored.id} icon={LayoutDashboard} label={stored.title || "Untitled dashboard"} action={
          <div className="flex items-center gap-1">
            <Button asChild variant="ghost" size="sm"><Link to="/dashboards/$id" params={{ id: stored.id }}><Eye />Open</Link></Button>
            <Button asChild variant="ghost" size="sm"><Link to="/dashboards/$id/edit" params={{ id: stored.id }}><Pencil />Edit</Link></Button>
            <Button variant="ghost" size="sm" disabled={duplicate.isPending} onClick={() => duplicate.mutate(stored)}><Copy />Duplicate</Button>
            <Button variant="ghost" size="sm" onClick={() => { if (globalThis.confirm(`Delete "${stored.title}"? Its history stays in the cell.`)) remove.mutate(stored) }}><Trash2 />Delete</Button>
          </div>
        }>
          <p className="text-sm text-muted-foreground">Version {stored.version}{stored.updatedAtMs > 0 ? ` · saved ${new Date(stored.updatedAtMs).toLocaleString()}` : ""}</p>
        </Panel>
      ))}
      {dashboards.hasNextPage && <Button variant="outline" size="sm" disabled={dashboards.isFetchingNextPage} onClick={() => void dashboards.fetchNextPage()}>{dashboards.isFetchingNextPage ? "Reading…" : "Show more"}</Button>}
    </section>
  )
}

function TileCard({ tile, index, count, invalid, catalog, onChange, onEdit, onMove, onDuplicate, onRemove }: {
  readonly tile: Tile; readonly index: number; readonly count: number; readonly invalid: boolean; readonly catalog: QuestionCatalog
  readonly onChange: (tile: Tile) => void; readonly onEdit: () => void; readonly onMove: (to: number) => void; readonly onDuplicate: () => void; readonly onRemove: () => void
}) {
  return (
    <div data-field={`tile.${index}`} data-invalid={invalid || undefined} className={`space-y-3 rounded-xl border bg-card p-3 ${tile.layout.w === 2 ? "lg:col-span-2" : ""} ${invalid ? "border-destructive" : ""}`}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2 font-medium"><ViewIcon view={tile.view} />{questionTitle(tile)}</div>
        <RowActions index={index} count={count} label={`tile ${index + 1}`} onDuplicate={onDuplicate} onMove={onMove} onRemove={onRemove} />
      </div>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <select aria-label={`Tile ${index + 1} view`} className="h-8 rounded-3xl bg-input/50 px-3" value={viewKey(tile.view)} onChange={(event) => onChange({ ...tile, view: viewOf(event.target.value) })}>
          {VIEWS.map(([key, text]) => <option key={key} value={key}>{text}</option>)}
        </select>
        <select aria-label={`Tile ${index + 1} width`} className="h-8 rounded-3xl bg-input/50 px-3" value={String(tile.layout.w)} onChange={(event) => onChange({ ...tile, layout: { ...tile.layout, w: Number(event.target.value) } })}>
          <option value="1">Half width</option>
          <option value="2">Full width</option>
        </select>
        <Button type="button" variant="outline" size="sm" onClick={onEdit}><Pencil />Edit question</Button>
      </div>
      <TileAnswer tile={tile} catalog={catalog} />
    </div>
  )
}

/** One dashboard: title and tiles, each an admitted question with its view. The cell admits and keeps it. */
export function DashboardEditor({ store, catalog, stored, guard, onSaved, onReload, cellAvailable = true }: {
  readonly store: DefinitionStore; readonly catalog: QuestionCatalog; readonly stored?: StoredDashboard; readonly cellAvailable?: boolean
  readonly guard?: (dirty: boolean) => ReactNode; readonly onSaved?: (saved: { readonly id: string; readonly version: number }) => void
  /** Reopen the stored version, dropping this one (after a conflict). */
  readonly onReload?: () => void
}) {
  const [at, setAt] = useState(stored ? { id: stored.id, version: stored.version } : undefined)
  const [findings, setFindings] = useState<readonly Finding[]>([])
  const [conflict, setConflict] = useState(false)
  const [editing, setEditing] = useState<number | "new">()
  const save = useMutation({
    mutationFn: async ({ dashboard, overwrite }: { readonly dashboard: Dashboard; readonly overwrite?: boolean }) => {
      const id = at?.id ?? dashboardId(dashboard.title)
      // A version saved here is authored in Studio, whoever proposed the one before it.
      return { dashboard, result: await saveDashboard(store, id, { ...dashboard, origin: { by: "studio" } }, overwrite ? undefined : at?.version) }
    },
    onSuccess: ({ dashboard, result }) => {
      setConflict("conflict" in result)
      setFindings("findings" in result ? result.findings : [])
      if ("saved" in result) { setAt(result.saved); editor.markSaved(dashboard); onSaved?.(result.saved) }
    },
  })
  // While the question sheet is open, its builder owns undo and redo.
  // One save at a time, from the shortcut or the toolbar; an ordinary save of an unchanged dashboard does nothing.
  const saving = useRef(false)
  const requestSave = (next: Dashboard, overwrite = false) => {
    if (!cellAvailable || saving.current || (!overwrite && !editor.dirty)) return
    saving.current = true
    save.mutate({ dashboard: next, overwrite }, { onSettled: () => { saving.current = false } })
  }
  // While the question sheet is open, its builder owns the shortcuts.
  const editor = useEditor<Dashboard>({ initial: stored?.dashboard ?? EMPTY, onSave: (dashboard) => requestSave(dashboard), shortcuts: editing === undefined })
  const { value: dashboard, set } = editor
  // Live validation: the cell's admit is side-effect free, so it runs on every edit (after a pause).
  useEffect(() => {
    if (!cellAvailable || (dashboard.title.trim() === "" && dashboard.tiles.length === 0)) { setFindings([]); return }
    // A response for an edit since superseded must not replace the current findings.
    let cancelled = false
    const timer = setTimeout(() => {
      store.admit(at?.id ?? dashboardId(dashboard.title), dashboard).then((admission) => { if (!cancelled) setFindings(admission.admitted ? [] : admission.findings) }, () => {})
    }, 400)
    return () => { cancelled = true; clearTimeout(timer) }
  }, [store, at?.id, dashboard, cellAvailable])
  const tiles = (next: readonly Tile[]) => set((current) => ({ ...current, tiles: layoutTiles(next) }))
  const issues = issuesOfFindings(findings)
  const invalid = (field: string) => issues.some((issue) => issue.field === field)
  const use = (question: Tile["question"], title: string) => {
    if (editing === "new") {
      const view = defaultView(question as { readonly groupBy?: readonly { readonly grain?: string }[] })
      tiles([...dashboard.tiles, { id: crypto.randomUUID(), title, question, view, layout: { x: 0, y: 0, w: view.kind === "number" ? 1 : 2, h: 1 } }])
    }
    else if (editing !== undefined) tiles(dashboard.tiles.map((tile, index) => index === editing ? { ...tile, title, question } : tile))
    setEditing(undefined)
  }
  const open = typeof editing === "number" ? dashboard.tiles[editing] : undefined
  return (
    <div className="space-y-4">
      {guard?.(editor.dirty)}
      <EditorToolbar editor={editor} onSave={() => requestSave(dashboard)} saving={save.isPending || !cellAvailable}>
        {at && <Badge variant="outline">Version {at.version}</Badge>}
      </EditorToolbar>
      {save.isError && <p role="alert" className="text-destructive">{failure(save.error).startsWith("Not saved") ? failure(save.error) : `Not saved. ${failure(save.error)}`}</p>}
      {conflict && (
        <div role="alert" className="flex flex-wrap items-center gap-2 rounded-xl border border-destructive/40 bg-card p-3 text-sm">
          <span>Someone saved a newer version of this dashboard since you opened it.</span>
          <Button type="button" size="sm" variant="outline" onClick={() => onReload?.()}>Reload theirs</Button>
          <Button type="button" size="sm" variant="destructive" disabled={!cellAvailable} onClick={() => requestSave(dashboard, true)}>Overwrite with mine</Button>
        </div>
      )}
      <ValidationPanel issues={issues} title="The cell refused this dashboard" />
      <label className="grid max-w-xl gap-1.5 text-sm" data-field="title"><span className={invalid("title") ? "text-destructive" : "text-muted-foreground"}>Title</span>
        <Input value={dashboard.title} aria-invalid={invalid("title") || undefined} placeholder="e.g. Refinery weekly" onChange={(event) => set((current) => ({ ...current, title: event.target.value }))} />
      </label>
      <div data-field="tiles" className="grid gap-4 lg:grid-cols-2">
        {dashboard.tiles.length === 0 && <div className="lg:col-span-2"><EmptyState icon={LayoutDashboard}>No questions on this dashboard yet.</EmptyState></div>}
        {dashboard.tiles.map((tile, index) => (
          <TileCard key={tile.id} tile={tile} index={index} count={dashboard.tiles.length} invalid={invalid(`tile.${index}`)} catalog={catalog}
            onChange={(next) => tiles(dashboard.tiles.map((t, at) => at === index ? next : t))}
            onEdit={() => setEditing(index)}
            onMove={(to) => tiles(list.move(dashboard.tiles, index, to))}
            onDuplicate={() => tiles(list.duplicate(dashboard.tiles, index).map((t, at) => at === index + 1 ? { ...t, id: crypto.randomUUID() } : t))}
            onRemove={() => tiles(list.remove(dashboard.tiles, index))} />
        ))}
      </div>
      <Button type="button" variant="outline" size="sm" onClick={() => setEditing("new")}><Plus />Add question</Button>
      <Sheet open={editing !== undefined} onOpenChange={(next) => { if (!next) setEditing(undefined) }}>
        <SheetContent className="w-full overflow-y-auto sm:max-w-4xl">
          <SheetHeader>
            <SheetTitle>{open ? `Edit "${questionTitle(open)}"` : "Add a question"}</SheetTitle>
            <SheetDescription>The server's checker must admit the question before it joins the dashboard.</SheetDescription>
          </SheetHeader>
          <div className="p-4">{editing !== undefined && <QuestionBuilder key={String(editing)} catalog={catalog} initial={open ? draftOf(open.question, open.title) : undefined} unsupported={open && !questionFitsDraft(open.question, catalog) ? UNEDITABLE : undefined} onUse={use} />}</div>
        </SheetContent>
      </Sheet>
    </div>
  )
}

export function DashboardEditorPage() {
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const { id } = useParams({ strict: false }) as { id?: string }
  const store = useDefinitions()
  const navigate = useNavigate()
  const catalog = useQuery({ queryKey: ["question-catalog"], queryFn: readCatalog, retry: false, staleTime: Infinity })
  // A query may not resolve to undefined: a missing dashboard is null, and a new one reads nothing.
  const stored = useQuery({ queryKey: ["dashboard", id, scope.data?.orgId, scope.data?.environmentId], queryFn: async () => (id === undefined ? null : await store.get(id)) ?? null, enabled: id !== undefined && cellAvailable, retry: false,
    // Only an open (and an explicit reload) reads the dashboard: a background refetch must not replace the draft being edited.
    staleTime: Infinity, refetchOnMount: "always", refetchOnWindowFocus: false, refetchOnReconnect: false })
  // Bumped by "Reload theirs": the one other time the editor starts again from the stored version.
  const [opened, setOpened] = useState(0)
  const fresh = id === undefined
  const [title, setTitle] = useState(id === undefined ? "New dashboard" : "Dashboard")
  useEffect(() => { if (stored.data) setTitle(stored.data.dashboard.title || "Untitled dashboard") }, [stored.data])
  return (
    <section className="space-y-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="text-title font-medium">{title}</h1>
        {id !== undefined && <Button asChild variant="outline" size="sm" className="ms-auto"><Link to="/dashboards/$id" params={{ id }}><Eye />View</Link></Button>}
      </div>
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {(scope.isPending || (cellAvailable && (catalog.isPending || (!fresh && stored.isPending)))) && <p className="text-muted-foreground">Reading the dashboard…</p>}
      {catalog.isError && <EmptyState icon={LayoutDashboard}>{catalog.error.message}</EmptyState>}
      {stored.isError && <EmptyState icon={LayoutDashboard}>{failure(stored.error)}</EmptyState>}
      {!fresh && stored.isSuccess && stored.data === null && <EmptyState icon={LayoutDashboard}>No dashboard {id} is saved.</EmptyState>}
      {/* The editor opens once the workspace scope is known, so nothing is typed into an editor its scope would then replace. */}
      {!scope.isPending && catalog.data && (fresh || (stored.data && stored.isFetchedAfterMount)) && (
        <DashboardEditor key={`${scope.data?.orgId ?? "none"}:${scope.data?.environmentId ?? "none"}:${stored.data?.id ?? "new"}:${opened}`} cellAvailable={cellAvailable} store={store} catalog={catalog.data} stored={stored.data ?? undefined} guard={(dirty) => <DirtyGuard dirty={dirty} />}
          // Start again from theirs only once it has been read; a failed or empty read keeps this draft and its conflict banner.
          onReload={() => void stored.refetch().then((read) => { if (read.isSuccess && read.data) setOpened((n) => n + 1) })}
          onSaved={(saved) => { if (id === undefined) setTimeout(() => void navigate({ to: "/dashboards/$id/edit", params: { id: saved.id }, replace: true })) }} />
      )}
    </section>
  )
}

/** A saved dashboard as its readers see it: every tile answered live from the cell and drawn as its view. */
export function DashboardBoard({ dashboard, catalog }: { readonly dashboard: Dashboard; readonly catalog: QuestionCatalog }) {
  const tiles = layoutTiles(dashboard.tiles)
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      {tiles.length === 0 && <div className="lg:col-span-2"><EmptyState icon={LayoutDashboard}>No questions on this dashboard yet.</EmptyState></div>}
      {tiles.map((tile) => (
        <Panel key={tile.id} icon={viewIcon(tile.view)} label={questionTitle(tile)} className={tile.layout.w === 2 ? "lg:col-span-2" : ""}>
          <TileAnswer tile={tile} catalog={catalog} />
        </Panel>
      ))}
    </div>
  )
}

/** One saved dashboard: its tiles drawn, with edit, duplicate and delete. */
export function DashboardBoardPage() {
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const { id } = useParams({ strict: false }) as { id: string }
  const store = useDefinitions()
  const navigate = useNavigate()
  const client = useQueryClient()
  const catalog = useQuery({ queryKey: ["question-catalog"], queryFn: readCatalog, retry: false, staleTime: Infinity })
  const stored = useQuery({ queryKey: ["dashboard-board", id, scope.data?.orgId, scope.data?.environmentId], queryFn: async () => (await store.get(id)) ?? null, enabled: cellAvailable, retry: false })
  const duplicate = useMutation({
    mutationFn: async (from: StoredDashboard) => {
      const copy = duplicateDashboard(from.dashboard)
      const id = dashboardId(copy.title)
      settled(await store.install(id, copy))
      return id
    },
    onSuccess: (copied) => { void client.invalidateQueries({ queryKey: ["dashboards"] }); void navigate({ to: "/dashboards/$id", params: { id: copied } }) },
  })
  const [removed, setRemoved] = useState<Removed>()
  const remove = useMutation({
    mutationFn: async (from: StoredDashboard) => {
      const retired = settled(await store.retire(from.id, from.version))
      return retired.installed ? { id: from.id, version: retired.version, dashboard: from.dashboard } : undefined
    },
    onSuccess: (gone) => { setRemoved(gone); void client.invalidateQueries({ queryKey: ["dashboards"] }) },
  })
  const undo = useMutation({
    mutationFn: async (gone: Removed) => settled(await store.install(gone.id, gone.dashboard, gone.version)),
    onSuccess: () => { setRemoved(undefined); void client.invalidateQueries({ queryKey: ["dashboards"] }); void stored.refetch() },
  })
  const board = removed ? undefined : stored.data
  return (
    <section className="space-y-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="text-title font-medium">{board?.dashboard.title || "Dashboard"}</h1>
        {board && <Badge variant="outline">Version {board.version}</Badge>}
        {board?.dashboard.origin?.by === "assistant" && <Badge variant="secondary" title={board.dashboard.origin.request ? `Asked: ${board.dashboard.origin.request}` : undefined}><Sparkles />Proposed by the assistant</Badge>}
        {board && (
          <div className="ms-auto flex items-center gap-1">
            <Button asChild size="sm"><Link to="/dashboards/$id/edit" params={{ id }}><Pencil />Edit</Link></Button>
            <Button variant="ghost" size="sm" disabled={duplicate.isPending} onClick={() => duplicate.mutate(board)}><Copy />Duplicate</Button>
            <Button variant="ghost" size="sm" disabled={remove.isPending} onClick={() => { if (globalThis.confirm(`Delete "${board.dashboard.title}"? Its history stays in the cell.`)) remove.mutate(board) }}><Trash2 />Delete</Button>
          </div>
        )}
      </div>
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {duplicate.isError && <p role="alert" className="text-destructive">Not duplicated. {failure(duplicate.error)}</p>}
      {remove.isError && <p role="alert" className="text-destructive">Not deleted. {failure(remove.error)}</p>}
      {(scope.isPending || (cellAvailable && (catalog.isPending || stored.isPending))) && <p className="text-muted-foreground">Reading the dashboard…</p>}
      {catalog.isError && <EmptyState icon={LayoutDashboard}>{catalog.error.message}</EmptyState>}
      {stored.isError && <EmptyState icon={LayoutDashboard}>{failure(stored.error)}</EmptyState>}
      {removed && <UndoDelete removed={removed} pending={undo.isPending} error={undo.isError ? failure(undo.error) : undefined} onUndo={() => undo.mutate(removed)} />}
      {removed && <Button asChild variant="ghost" size="sm"><Link to="/dashboards">Back to dashboards</Link></Button>}
      {stored.isSuccess && board === null && <EmptyState icon={LayoutDashboard}>No dashboard {id} is saved.</EmptyState>}
      {board && catalog.data && <DashboardBoard dashboard={board.dashboard} catalog={catalog.data} />}
    </section>
  )
}
