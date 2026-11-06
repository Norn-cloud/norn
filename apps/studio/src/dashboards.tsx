import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate, useParams } from "@tanstack/react-router"
import { ChartLine, Copy, LayoutDashboard, Pencil, Plus, Table2, Trash2 } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"
import { DirtyGuard, EditorToolbar, RowActions, ValidationPanel, useEditor } from "@/components/editor"
import { EmptyState, Panel } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { list, type Issue } from "@/lib/editor"
import { dashboardField, dashboardId, duplicateDashboard, saveDashboard, useDefinitions, type Dashboard, type DefinitionStore, type DashboardSummary, type Finding, type Installed, type StoredDashboard, type Tile } from "@/lib/definitions"
import { QuestionBuilder, draftOf, readCatalog, type QuestionCatalog } from "./builder.tsx"

const EMPTY: Dashboard = { title: "", tiles: [] }
const VIEWS = [["chart:line", "Line chart"], ["chart:area", "Area chart"], ["chart:bar", "Bar chart"], ["table", "Table"]] as const
const viewKey = ({ kind, chart }: Tile["view"]) => kind === "table" ? "table" : `chart:${chart ?? "line"}`
const viewOf = (key: string): Tile["view"] => key === "table" ? { kind: "table" } : { kind: "chart", chart: key.slice("chart:".length) as "line" | "bar" | "area" }
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

/** Every saved dashboard, with open, duplicate and delete. */
export function DashboardsPage() {
  const store = useDefinitions()
  const client = useQueryClient()
  const navigate = useNavigate()
  const dashboards = useInfiniteQuery({ queryKey: ["dashboards"], queryFn: ({ pageParam }) => store.list(pageParam), initialPageParam: null as string | null, getNextPageParam: (page) => page.cursor, retry: false })
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
  const remove = useMutation({ mutationFn: async (summary: DashboardSummary) => settled(await store.retire(summary.id, summary.version)), onSuccess: refresh })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Dashboards</h1>
        <p className="max-w-2xl text-muted-foreground">Saved sets of questions. Each is a definition the cell checks before it is kept.</p>
        <Button size="sm" onClick={() => navigate({ to: "/dashboards/new" })}><Plus />New dashboard</Button>
      </div>
      {dashboards.isPending && <p className="text-muted-foreground">Reading dashboards…</p>}
      {dashboards.isError && <EmptyState icon={LayoutDashboard}>{failure(dashboards.error)}</EmptyState>}
      {(duplicate.isError || remove.isError) && <p role="alert" className="text-destructive">{failure(duplicate.error ?? remove.error)}</p>}
      {rows?.length === 0 && <EmptyState icon={LayoutDashboard}>No dashboards saved yet.</EmptyState>}
      {rows?.map((stored) => (
        <Panel key={stored.id} icon={LayoutDashboard} label={stored.title || "Untitled dashboard"} action={
          <div className="flex items-center gap-1">
            <Button asChild variant="ghost" size="sm"><Link to="/dashboards/$id" params={{ id: stored.id }}><Pencil />Open</Link></Button>
            <Button variant="ghost" size="sm" onClick={() => duplicate.mutate(stored)}><Copy />Duplicate</Button>
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

function TileCard({ tile, index, count, invalid, onChange, onEdit, onMove, onDuplicate, onRemove }: {
  readonly tile: Tile; readonly index: number; readonly count: number; readonly invalid: boolean
  readonly onChange: (tile: Tile) => void; readonly onEdit: () => void; readonly onMove: (to: number) => void; readonly onDuplicate: () => void; readonly onRemove: () => void
}) {
  return (
    <div data-field={`tile.${index}`} data-invalid={invalid || undefined} className={`space-y-3 rounded-xl border bg-card p-3 ${tile.layout.w === 2 ? "lg:col-span-2" : ""} ${invalid ? "border-destructive" : ""}`}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2 font-medium">{tile.view.kind === "table" ? <Table2 className="size-4" /> : <ChartLine className="size-4" />}{questionTitle(tile)}</div>
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
      <p className="rounded-lg bg-muted px-3 py-6 text-center text-sm text-muted-foreground">Not run yet: the evaluate route isn't connected.</p>
    </div>
  )
}

/** One dashboard: title and tiles, each an admitted question with its view. The cell admits and keeps it. */
export function DashboardEditor({ store, catalog, stored, guard, onSaved }: {
  readonly store: DefinitionStore; readonly catalog: QuestionCatalog; readonly stored?: StoredDashboard
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
      return { dashboard, result: await saveDashboard(store, id, dashboard, overwrite ? undefined : at?.version) }
    },
    onSuccess: ({ dashboard, result }) => {
      setConflict("conflict" in result)
      setFindings("findings" in result ? result.findings : [])
      if ("saved" in result) { setAt(result.saved); editor.markSaved(dashboard); onSaved?.(result.saved) }
    },
  })
  const editor = useEditor<Dashboard>({ initial: stored?.dashboard ?? EMPTY, onSave: (dashboard) => save.mutate({ dashboard }) })
  const { value: dashboard, set } = editor
  // Live validation: the cell's admit is side-effect free, so it runs on every edit (after a pause).
  useEffect(() => {
    if (dashboard.title.trim() === "" && dashboard.tiles.length === 0) return
    const timer = setTimeout(() => {
      store.admit(at?.id ?? dashboardId(dashboard.title), dashboard).then((admission) => setFindings(admission.admitted ? [] : admission.findings), () => {})
    }, 400)
    return () => clearTimeout(timer)
  }, [store, at?.id, dashboard])
  const tiles = (next: readonly Tile[]) => set((current) => ({ ...current, tiles: layoutTiles(next) }))
  const issues = issuesOfFindings(findings)
  const invalid = (field: string) => issues.some((issue) => issue.field === field)
  const use = (question: Tile["question"], title: string) => {
    if (editing === "new") tiles([...dashboard.tiles, { id: crypto.randomUUID(), title, question, view: { kind: "chart", chart: "line" }, layout: { x: 0, y: 0, w: 2, h: 1 } }])
    else if (editing !== undefined) tiles(dashboard.tiles.map((tile, index) => index === editing ? { ...tile, title, question } : tile))
    setEditing(undefined)
  }
  const open = typeof editing === "number" ? dashboard.tiles[editing] : undefined
  return (
    <div className="space-y-4">
      {guard?.(editor.dirty)}
      <EditorToolbar editor={editor} onSave={() => save.mutate({ dashboard })} saving={save.isPending}>
        {at && <Badge variant="outline">Version {at.version}</Badge>}
      </EditorToolbar>
      {save.isError && <p role="alert" className="text-destructive">{failure(save.error).startsWith("Not saved") ? failure(save.error) : `Not saved. ${failure(save.error)}`}</p>}
      {conflict && (
        <div role="alert" className="flex flex-wrap items-center gap-2 rounded-xl border border-destructive/40 bg-card p-3 text-sm">
          <span>Someone saved a newer version of this dashboard since you opened it.</span>
          <Button type="button" size="sm" variant="outline" onClick={() => onReload?.()}>Reload theirs</Button>
          <Button type="button" size="sm" variant="destructive" onClick={() => save.mutate({ dashboard, overwrite: true })}>Overwrite with mine</Button>
        </div>
      )}
      <ValidationPanel issues={issues} title="The cell refused this dashboard" />
      <label className="grid max-w-xl gap-1.5 text-sm" data-field="title"><span className={invalid("title") ? "text-destructive" : "text-muted-foreground"}>Title</span>
        <Input value={dashboard.title} aria-invalid={invalid("title") || undefined} placeholder="e.g. Refinery weekly" onChange={(event) => set((current) => ({ ...current, title: event.target.value }))} />
      </label>
      <div data-field="tiles" className="grid gap-4 lg:grid-cols-2">
        {dashboard.tiles.length === 0 && <div className="lg:col-span-2"><EmptyState icon={LayoutDashboard}>No questions on this dashboard yet.</EmptyState></div>}
        {dashboard.tiles.map((tile, index) => (
          <TileCard key={tile.id} tile={tile} index={index} count={dashboard.tiles.length} invalid={invalid(`tile.${index}`)}
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
          <div className="p-4">{editing !== undefined && <QuestionBuilder key={String(editing)} catalog={catalog} initial={open ? draftOf(open.question, open.title) : undefined} onUse={use} />}</div>
        </SheetContent>
      </Sheet>
    </div>
  )
}

export function DashboardEditorPage() {
  const { id } = useParams({ strict: false }) as { id?: string }
  const store = useDefinitions()
  const navigate = useNavigate()
  const catalog = useQuery({ queryKey: ["question-catalog"], queryFn: readCatalog, retry: false, staleTime: Infinity })
  const stored = useQuery({ queryKey: ["dashboard", id], queryFn: () => id === undefined ? Promise.resolve(undefined) : store.get(id), retry: false })
  const [title, setTitle] = useState(id === undefined ? "New dashboard" : "Dashboard")
  useEffect(() => { if (stored.data) setTitle(stored.data.dashboard.title || "Untitled dashboard") }, [stored.data])
  return (
    <section className="space-y-6">
      <h1 className="text-title font-medium">{title}</h1>
      {(catalog.isPending || stored.isPending) && <p className="text-muted-foreground">Reading the dashboard…</p>}
      {catalog.isError && <EmptyState icon={LayoutDashboard}>{catalog.error.message}</EmptyState>}
      {stored.isError && <EmptyState icon={LayoutDashboard}>{failure(stored.error)}</EmptyState>}
      {id !== undefined && stored.isSuccess && stored.data === undefined && <EmptyState icon={LayoutDashboard}>No dashboard {id} is saved.</EmptyState>}
      {catalog.data && stored.isSuccess && (id === undefined || stored.data) && (
        <DashboardEditor key={`${stored.data?.id ?? "new"}:${stored.data?.version ?? 0}`} store={store} catalog={catalog.data} stored={stored.data} guard={(dirty) => <DirtyGuard dirty={dirty} />}
          onReload={() => void stored.refetch()}
          onSaved={(saved) => { if (id === undefined) setTimeout(() => void navigate({ to: "/dashboards/$id", params: { id: saved.id }, replace: true })) }} />
      )}
    </section>
  )
}
