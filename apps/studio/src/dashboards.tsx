import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate, useParams } from "@tanstack/react-router"
import { BadgeCheck, ChartLine, Copy, LayoutDashboard, ListChecks, Pencil, Play, Plus, ShieldCheck, Table2, Trash2, Workflow } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"
import { DirtyGuard, EditorToolbar, RowActions, ValidationPanel, useEditor } from "@/components/editor"
import { EmptyState, Panel } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { list, type Issue } from "@/lib/editor"
import { cn } from "@/lib/utils"
import { KindChip } from "@/components/canvas/node-card"
import { dashboardField, dashboardId, duplicateDashboard, saveDashboard, useDefinitions, useStudioScope, NO_DATA_CELL, type Dashboard, type DefinitionStore, type DashboardSummary, type Finding, type Installed, type Restatement, type StoredDashboard, type Tile } from "@/lib/definitions"
import { questionDefinitions } from "@/lib/question-definitions"
import { buildQuestion, draftOf, readCatalog, type QuestionCatalog } from "./builder.tsx"
import { QuestionTree, readRecommendations, stable } from "./question-tree.tsx"
import { RunQuestion } from "./run.tsx"

const EMPTY: Dashboard = { title: "", tiles: [] }
/** A duplicate on its way to the editor: the list sets it, the new-dashboard page takes it once. */
let pendingCopy: Dashboard | undefined
const VIEWS = [["chart:line", "Line chart"], ["chart:area", "Area chart"], ["chart:bar", "Bar chart"], ["table", "Table"]] as const
const viewKey = ({ kind, chart }: Tile["view"]) => kind === "table" ? "table" : `chart:${chart ?? "line"}`
const viewOf = (key: string): Tile["view"] => key === "table" ? { kind: "table" } : { kind: "chart", chart: key.slice("chart:".length) as "line" | "bar" | "area" }
const questionTitle = (tile: Tile) => tile.title || "Untitled question"

/** Tiles laid out in reading order, two columns: a tile is one or two columns wide and one or two rows tall. */
export function layoutTiles(tiles: readonly Tile[]): readonly Tile[] {
  let x = 0
  let y = 0
  return tiles.map((tile) => {
    const w = Math.min(Math.max(tile.layout.w, 1), 2)
    if (x + w > 2) { x = 0; y += 1 }
    const placed = { ...tile, layout: { x, y, w, h: Math.min(Math.max(tile.layout.h, 1), 2) } }
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
  if ("conflict" in result) throw new Error("Someone changed this dashboard since the list was read; reload and try again.")
  if ("signoff" in result) throw new Error(result.signoff === "unavailable" ? "The cell cannot verify a sign-off yet; nothing was changed." : "The cell reads this dashboard differently from what was signed; open it and sign off again.")
  throw new Error(result.findings.map(({ message }) => message).join(" ") || "The cell refused the change.")
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
  // A copy is a new dashboard: it opens in the editor, to be read in plain words and signed off before it is kept.
  const duplicate = useMutation({
    mutationFn: async (summary: DashboardSummary) => {
      const stored = await store.get(summary.id)
      if (stored === undefined) throw new Error(`"${summary.title}" is no longer saved.`)
      return duplicateDashboard(stored.dashboard)
    },
    onSuccess: (copy) => { pendingCopy = copy; void navigate({ to: "/dashboards/new" }) },
  })
  // Delete retires the definition; Undo installs the same payload again under the sign-off the cell recorded for it.
  const [removed, setRemoved] = useState<{ readonly stored: StoredDashboard; readonly retiredVersion: number }>()
  const remove = useMutation({
    mutationFn: async (summary: DashboardSummary) => {
      const stored = await store.get(summary.id)
      if (stored === undefined) throw new Error(`"${summary.title}" is no longer saved.`)
      const result = settled(await store.retire(summary.id, summary.version))
      return { stored, retiredVersion: result.version }
    },
    onSuccess: (gone) => { setRemoved(gone); void refresh() },
  })
  const restore = useMutation({
    mutationFn: async () => removed && settled(await store.install(removed.stored.id, removed.stored.dashboard, removed.retiredVersion, removed.stored.signoff?.restatementSha256)),
    onSuccess: () => { setRemoved(undefined); void refresh() },
  })
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
      {(duplicate.isError || remove.isError || restore.isError) && <p role="alert" className="text-destructive">{failure(duplicate.error ?? remove.error ?? restore.error)}</p>}
      {removed && (
        <div role="status" className="flex flex-wrap items-center gap-3 rounded-node border border-node-border bg-card px-4 py-3 text-sm shadow-node">
          <span>Deleted "{removed.stored.dashboard.title || "Untitled dashboard"}". Its history stays in the cell.</span>
          <Button type="button" size="sm" variant="outline" disabled={restore.isPending || !cellAvailable} onClick={() => restore.mutate()}>{restore.isPending ? "Restoring…" : "Undo"}</Button>
        </div>
      )}
      {rows?.length === 0 && <EmptyState icon={LayoutDashboard}>No dashboards saved yet.</EmptyState>}
      {rows?.map((stored) => (
        <Panel key={stored.id} className="rounded-node border-node-border shadow-node" icon={LayoutDashboard} label={stored.title || "Untitled dashboard"} action={
          <div className="flex items-center gap-1">
            <Button asChild variant="ghost" size="sm"><Link to="/dashboards/$id" params={{ id: stored.id }}><Pencil />Open</Link></Button>
            <Button variant="ghost" size="sm" disabled={!cellAvailable} onClick={() => duplicate.mutate(stored)}><Copy />Duplicate</Button>
            <Button variant="ghost" size="sm" disabled={!cellAvailable} onClick={() => { if (globalThis.confirm(`Delete "${stored.title}"? Its history stays in the cell.`)) remove.mutate(stored) }}><Trash2 />Delete</Button>
          </div>
        }>
          <p className="text-sm text-muted-foreground">Version {stored.version}{stored.updatedAtMs > 0 ? ` · saved ${new Date(stored.updatedAtMs).toLocaleString()}` : ""}</p>
        </Panel>
      ))}
      {dashboards.hasNextPage && <Button variant="outline" size="sm" disabled={dashboards.isFetchingNextPage} onClick={() => void dashboards.fetchNextPage()}>{dashboards.isFetchingNextPage ? "Reading…" : "Show more"}</Button>}
    </section>
  )
}

function TileCard({ tile, index, count, invalid, catalog, runs, onChange, onEdit, onMove, onDuplicate, onRemove }: {
  readonly tile: Tile; readonly index: number; readonly count: number; readonly invalid: boolean; readonly catalog: QuestionCatalog; readonly runs: number
  readonly onChange: (tile: Tile) => void; readonly onEdit: () => void; readonly onMove: (to: number) => void; readonly onDuplicate: () => void; readonly onRemove: () => void
}) {
  return (
    <div data-field={`tile.${index}`} data-testid={`tile-${index + 1}`} data-invalid={invalid || undefined}
      className={cn("canvas-tile space-y-3 rounded-node border border-node-border bg-card p-4", tile.layout.w === 2 && "lg:col-span-2", tile.layout.h === 2 && "lg:row-span-2", invalid && "canvas-node-failing")}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2.5">
          <KindChip kind={tile.view.kind === "table" ? "table" : "chart"} />
          <span className="truncate text-[15px] leading-5 font-semibold tracking-[-0.01em]">{questionTitle(tile)}</span>
        </div>
        <RowActions index={index} count={count} label={`tile ${index + 1}`} onDuplicate={onDuplicate} onMove={onMove} onRemove={onRemove} />
      </div>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <Input aria-label={`Tile ${index + 1} title`} className="h-8 w-56" value={tile.title} placeholder="Untitled question" onChange={(event) => onChange({ ...tile, title: event.target.value })} />
        <select aria-label={`Tile ${index + 1} view`} className="h-8 rounded-3xl bg-input/50 px-3" value={viewKey(tile.view)} onChange={(event) => onChange({ ...tile, view: viewOf(event.target.value) })}>
          {VIEWS.map(([key, text]) => <option key={key} value={key}>{text}</option>)}
        </select>
        <select aria-label={`Tile ${index + 1} width`} className="h-8 rounded-3xl bg-input/50 px-3" value={String(tile.layout.w)} onChange={(event) => onChange({ ...tile, layout: { ...tile.layout, w: Number(event.target.value) } })}>
          <option value="1">Half width</option>
          <option value="2">Full width</option>
        </select>
        <select aria-label={`Tile ${index + 1} height`} className="h-8 rounded-3xl bg-input/50 px-3" value={String(tile.layout.h)} onChange={(event) => onChange({ ...tile, layout: { ...tile.layout, h: Number(event.target.value) } })}>
          <option value="1">Short</option>
          <option value="2">Tall</option>
        </select>
        <Button type="button" variant="ghost" size="sm" onClick={onEdit}><Pencil />Edit question</Button>
      </div>
      <RunQuestion key={JSON.stringify(tile.question)} question={tile.question} catalog={catalog} compact runs={runs} />
    </div>
  )
}

/** Where a new tile's question comes from: one already saved (and signed off), or one built here through the question tree. */
function AddTile({ catalog, onUse }: { readonly catalog: QuestionCatalog; readonly onUse: (question: Tile["question"], title: string) => void }) {
  const [from, setFrom] = useState<"saved" | "tree">("saved")
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const saved = useQuery({ queryKey: ["saved-questions", "for-dashboard", scope.data?.orgId, scope.data?.environmentId], queryFn: () => SAVED_QUESTIONS.list(), enabled: cellAvailable && from === "saved", retry: false })
  const tree = useQuery({ queryKey: ["question-tree"], queryFn: readRecommendations, enabled: from === "tree", retry: false, staleTime: 60_000 })
  const pick = useMutation({
    mutationFn: async (id: string) => SAVED_QUESTIONS.get(id),
    onSuccess: (stored, id) => onUse(stored.payload as Tile["question"], saved.data?.items.find((item) => item.id === id)?.title || stored.id),
  })
  return (
    <div className="space-y-4">
      <div role="group" aria-label="Where the question comes from" className="inline-flex rounded-xl border border-node-border bg-muted/40 p-1">
        <Button type="button" size="sm" variant={from === "saved" ? "secondary" : "ghost"} aria-pressed={from === "saved"} onClick={() => setFrom("saved")}><ListChecks />A saved question</Button>
        <Button type="button" size="sm" variant={from === "tree" ? "secondary" : "ghost"} aria-pressed={from === "tree"} onClick={() => setFrom("tree")}><Workflow />Build one</Button>
      </div>
      {from === "saved" && (
        <div className="space-y-2" data-testid="saved-questions">
          {!cellAvailable && scope.isSuccess && <p role="status" className="text-sm text-muted-foreground">{NO_DATA_CELL}</p>}
          {saved.isLoading && <p role="status" className="text-sm text-muted-foreground">Reading saved questions…</p>}
          {saved.isError && <p role="alert" className="text-destructive">{failure(saved.error)}</p>}
          {saved.data?.items.length === 0 && <EmptyState icon={ChartLine}>No saved questions yet. Build one instead, or save one under Questions.</EmptyState>}
          {saved.data?.items.map((item) => (
            <div key={item.id} className="flex items-center justify-between gap-3 rounded-node border border-node-border bg-card px-3 py-2 shadow-node">
              <div className="min-w-0"><div className="truncate font-medium">{item.title || item.id}</div><div className="text-meta text-muted-foreground">Version {item.version}</div></div>
              <Button type="button" size="sm" variant="outline" disabled={pick.isPending} onClick={() => pick.mutate(item.id)}><Plus />Add</Button>
            </div>
          ))}
          {pick.isError && <p role="alert" className="text-destructive">{failure(pick.error)}</p>}
        </div>
      )}
      {from === "tree" && (
        tree.isLoading ? <p role="status" className="text-sm text-muted-foreground">Reading the question tree…</p>
          : tree.isError ? <p role="alert" className="text-destructive">{failure(tree.error)}</p>
            : tree.data && <QuestionTree tree={tree.data} catalog={catalog} onUse={(question, title) => onUse(question as Tile["question"], title)} />
      )}
    </div>
  )
}

const SAVED_QUESTIONS = questionDefinitions()
/** A digest no text has: offered only to ask a cell without words on admit for its own (it refuses, stating them). */
const NO_WORDS = "0".repeat(64)

/**
 * A tile's question in the tree, opened on its current picks. A question the tree cannot express without changing
 * it is said so, and the tree offers a replacement instead; no other editor is reachable from a dashboard.
 */
function EditTile({ tile, catalog, onUse }: { readonly tile: Tile; readonly catalog: QuestionCatalog; readonly onUse: (question: Tile["question"], title: string) => void }) {
  const tree = useQuery({ queryKey: ["question-tree"], queryFn: readRecommendations, retry: false, staleTime: 60_000 })
  const draft = draftOf(tile.question, tile.title)
  const { name: _built, ...rebuilt } = buildQuestion(draft, catalog)
  const { name: _saved, ...current } = tile.question
  const fits = stable(rebuilt) === stable(current)
  if (tree.isLoading) return <p role="status" className="text-sm text-muted-foreground">Reading the question tree…</p>
  if (tree.isError) return <p role="alert" className="text-destructive">{failure(tree.error)}</p>
  if (!tree.data) return null
  return (
    <div className="space-y-4">
      {!fits && <p role="status" className="rounded-node border border-node-border bg-card p-3 text-sm shadow-node">This question uses choices the tree does not offer, so it cannot be edited here without changing it. Build its replacement below; the tile keeps its place.</p>}
      <QuestionTree tree={tree.data} catalog={catalog} {...fits ? { initial: draft } : {}} onUse={(question, title) => onUse(question as Tile["question"], title)} />
    </div>
  )
}

/** One dashboard: title and tiles, each an admitted question with its view. The cell admits and keeps it. */
export function DashboardEditor({ store, catalog, stored, initial, guard, onSaved, onReload, cellAvailable = true }: {
  readonly store: DefinitionStore; readonly catalog: QuestionCatalog; readonly stored?: StoredDashboard; readonly cellAvailable?: boolean
  /** A new dashboard's starting content (a duplicate); unsaved until signed off and saved. */
  readonly initial?: Dashboard
  readonly guard?: (dirty: boolean) => ReactNode; readonly onSaved?: (saved: { readonly id: string; readonly version: number }) => void
  /** Reopen the stored version, dropping this one (after a conflict). */
  readonly onReload?: () => void
}) {
  const [at, setAt] = useState(stored ? { id: stored.id, version: stored.version } : undefined)
  const [findings, setFindings] = useState<readonly Finding[]>([])
  const [conflict, setConflict] = useState(false)
  const [editing, setEditing] = useState<number | "new">()
  const [runs, setRuns] = useState(0)
  // D59 sign-off: the person reads the dashboard in plain words and signs off on exactly those words (by their hash).
  const [words, setWords] = useState<{ readonly key: string; readonly value: Restatement }>()
  const [serverWords, setServerWords] = useState<{ readonly key: string; readonly value: Restatement }>()
  const [signOff, setSignOff] = useState<string>()
  const [signoffNote, setSignoffNote] = useState<string>()
  const save = useMutation({
    mutationFn: async ({ dashboard, overwrite, signed }: { readonly dashboard: Dashboard; readonly overwrite?: boolean; readonly signed: string }) => {
      const id = at?.id ?? dashboardId(dashboard.title)
      return { dashboard, result: await saveDashboard(store, id, dashboard, overwrite ? undefined : at?.version, signed) }
    },
    onSuccess: ({ dashboard, result }, { signed: offered }) => {
      setConflict("conflict" in result)
      setFindings("findings" in result ? result.findings : [])
      setSignoffNote(undefined)
      if ("signoff" in result) {
        setSignOff(undefined)
        if (result.signoff === "unavailable") setSignoffNote("The cell cannot verify a sign-off yet. Your dashboard has not been saved.")
        else if (result.restatement) {
          setServerWords({ key: JSON.stringify(dashboard), value: result.restatement })
          // Asked for its words (the placeholder digest), the cell stated them: nothing differs, nothing was kept.
          setSignoffNote(offered === NO_WORDS ? undefined : "The cell reads this dashboard differently. Review its words and sign off again.")
        }
        else setSignoffNote("The cell rejected these words and sent none of its own. Your dashboard was not saved.")
      }
      if ("saved" in result) { setAt(result.saved); editor.markSaved(dashboard); onSaved?.(result.saved) }
    },
  })
  const editor = useEditor<Dashboard>({ initial: stored?.dashboard ?? EMPTY, onSave: (value) => { if (cellAvailable && signedFor(value)) save.mutate({ dashboard: value, signed: signOff! }) } })
  const { value: dashboard, set } = editor
  // A duplicate starts as an edit of an empty dashboard, so it reads as unsaved until it is signed off and saved.
  useEffect(() => { if (initial) set(initial) }, [])
  const key = JSON.stringify(dashboard)
  const complete = dashboard.title.trim() !== "" && dashboard.tiles.length > 0
  const restated = serverWords?.key === key ? serverWords.value : words?.key === key ? words.value : undefined
  const signedFor = (value: Dashboard) => signOff !== undefined && restated?.sha256 === signOff && JSON.stringify(value) === key
  const signed = signedFor(dashboard)
  // Any edit changes the words, and a sign-off holds only for the exact words signed.
  useEffect(() => { if (signOff !== undefined && restated !== undefined && restated.sha256 !== signOff) setSignOff(undefined) }, [signOff, restated?.sha256])
  // Live validation: the cell's admit is side-effect free, so it runs on every edit (after a pause).
  // Live validation and the plain words: the cell's admit is side-effect free and, with DEFS' restater, states the
  // dashboard in its own words. One source: the person signs off on exactly what the cell will recompute.
  const [checkedKey, setCheckedKey] = useState<string>()
  useEffect(() => {
    if (!cellAvailable || (dashboard.title.trim() === "" && dashboard.tiles.length === 0)) return
    let current = true
    const at_ = key
    const timer = setTimeout(() => {
      store.admit(at?.id ?? dashboardId(dashboard.title), dashboard).then((admission) => {
        if (!current) return
        setFindings(admission.admitted ? [] : admission.findings)
        setCheckedKey(at_)
        if (admission.restatement) setWords({ key: at_, value: admission.restatement })
      }, () => {})
    }, 400)
    return () => { current = false; clearTimeout(timer) }
  }, [store, at?.id, key, cellAvailable])
  const tiles = (next: readonly Tile[]) => set((current) => ({ ...current, tiles: layoutTiles(next) }))
  const issues = issuesOfFindings(findings)
  const invalid = (field: string) => issues.some((issue) => issue.field === field)
  const admitted = issues.length === 0
  // Until the cell states words on admit (DEFS), Save asks for them: it offers a digest no words have, the cell
  // refuses it as a mismatch and answers with its own words, and those are what the person then signs off on.
  const askCell = complete && admitted && checkedKey === key && restated === undefined
  const saveBlocked = !complete ? "Give the dashboard a title and at least one question first" : !signed && !askCell ? "Sign off on the dashboard in plain words first" : undefined
  const use = (question: Tile["question"], title: string) => {
    if (editing === "new") tiles([...dashboard.tiles, { id: crypto.randomUUID(), title, question, view: { kind: "chart", chart: "line" }, layout: { x: 0, y: 0, w: 2, h: 1 } }])
    else if (editing !== undefined) tiles(dashboard.tiles.map((tile, index) => index === editing ? { ...tile, title, question } : tile))
    setEditing(undefined)
  }
  const open = typeof editing === "number" ? dashboard.tiles[editing] : undefined
  return (
    <div className="space-y-4">
      {guard?.(editor.dirty)}
      <div className="flex flex-wrap items-center gap-2 rounded-xl border border-node-border bg-card/95 p-1.5 shadow-float">
        <EditorToolbar editor={editor} onSave={() => signed && signOff ? save.mutate({ dashboard, signed: signOff }) : askCell && save.mutate({ dashboard, signed: NO_WORDS })} saving={save.isPending || !cellAvailable} saveBlocked={saveBlocked} version={at?.version}>
          <Button type="button" size="sm" variant="ghost" disabled={dashboard.tiles.length === 0} title="Run every tile's question in the cell" onClick={() => setRuns((n) => n + 1)}><Play />Run all</Button>
        </EditorToolbar>
      </div>
      {save.isError && <p role="alert" className="text-destructive">{failure(save.error).startsWith("Not saved") ? failure(save.error) : `Not saved. ${failure(save.error)}`}</p>}
      {conflict && (
        <div role="alert" className="flex flex-wrap items-center gap-2 rounded-xl border border-destructive/40 bg-card p-3 text-sm">
          <span>Someone saved a newer version of this dashboard since you opened it.</span>
          <Button type="button" size="sm" variant="outline" onClick={() => onReload?.()}>Reload theirs</Button>
          <Button type="button" size="sm" variant="destructive" disabled={!cellAvailable || !signed} title={signed ? undefined : "Sign off on the dashboard in plain words first"} onClick={() => signed && signOff && save.mutate({ dashboard, overwrite: true, signed: signOff })}>Overwrite with mine</Button>
        </div>
      )}
      <ValidationPanel issues={issues} title="The cell refused this dashboard" />
      {signoffNote && <p role="alert" className="text-destructive">{signoffNote}</p>}
      <label className="grid max-w-xl gap-1.5 text-sm" data-field="title"><span className={invalid("title") ? "text-destructive" : "text-muted-foreground"}>Title</span>
        <Input value={dashboard.title} aria-invalid={invalid("title") || undefined} placeholder="e.g. Refinery weekly" onChange={(event) => set((current) => ({ ...current, title: event.target.value }))} />
      </label>
      <div data-field="tiles" className="grid gap-4 lg:grid-cols-2">
        {dashboard.tiles.length === 0 && <div className="lg:col-span-2"><EmptyState icon={LayoutDashboard}>No questions on this dashboard yet.</EmptyState></div>}
        {dashboard.tiles.map((tile, index) => (
          <TileCard key={tile.id} tile={tile} index={index} count={dashboard.tiles.length} invalid={invalid(`tile.${index}`)} catalog={catalog} runs={runs}
            onChange={(next) => tiles(dashboard.tiles.map((t, at) => at === index ? next : t))}
            onEdit={() => setEditing(index)}
            onMove={(to) => tiles(list.move(dashboard.tiles, index, to))}
            onDuplicate={() => tiles(list.duplicate(dashboard.tiles, index).map((t, at) => at === index + 1 ? { ...t, id: crypto.randomUUID() } : t))}
            onRemove={() => tiles(list.remove(dashboard.tiles, index))} />
        ))}
      </div>
      <Button type="button" variant="outline" size="sm" onClick={() => setEditing("new")}><Plus />Add question</Button>
      {complete && (
        <Panel icon={ShieldCheck} className="rounded-node border-node-border shadow-node" label="In plain words" action={restated && <span className="font-mono text-xs text-muted-foreground" title={restated.sha256}>{restated.sha256.slice(0, 12)}</span>}>
          <div className="space-y-3 p-2">
            {!restated && !askCell && <p role="status" className="text-sm text-muted-foreground">The cell is checking the dashboard…</p>}
            {askCell && <p role="status" className="text-sm text-muted-foreground">This cell does not state dashboards in plain words on its check yet. Save asks it for its words; nothing is kept until you sign off on them.</p>}
            {restated && <>
              {serverWords?.key === key && <p className="font-medium">The cell reads your dashboard as:</p>}
              <p className="text-base whitespace-pre-line" data-testid="dashboard-restatement">{restated.text}</p>
              <p className="text-meta text-muted-foreground">Written by the cell from the dashboard's questions, not by the assistant. It checks these exact words again when it keeps the dashboard.</p>
              <div className="flex flex-wrap items-center gap-3">
                {signed
                  ? <span className="flex items-center gap-2 text-sm font-medium"><BadgeCheck className="size-4 text-success" />Signed off: this is the dashboard you mean</span>
                  : <Button type="button" size="sm" disabled={!admitted} title={admitted ? undefined : "The cell must admit the dashboard first"} onClick={() => setSignOff(restated.sha256)}>This is what I asked for</Button>}
                {!admitted && <span className="text-meta text-muted-foreground">Sign-off opens once the cell admits the dashboard.</span>}
              </div>
            </>}
          </div>
        </Panel>
      )}
      <Sheet open={editing !== undefined} onOpenChange={(next) => { if (!next) setEditing(undefined) }}>
        <SheetContent className="w-full overflow-y-auto sm:max-w-4xl">
          <SheetHeader>
            <SheetTitle>{open ? `Edit "${questionTitle(open)}"` : "Add a question"}</SheetTitle>
            <SheetDescription>{open ? "The tree opens on this tile's picks; change them, sign off on the new words and update the tile." : "Add one you have saved, or build one here; either way the checker admits it and you sign off on its words."}</SheetDescription>
          </SheetHeader>
          <div className="p-4">{editing === "new" ? <AddTile catalog={catalog} onUse={use} /> : open && <EditTile key={String(editing)} tile={open} catalog={catalog} onUse={use} />}</div>
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
  const stored = useQuery({ queryKey: ["dashboard", id, scope.data?.orgId, scope.data?.environmentId], queryFn: async () => (id === undefined ? null : await store.get(id)) ?? null, enabled: id !== undefined && cellAvailable, retry: false })
  const fresh = id === undefined
  // A duplicate opens here as a new, unsaved dashboard (it is kept only once signed off and saved).
  const [copy] = useState(() => { const taken = fresh ? pendingCopy : undefined; pendingCopy = undefined; return taken })
  const [title, setTitle] = useState(id === undefined ? "New dashboard" : "Dashboard")
  useEffect(() => { if (stored.data) setTitle(stored.data.dashboard.title || "Untitled dashboard") }, [stored.data])
  return (
    <section className="space-y-6">
      <h1 className="text-title font-medium">{title}</h1>
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {cellAvailable && (catalog.isPending || (!fresh && stored.isPending)) && <p className="text-muted-foreground">Reading the dashboard…</p>}
      {catalog.isError && <EmptyState icon={LayoutDashboard}>{catalog.error.message}</EmptyState>}
      {stored.isError && <EmptyState icon={LayoutDashboard}>{failure(stored.error)}</EmptyState>}
      {!fresh && stored.isSuccess && stored.data === null && <EmptyState icon={LayoutDashboard}>No dashboard {id} is saved.</EmptyState>}
      {catalog.data && (fresh || stored.data) && (
        <DashboardEditor key={`${scope.data?.orgId ?? "none"}:${scope.data?.environmentId ?? "none"}:${stored.data?.id ?? "new"}:${stored.data?.version ?? 0}`} cellAvailable={cellAvailable} store={store} catalog={catalog.data} stored={stored.data ?? undefined} initial={copy} guard={(dirty) => <DirtyGuard dirty={dirty} />}
          onReload={() => void stored.refetch()}
          onSaved={(saved) => { if (id === undefined) setTimeout(() => void navigate({ to: "/dashboards/$id", params: { id: saved.id }, replace: true })) }} />
      )}
    </section>
  )
}
