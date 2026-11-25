import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate, useParams } from "@tanstack/react-router"
import { ArrowDown, ArrowUp, Bookmark, Copy, Eye, Pencil, Plus, Shapes, Trash2, Undo2 } from "lucide-react"
import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type FormEvent } from "react"
import { DirtyGuard, EditorToolbar, ValidationPanel, useEditor } from "@/components/editor"
import { EmptyState, PageHeader, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { isWithheld, WITHHELD_HINT, WITHHELD_ROW, WithheldMark, type GridRow } from "./grid/cells.tsx"
import { FilterEditor, fromPredicate, toPredicate, type DraftClause } from "./grid/filter-editor.tsx"
import { RecordsGrid } from "./grid/records-grid.tsx"
import { openedQuery } from "./grid/views-bar.tsx"
import type { Concept } from "./lib/catalog.ts"
import { NO_DATA_CELL, StudioScopeError, useStudioScope, type Finding, type StudioScope } from "./lib/definitions.ts"
import { untilDecided, useWorkspace, type Workspace } from "./lib/workspace.tsx"
import { matchWorkspaceState } from "./lib/state/index.ts"
import { WorkspaceUndecided } from "@/components/load-state"
import type { Issue } from "./lib/editor.ts"
import { readerKeyIn, useReaderKey } from "./lib/session.ts"
import { recordName } from "./lib/objects.ts"
import { LIMITS, propertyWithheld, recordsQueryPrefix, sortable, totalText, type SortKey } from "./lib/records-query.ts"
import { offersTimeline } from "./lib/timeline.ts"
import {
  cellViews,
  cleanView,
  DEFAULT_SPACES,
  duplicateView,
  newViewId,
  sameScope,
  saveViewDraft,
  sortKeys,
  sortText,
  viewField,
  viewPreviewQueryKey,
  WORKSPACE_CHANGED_MESSAGE,
  type SavedView,
  type StoredView,
  type ViewConflict,
  type ViewSaved,
  type ViewsClient,
} from "./lib/views.ts"
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

export interface RemovedView {
  readonly id: string
  readonly version: number
  readonly scope: StudioScope
  readonly view: SavedView
}

/** A deleted view's receipt: the view as it was, and the reader who deleted it. */
export interface DeletionReceipt extends RemovedView { readonly reader: string }

/**
 * Deletion receipts, kept above the routes so each deletion keeps its own Undo across navigation,
 * keyed by reader, workspace and view id: a second deletion never replaces the first, and one
 * reader's or workspace's receipt never shows for another. Memory only, like kept drafts.
 */
const receiptKey = ({ reader, scope, id }: Pick<DeletionReceipt, "reader" | "scope" | "id">) => JSON.stringify([reader, scope.orgId, scope.environmentId, id])
let receiptList: readonly DeletionReceipt[] = []
const receiptListeners = new Set<() => void>()
const publishReceipts = (next: readonly DeletionReceipt[]) => { receiptList = next; for (const listener of receiptListeners) listener() }
export const deletionReceipts = {
  add(receipt: DeletionReceipt): void {
    const key = receiptKey(receipt)
    publishReceipts([...receiptList.filter((kept) => receiptKey(kept) !== key), receipt])
  },
  remove(receipt: Pick<DeletionReceipt, "reader" | "scope" | "id">): void {
    const key = receiptKey(receipt)
    publishReceipts(receiptList.filter((kept) => receiptKey(kept) !== key))
  },
  /** The receipts this reader made in this workspace, oldest first. */
  of(reader: string, scope: StudioScope | null): readonly DeletionReceipt[] {
    return receiptList.filter((kept) => kept.reader === reader && sameScope(kept.scope, scope))
  },
  subscribe(listener: () => void): () => void { receiptListeners.add(listener); return () => { receiptListeners.delete(listener) } },
  snapshot: (): readonly DeletionReceipt[] => receiptList,
  clear(): void { publishReceipts([]) },
}

/** Who started a deletion: the reader and the workspace shown when Delete was pressed. */
export interface DeletionStart { readonly reader: string; readonly scope: StudioScope | null }

/**
 * The receipt for a deletion the cell just answered, bound to who started it. When the reader
 * (another sign-in, or a role change) or the workspace changed while the deletion was pending,
 * there is none: its Undo belongs to the reader who deleted, never to the next one.
 */
export function deletionReceiptFor(gone: RemovedView | undefined, started: DeletionStart, now: DeletionStart): DeletionReceipt | undefined {
  if (gone === undefined || started.reader !== now.reader || !sameScope(started.scope, now.scope) || !sameScope(gone.scope, started.scope)) return undefined
  return { ...gone, reader: started.reader }
}

/**
 * Whether a read confirms a newer active revision of a deleted view (someone restored it, here or
 * elsewhere): the receipt is stale, and its Undo would only conflict with that revision.
 */
export function receiptSuperseded(receipt: RemovedView, served: StoredView | null | undefined): boolean {
  return served != null && served.id === receipt.id && sameScope(served.scope, receipt.scope) && served.version > receipt.version
}

/** Drop every receipt a list or detail read shows superseded by a newer active revision. */
function reconcileReceipts(served: readonly StoredView[]): void {
  for (const receipt of deletionReceipts.snapshot()) {
    if (served.some((view) => receiptSuperseded(receipt, view))) deletionReceipts.remove(receipt)
  }
}

/** A deletion's start (as rendered when pressed) and the reader and workspace now (from the cache, when the cell answers). */
function useDeletionStart(scope: StudioScope | null): { readonly start: () => DeletionStart; readonly now: () => DeletionStart } {
  const client = useQueryClient()
  const reader = useReaderKey()
  return {
    start: () => ({ reader, scope }),
    now: () => ({ reader: readerKeyIn(client), scope: client.getQueryData<StudioScope | null>(["studio-scope"]) ?? null }),
  }
}

/** This reader's deletion receipts in `scope`, re-rendered as receipts are added or restored. */
function useDeletionReceipts(scope: StudioScope | null): { readonly reader: string; readonly receipts: readonly DeletionReceipt[] } {
  const reader = useReaderKey()
  const all = useSyncExternalStore(deletionReceipts.subscribe, deletionReceipts.snapshot, deletionReceipts.snapshot)
  const receipts = useMemo(() => all.filter((kept) => kept.reader === reader && sameScope(kept.scope, scope)), [all, reader, scope?.orgId, scope?.environmentId])
  return { reader, receipts }
}

export type ReloadViewOutcome = "reloaded" | "deleted" | "failed"

/** Scope a deleted-view banner to the exact route view id and active workspace scope. */
export const scopedDeletedView = <T extends RemovedView>(
  removed: T | undefined,
  id: string,
  activeScope: StudioScope | null,
): T | undefined =>
  removed !== undefined && removed.id === id && sameScope(removed.scope, activeScope) ? removed : undefined

/**
 * Reload the latest server revision after a conflict. Distinguishes an active reload from a
 * retired/missing view (`"deleted"`, suppressed to null by `views.get`) and from a transport failure.
 */
export async function reloadConflictingView(
  refetch: () => Promise<{ readonly isSuccess: boolean; readonly data: StoredView | null | undefined }>,
  onLoaded: (stored: StoredView) => void,
): Promise<ReloadViewOutcome> {
  const result = await refetch()
  if (!result.isSuccess) return "failed"
  if (result.data) {
    onLoaded(result.data)
    return "reloaded"
  }
  return "deleted"
}

export function ViewConflictBanner({
  conflict,
  canReload,
  canOverwrite,
  canCreate,
  saving,
  reloading,
  reloadError,
  onReload,
  onOverwrite,
  onSaveAsNew,
  onDiscard,
}: {
  readonly conflict: ViewConflict
  readonly canReload: boolean
  readonly canOverwrite: boolean
  readonly canCreate: boolean
  readonly saving: boolean
  readonly reloading: boolean
  readonly reloadError: string | null
  readonly onReload: () => void
  readonly onOverwrite: (confirmedVersion: number) => void
  readonly onSaveAsNew: () => void
  readonly onDiscard?: () => void
}) {
  if (conflict.kind === "deleted") {
    return (
      <div role="alert" className="flex flex-wrap items-center gap-2 rounded-xl border border-destructive/40 bg-card p-3 text-body">
        <span>This view was deleted by someone else.</span>
        <Button type="button" size="sm" variant="outline" disabled={!canCreate || saving} onClick={onSaveAsNew}>
          Save as new view
        </Button>
        {onDiscard && (
          <Button type="button" size="sm" variant="ghost" disabled={saving} onClick={onDiscard}>
            Discard my changes
          </Button>
        )}
      </div>
    )
  }
  return (
    <div role="alert" className="flex flex-wrap items-center gap-2 rounded-xl border border-destructive/40 bg-card p-3 text-body">
      <span>
        {conflict.kind === "updated"
          ? `Someone saved version ${conflict.version} of this view since you opened it.`
          : "Someone saved a newer version of this view since you opened it."}
      </span>
      {canReload && (
        <Button type="button" size="sm" variant="outline" disabled={reloading || saving} onClick={onReload}>
          {reloading ? "Reloading…" : "Reload theirs"}
        </Button>
      )}
      {canOverwrite && conflict.kind === "updated" && (
        <Button
          type="button"
          size="sm"
          variant="destructive"
          disabled={!canCreate || saving || reloading}
          onClick={() => onOverwrite(conflict.version)}
        >
          Overwrite with mine
        </Button>
      )}
      {reloadError && <p className="basis-full text-destructive">{reloadError}</p>}
    </div>
  )
}

/** One deletion's notice with its own Undo: restoring one never touches another's. */
function DeletionReceiptNotice({ receipt, store, onRestored }: {
  readonly receipt: DeletionReceipt
  readonly store: Pick<ViewsClient, "save">
  readonly onRestored: () => void
}) {
  const undo = useMutation({
    mutationFn: async (gone: DeletionReceipt) => settled(await store.save(gone.id, gone.view, gone.version, gone.scope)),
    onSuccess: (_saved, gone) => { deletionReceipts.remove(gone); onRestored() },
  })
  return <UndoDeleteView removed={receipt} pending={undo.isPending} error={undo.isError ? failure(undo.error) : undefined} onUndo={() => undo.mutate(receipt)} />
}

function UndoDeleteView({ removed, pending, error, onUndo }: {
  readonly removed: RemovedView
  readonly pending: boolean
  readonly error?: string
  readonly onUndo: () => void
}) {
  return (
    <div role="status" className="flex flex-wrap items-center gap-2 rounded-xl border bg-card p-3 text-body shadow-elevation">
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
          <label className="grid gap-1 text-body">
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

export { useViewsMenu } from "./lib/nav-menus.ts"

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
  const { receipts } = useDeletionReceipts(activeScope)
  const deletion = useDeletionStart(activeScope)
  useEffect(() => { if (list.data) reconcileReceipts(list.data) }, [list.data])

  const duplicate = useMutation({
    mutationFn: async (stored: StoredView) => {
      const copy = duplicateView(stored.view)
      const id = newViewId(copy.objectType, copy.title)
      return settled(await views.save(id, copy, undefined, stored.scope))
    },
    onSuccess: refresh,
  })

  // The reader and workspace travel with the deletion from the press; the receipt is kept only if both still hold when the cell answers.
  const remove = useMutation({
    mutationFn: async ({ stored }: { readonly stored: StoredView; readonly started: DeletionStart }) => {
      const retired = settled(await views.retire(stored.id, stored.version, stored.scope))
      return retired.saved ? { id: stored.id, version: retired.version, scope: stored.scope, view: stored.view } : undefined
    },
    onSuccess: (gone, { started }) => { const receipt = deletionReceiptFor(gone, started, deletion.now()); if (receipt) deletionReceipts.add(receipt) },
    onSettled: refresh,
  })

  const rename = async (stored: StoredView, title: string): Promise<string | null> => {
    try {
      const res = await views.save(stored.id, { ...stored.view, title }, stored.version, stored.scope)
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
        description="Declarative projections over admitted record types, pinned into Spaces."
        primary={<Button disabled={!cellAvailable} onClick={() => void navigate({ to: "/views/new" })}><Plus />New view</Button>}
      />
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="flex items-center gap-2 text-foreground"><span aria-hidden className="size-[6px] shrink-0 rounded-full bg-dot-bad" />{scope.error.message}</p>}
      {cellAvailable && list.isPending && <p className="text-muted-foreground">Reading views…</p>}
      {list.isError && <EmptyState icon={Bookmark}>{failure(list.error)}</EmptyState>}
      {duplicate.isError && <p role="alert" className="flex items-center gap-2 text-foreground"><span aria-hidden className="size-[6px] shrink-0 rounded-full bg-dot-bad" />Not duplicated. {failure(duplicate.error)}</p>}
      {remove.isError && <p role="alert" className="flex items-center gap-2 text-foreground"><span aria-hidden className="size-[6px] shrink-0 rounded-full bg-dot-bad" />Not deleted. {failure(remove.error)}</p>}
      {receipts.map((receipt) => <DeletionReceiptNotice key={receipt.id} receipt={receipt} store={views} onRestored={() => void refresh()} />)}
      {list.data?.length === 0 && (
        <EmptyState
          icon={Bookmark}
          title="No views yet"
          next={cellAvailable && <Button size="sm" variant="outline" onClick={() => void navigate({ to: "/views/new" })}><Plus />New view</Button>}
        >
          Build a filtered view over any record type, pick its columns and sort order, and pin it into a Space.
        </EmptyState>
      )}
      {grouped.map(([space, items]) => (
        <div key={space} className="space-y-3">
          <h2 className="text-meta text-muted-foreground">{space}</h2>
          <div className="grid gap-3">
            {items.map((stored) => {
              const concept = catalog?.byName.get(stored.view.objectType)
              const filterCount = stored.view.filter?.all.length ?? 0
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
                      <Button variant="ghost" size="sm" onClick={() => remove.mutate({ stored, started: deletion.start() })}><Trash2 />Delete</Button>
                    </div>
                  }
                >
                  <Well className="px-3.5 py-3">
                    <div className="flex flex-wrap items-center gap-2 text-body text-muted-foreground">
                      <Badge variant="secondary">{concept?.title ?? stored.view.objectType}</Badge>
                      {stored.view.space && <Badge variant="outline">Space: {stored.view.space}</Badge>}
                      <span>{filterCount === 0 ? "All records" : `${filterCount} filter${filterCount === 1 ? "" : "s"}`}</span>
                      <span><span aria-hidden className="text-subtle-foreground">·</span> Version {stored.version}</span>
                    </div>
                    {stored.view.description && <p className="mt-1.5 text-body text-muted-foreground">{stored.view.description}</p>}
                  </Well>
                </Panel>
              )
            })}
          </div>
        </div>
      ))}
    </section>
  )
}

/** `/views/$id`: open a saved view with its filtered list, record drawer and action sheet. */
export function ViewDetail(props: {
  readonly store?: ViewsClient
}) {
  const { id = "" } = useParams({ strict: false }) as { id?: string }
  return <ScopedViewDetail key={id} id={id} {...props} />
}

function ScopedViewDetail({ id, store = views }: {
  readonly id: string
  readonly store?: ViewsClient
}) {
  const scope = useStudioScope()
  const activeScope = scope.data ?? null
  const cellAvailable = scope.isSuccess && activeScope !== null
  const { catalog } = useCatalog()
  const navigate = useNavigate()
  const client = useQueryClient()
  const [sourceScope, setSourceScope] = useState<StudioScope | null>(null)
  const readScope = sourceScope ?? activeScope
  const scopeMismatch = sourceScope !== null && !sameScope(activeScope, sourceScope)
  const stored = useQuery({
    queryKey: ["view", id, readScope?.orgId, readScope?.environmentId],
    queryFn: async ({ signal }) => readScope === null ? null : (await store.getAtScope(id, readScope, signal)) ?? null,
    enabled: cellAvailable && !scopeMismatch && id !== "",
    retry: false,
  })
  if (sourceScope === null && stored.data != null) setSourceScope(stored.data.scope)
  const detailScopeMismatch = scopeMismatch
    || (stored.error instanceof StudioScopeError && stored.error.message === WORKSPACE_CHANGED_MESSAGE)
  const { receipts } = useDeletionReceipts(activeScope)
  const deletion = useDeletionStart(activeScope)
  useEffect(() => { if (stored.data) reconcileReceipts([stored.data]) }, [stored.data])
  const invalidate = () => {
    void client.invalidateQueries({ queryKey: ["views"] })
    void client.invalidateQueries({ queryKey: ["view", id] })
  }

  const duplicate = useMutation({
    mutationFn: async (from: StoredView) => {
      const copy = duplicateView(from.view)
      const nextId = newViewId(copy.objectType, copy.title)
      settled(await store.save(nextId, copy, undefined, from.scope))
      return nextId
    },
    onSuccess: (copied) => {
      void client.invalidateQueries({ queryKey: ["views"] })
      void navigate({ to: "/views/$id", params: { id: copied } })
    },
  })

  const remove = useMutation({
    mutationFn: async ({ from }: { readonly from: StoredView; readonly started: DeletionStart }) => {
      const retired = settled(await store.retire(from.id, from.version, from.scope))
      return retired.saved ? { id: from.id, version: retired.version, scope: from.scope, view: from.view } : undefined
    },
    onSuccess: (gone, { started }) => {
      const receipt = deletionReceiptFor(gone, started, deletion.now())
      if (receipt) deletionReceipts.add(receipt)
      void client.invalidateQueries({ queryKey: ["views"] })
    },
  })

  const rename = async (from: StoredView, title: string): Promise<string | null> => {
    try {
      const res = await store.save(from.id, { ...from.view, title }, from.version, from.scope)
      if (!res.saved) return "conflict" in res ? "Someone changed this view; reload and try again." : res.findings.map((f) => f.message).join(" ")
      invalidate()
      return null
    } catch (error) {
      return failure(error)
    }
  }

  // A read that serves a newer active revision wins over a stale receipt (the view was restored).
  const receipt = receipts.find((kept) => kept.id === id && !receiptSuperseded(kept, stored.data))
  const deleted = detailScopeMismatch ? undefined : scopedDeletedView(receipt, id, activeScope)
  const active = deleted || detailScopeMismatch ? undefined : stored.data
  const loaded = stored.data
  useEffect(() => {
    if (detailScopeMismatch && loaded != null) {
      void client.cancelQueries({ queryKey: recordsQueryPrefix(loaded.view.objectType, loaded.scope) })
    }
  }, [client, detailScopeMismatch, loaded])
  // The view's records are read through the workspace the page's mode decided, and only when that is
  // the workspace the view was read from: never at a workspace the mode did not decide (#1762).
  const workspace = useWorkspace()
  const viewReader = active != null && workspace.kind === "live" && sameScope(workspace.scope, active.scope) ? workspace.reader : undefined
  const concept = active ? catalog?.byName.get(active.view.objectType) : undefined
  const timeline = catalog !== undefined && concept !== undefined && offersTimeline(catalog, concept)

  return (
    <section className="space-y-6">
      <div className="flex flex-wrap items-start gap-2">
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="text-title tracking-tight">{active?.view.title || "View"}</h1>
            {active?.view.space && <Badge variant="outline">Space: {active.view.space}</Badge>}
            {active && <Badge variant="secondary">{concept?.title ?? active.view.objectType}</Badge>}
            {active && <Badge variant="outline">Version {active.version}</Badge>}
          </div>
          {active?.view.description && <p className="text-body text-muted-foreground">{active.view.description}</p>}
        </div>
        {active && (
          <div className="flex flex-wrap items-center gap-1">
            <Button asChild size="sm"><Link to="/views/$id/edit" params={{ id: active.id }}><Pencil />Edit</Link></Button>
            <RenamePopover current={active.view.title} onRename={(title) => rename(active, title)} />
            <Button variant="ghost" size="sm" disabled={duplicate.isPending} onClick={() => duplicate.mutate(active)}><Copy />Duplicate</Button>
            <Button variant="ghost" size="sm" disabled={remove.isPending} onClick={() => remove.mutate({ from: active, started: deletion.start() })}><Trash2 />Delete</Button>
          </div>
        )}
      </div>

      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="flex items-center gap-2 text-foreground"><span aria-hidden className="size-[6px] shrink-0 rounded-full bg-dot-bad" />{scope.error.message}</p>}
      {detailScopeMismatch && <p role="alert" className="text-destructive">{WORKSPACE_CHANGED_MESSAGE}</p>}
      {duplicate.isError && <p role="alert" className="flex items-center gap-2 text-foreground"><span aria-hidden className="size-[6px] shrink-0 rounded-full bg-dot-bad" />Not duplicated. {failure(duplicate.error)}</p>}
      {remove.isError && <p role="alert" className="flex items-center gap-2 text-foreground"><span aria-hidden className="size-[6px] shrink-0 rounded-full bg-dot-bad" />Not deleted. {failure(remove.error)}</p>}
      {!detailScopeMismatch && (scope.isPending || (cellAvailable && stored.isPending)) && <p className="text-muted-foreground">Reading view…</p>}
      {!detailScopeMismatch && stored.isError && <EmptyState icon={Bookmark}>{failure(stored.error)}</EmptyState>}
      {deleted && <DeletionReceiptNotice receipt={deleted} store={store} onRestored={() => { invalidate(); void stored.refetch() }} />}
      {deleted && <Button asChild variant="ghost" size="sm"><Link to="/views">Back to views</Link></Button>}
      {stored.isSuccess && active === null && <EmptyState icon={Bookmark}>No view {id} is saved.</EmptyState>}
      {active && concept === undefined && catalog && (
        <EmptyState icon={Shapes}>Record type {active.view.objectType} is not installed in this cell.</EmptyState>
      )}
      {active && concept !== undefined && (viewReader !== undefined
        ? (
          <RecordsGrid
            key={`${active.scope.orgId}:${active.scope.environmentId}:${active.id}:${active.version}`}
            concept={concept}
            reader={viewReader}
            initial={openedQuery(active)}
            timeline={timeline}
          />
        )
        : workspace.kind === "deciding" || workspace.kind === "unavailable"
          ? <WorkspaceUndecided workspace={workspace} bone="records-table" loadingLabel="Reading the records" />
          : <p role="alert" className="text-destructive">{workspace.kind === "recorded" ? NO_DATA_CELL : WORKSPACE_CHANGED_MESSAGE}</p>)}
    </section>
  )
}

export function ViewDetailPage() {
  return <ViewDetail />
}

function defaultViewFor(concept: Concept | undefined): SavedView {
  const cols = (concept?.fields ?? []).filter(({ id }) => !HIDDEN_FIELDS.has(id)).slice(0, 6).map(({ id }) => id)
  return {
    title: "",
    description: "",
    space: DEFAULT_SPACES[0],
    objectType: concept?.name ?? "",
    columns: cols,
  }
}

type ViewEditorDraft = { readonly view: SavedView; readonly spaceMode: "preset" | "custom" }

export function viewEditorDraft(view: SavedView): ViewEditorDraft {
  return { view, spaceMode: view.space && !DEFAULT_SPACES.some((space) => space === view.space) ? "custom" : "preset" }
}

export function selectViewSpace(draft: ViewEditorDraft, selection: string): ViewEditorDraft {
  if (selection === "__custom__") {
    return { view: { ...draft.view, space: "" }, spaceMode: "custom" }
  }
  return { view: { ...draft.view, space: selection || undefined }, spaceMode: "preset" }
}

export function ViewEditor({
  concepts,
  stored,
  activeScope,
  workspace,
  cellAvailable,
  onSaved,
  onReload,
  onDiscard,
  store = views,
  initialConflict = null,
}: {
  readonly concepts: readonly Concept[]
  readonly stored?: StoredView
  readonly activeScope: StudioScope | null
  /**
   * The workspace the page's mode decided (`lib/workspace.tsx`): the live preview reads only in a live
   * one, and only while the draft is bound to it (#1762); until one is decided it reads and shows nothing.
   */
  readonly workspace: Workspace
  readonly cellAvailable: boolean
  readonly onSaved: (saved: { readonly id: string; readonly version: number }) => void
  readonly onReload?: () => Promise<ReloadViewOutcome>
  readonly onDiscard?: () => void
  readonly store?: ViewsClient
  readonly initialConflict?: ViewConflict | null
}) {
  const client = useQueryClient()
  // Bind this draft once to the first workspace scope it was loaded from; never silently retarget a write.
  const [draftScope, setDraftScope] = useState<StudioScope | null>(() => stored?.scope ?? activeScope)
  useEffect(() => {
    if (draftScope === null && activeScope !== null) setDraftScope(activeScope)
  }, [draftScope, activeScope])
  const scopeMismatch = draftScope !== null && !sameScope(activeScope, draftScope)
  const [at, setAt] = useState(stored ? { id: stored.id, version: stored.version } : undefined)
  const [findings, setFindings] = useState<readonly Finding[]>([])
  const [restatement, setRestatement] = useState<string | null>(null)
  const [conflict, setConflict] = useState<ViewConflict | null>(initialConflict)
  const [scopeError, setScopeError] = useState<string | null>(null)
  const [reloading, setReloading] = useState(false)
  const [reloadError, setReloadError] = useState<string | null>(null)
  const initial = useMemo(() => viewEditorDraft(stored?.view ?? defaultViewFor(concepts[0])), [stored, concepts])
  const saving = useRef(false)
  const [pendingNav, setPendingNav] = useState<{ readonly id: string; readonly version: number } | null>(null)
  const [pendingDiscard, setPendingDiscard] = useState(false)
  const save = useMutation({
    mutationFn: async ({
      draft,
      confirmedVersion,
      asNew,
    }: {
      readonly draft: ViewEditorDraft
      readonly confirmedVersion?: number
      readonly asNew?: boolean
    }) => {
      const { view } = draft
      if (draftScope === null) throw new StudioScopeError(NO_DATA_CELL)
      if (scopeMismatch || scopeError === WORKSPACE_CHANGED_MESSAGE) {
        throw new StudioScopeError(WORKSPACE_CHANGED_MESSAGE)
      }
      const existing = !asNew && at !== undefined
      const id = existing ? at.id : newViewId(view.objectType, view.title)
      const expectedVersion = existing
        ? (confirmedVersion !== undefined ? confirmedVersion : at.version)
        : undefined
      return saveViewDraft(store, {
        id,
        existing,
        view,
        expectedVersion,
        scope: draftScope,
      })
    },
    onSuccess: ({ result, conflict: nextConflict }, { draft }) => {
      setScopeError(null)
      setReloadError(null)
      setFindings("findings" in result ? result.findings : [])
      if (result.saved) {
        setConflict(null)
        setAt({ id: result.id, version: result.version })
        editor.markSaved(draft)
        setPendingNav({ id: result.id, version: result.version })
      } else if (nextConflict !== null) {
        setConflict(nextConflict)
      } else {
        setConflict((prev) => (prev?.kind === "deleted" ? prev : null))
      }
    },
    onError: (error) => {
      if (error instanceof StudioScopeError && error.message === WORKSPACE_CHANGED_MESSAGE) {
        setScopeError(error.message)
        setConflict(null)
      }
    },
  })
  const requestSave = (draft: ViewEditorDraft, mode: { readonly confirmedVersion?: number; readonly asNew?: boolean } = {}) => {
    const forced = mode.confirmedVersion !== undefined || mode.asNew === true
    if (!cellAvailable || draftScope === null || saving.current || reloading || (!forced && (!editor.dirty || conflict?.kind === "deleted"))) return
    if (scopeMismatch || scopeError === WORKSPACE_CHANGED_MESSAGE) {
      setScopeError(WORKSPACE_CHANGED_MESSAGE)
      return
    }
    saving.current = true
    save.mutate(
      { draft, confirmedVersion: mode.confirmedVersion, asNew: mode.asNew },
      { onSettled: () => { saving.current = false } },
    )
  }
  const editor = useEditor<ViewEditorDraft>({ initial, onSave: (draft) => requestSave(draft) })
  const { view, spaceMode } = editor.value
  const set = (update: (view: SavedView) => SavedView) =>
    editor.set((draft) => ({ ...draft, view: update(draft.view) }))
  // Navigate only after React commits `editor.dirty === false` so DirtyGuard never blocks a completed save or discard.
  useEffect(() => {
    if (pendingNav !== null && !editor.dirty) {
      const next = pendingNav
      setPendingNav(null)
      onSaved(next)
    }
  }, [pendingNav, editor.dirty, onSaved])
  useEffect(() => {
    if (pendingDiscard && !editor.dirty) {
      setPendingDiscard(false)
      onDiscard?.()
    }
  }, [pendingDiscard, editor.dirty, onDiscard])

  const discardChanges = () => {
    setConflict(null)
    setReloadError(null)
    editor.markSaved(editor.value)
    setPendingDiscard(true)
  }

  const reloadTheirs = async () => {
    if (!onReload || reloading || saving.current) return
    setReloading(true)
    setReloadError(null)
    try {
      const outcome = await onReload()
      if (outcome === "deleted") {
        setConflict({ kind: "deleted", version: null })
      } else if (outcome === "failed") {
        setReloadError("Could not reload the latest version of this view.")
      }
    } catch (error) {
      if (error instanceof StudioScopeError && error.message === WORKSPACE_CHANGED_MESSAGE) {
        setScopeError(error.message)
        setConflict(null)
      } else {
        setReloadError(failure(error))
      }
    } finally {
      setReloading(false)
    }
  }

  const concept = concepts.find(({ name }) => name === view.objectType) ?? concepts[0]
  const fields = useMemo(() => (concept?.fields ?? []).filter(({ id }) => !HIDDEN_FIELDS.has(id)), [concept])
  const sortableFields = useMemo(() => fields.filter(sortable), [fields])

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

  // Live server admission check (side-effect free), bound to `draftScope`.
  useEffect(() => {
    if (!cellAvailable || draftScope === null || scopeMismatch || scopeError !== null || view.title.trim() === "" || view.objectType === "") {
      setFindings([])
      setRestatement(null)
      return
    }
    let cancelled = false
    const timer = setTimeout(() => {
      const id = at?.id ?? newViewId(view.objectType, view.title, () => "check")
      store.admit(id, view, draftScope).then(
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
        (error: unknown) => {
          if (cancelled) return
          if (error instanceof StudioScopeError && error.message === WORKSPACE_CHANGED_MESSAGE) {
            setScopeError(error.message)
          }
        },
      )
    }, 350)
    return () => { cancelled = true; clearTimeout(timer) }
  }, [cellAvailable, draftScope, scopeMismatch, scopeError, at?.id, view, store])

  const sort = sortKeys(view.sort)
  const columns = view.columns ?? fields.slice(0, 6).map(({ id }) => id)

  // Live bounded preview of matching records, bound explicitly to `draftScope` and read only when that
  // is the live workspace the page's mode decided. Until it is, the read waits under a key no answer
  // reaches (`untilDecided`), so rows read before the mode failed, or in another workspace, never show (#1763).
  const previewScope = workspace.kind === "live" && sameScope(workspace.scope, draftScope) ? workspace.scope : null
  const previewRead = untilDecided(previewScope === null ? undefined : {
    queryKey: viewPreviewQueryKey(previewScope, view),
    queryFn: ({ signal }) => store.preview(view.objectType, { filter: view.filter ?? null, sort, limit: 5 }, previewScope, signal),
  }, viewPreviewQueryKey(draftScope, view))
  const previewQuery = useQuery({
    ...previewRead,
    enabled: previewRead.enabled && cellAvailable && !scopeMismatch && scopeError === null && view.objectType !== "",
    retry: false,
  })
  useEffect(() => {
    if (scopeMismatch && draftScope !== null) {
      void client.cancelQueries({ queryKey: ["view-preview", draftScope.orgId, draftScope.environmentId] })
    }
  }, [client, scopeMismatch, draftScope])
  useEffect(() => {
    if (previewQuery.error instanceof StudioScopeError && previewQuery.error.message === WORKSPACE_CHANGED_MESSAGE) {
      setScopeError(WORKSPACE_CHANGED_MESSAGE)
    }
  }, [previewQuery.error])
  const previewWorkspaceChanged =
    previewQuery.error instanceof StudioScopeError && previewQuery.error.message === WORKSPACE_CHANGED_MESSAGE
  const workspaceAlert = scopeMismatch || previewWorkspaceChanged ? WORKSPACE_CHANGED_MESSAGE : scopeError
  const previewPage = workspaceAlert === null && previewScope !== null && previewQuery.data?.kind === "page" ? previewQuery.data : undefined

  const issues = issuesOf(findings)
  const invalid = (field: string) => issues.some((i) => i.field === field)
  const selectCls = "h-9 rounded-md border bg-background px-2.5 text-body"

  const pickType = (nextType: string) => {
    const nextConcept = concepts.find(({ name }) => name === nextType)
    const nextCols = (nextConcept?.fields ?? []).filter(({ id }) => !HIDDEN_FIELDS.has(id)).slice(0, 6).map(({ id }) => id)
    setDraftFilter([])
    set((cur) => ({ ...cur, objectType: nextType, filter: undefined, sort: undefined, columns: nextCols }))
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

  const setSort = (next: readonly SortKey[]) => set((cur) => ({ ...cur, sort: sortText(next) }))
  const previewCols = columns.slice(0, 5)

  return (
    <div className="space-y-6">
      <DirtyGuard dirty={editor.dirty} />
      <EditorToolbar editor={editor} onSave={() => requestSave(editor.value)} saving={save.isPending || reloading || !cellAvailable || workspaceAlert !== null || conflict?.kind === "deleted"}>
        {at && <Badge variant="outline">Version {at.version}</Badge>}
      </EditorToolbar>
      {workspaceAlert && <p role="alert" className="text-destructive">{workspaceAlert}</p>}
      {save.isError && !(save.error instanceof StudioScopeError && save.error.message === workspaceAlert) && (
        <p role="alert" className="text-destructive">Not saved. {failure(save.error)}</p>
      )}
      {conflict && !workspaceAlert && (
        <ViewConflictBanner
          conflict={conflict}
          canReload={at !== undefined && onReload !== undefined}
          canOverwrite={at !== undefined}
          canCreate={cellAvailable}
          saving={save.isPending}
          reloading={reloading}
          reloadError={reloadError}
          onReload={() => void reloadTheirs()}
          onOverwrite={(version) => requestSave(editor.value, { confirmedVersion: version })}
          onSaveAsNew={() => requestSave(editor.value, { asNew: true })}
          onDiscard={onDiscard ? discardChanges : undefined}
        />
      )}
      <ValidationPanel issues={issues} title="The cell refused this view" />
      {restatement && (
        <p role="status" className="rounded-lg border border-node-border bg-muted/40 px-3 py-2 text-body text-muted-foreground">
          {restatement}
        </p>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        <label className="grid gap-1.5 text-body" data-field="title">
          <span className={invalid("title") ? "text-destructive" : ""}>View title</span>
          <Input
            aria-label="View title"
            aria-invalid={invalid("title") || undefined}
            placeholder="e.g. Items needing review"
            value={view.title}
            onChange={(e) => set((cur) => ({ ...cur, title: e.target.value }))}
          />
        </label>

        <label className="grid gap-1.5 text-body" data-field="objectType">
          <span className={invalid("objectType") ? "text-destructive" : ""}>Record type</span>
          <select
            aria-label="Record type"
            className={selectCls}
            value={view.objectType}
            onChange={(e) => pickType(e.target.value)}
          >
            {concepts.map((c) => <option key={c.name} value={c.name}>{c.title} ({c.name})</option>)}
          </select>
        </label>

        <label className="grid gap-1.5 text-body" data-field="description">
          <span>Description <span className="font-normal text-muted-foreground">(optional)</span></span>
          <Input
            aria-label="Description"
            placeholder="What this view helps an operator track"
            value={view.description ?? ""}
            onChange={(e) => set((cur) => ({ ...cur, description: e.target.value }))}
          />
        </label>

        <div className="grid gap-1.5 text-body" data-field="space">
          <span>Pin to Space</span>
          <div className="flex flex-wrap gap-2">
            <select
              aria-label="Space"
              className={`${selectCls} min-w-44 flex-1`}
              value={spaceMode === "custom" ? "__custom__" : (view.space ?? "")}
              onChange={(e) => editor.set(selectViewSpace(editor.value, e.target.value))}
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
                <label key={field.id} className="inline-flex items-center gap-1.5 text-body">
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
              <ol className="divide-y rounded-lg border text-body" aria-label="Column order">
                {columns.map((col, idx) => (
                  <li key={`${col}:${idx}`} className="flex items-center justify-between px-2.5 py-1">
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
            {sort.length === 0 && <p className="text-body text-muted-foreground">Default record order.</p>}
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

      <Panel label={`Live preview${previewPage?.total ? ` · ${totalText(previewPage.total)}` : ""}`}>
        <div className="border-t px-4 py-3" data-view-preview="">
        {workspaceAlert
          ? <p className="text-body text-destructive">{workspaceAlert}</p>
          : matchWorkspaceState(workspace, {
            // No workspace is decided yet, or the mode read failed: nothing is read, and nothing cached shows.
            deciding: (state) => <WorkspaceUndecided workspace={state} skeleton="inline" loadingLabel="Checking matching records" />,
            unavailable: (state) => <WorkspaceUndecided workspace={state} />,
            // No cell is connected: the recording neither filters nor sorts, so it previews nothing.
            recorded: () => <p role="status" className="text-body text-muted-foreground">{NO_DATA_CELL}</p>,
            live: () => draftScope !== null && previewScope === null
              ? <p role="alert" className="text-body text-destructive">{WORKSPACE_CHANGED_MESSAGE}</p>
              : <>
              {previewQuery.isPending && <p className="text-body text-muted-foreground">Checking matching records…</p>}
              {previewQuery.isError && <p className="text-body text-muted-foreground">{failure(previewQuery.error)}</p>}
              {previewQuery.data?.kind === "refused" && <p role="alert" className="text-body text-destructive">{previewQuery.data.message}</p>}
              {previewPage && previewPage.read.document.records.length === 0 && (
                <p className="text-body text-muted-foreground">No records currently match this filter.</p>
              )}
              {previewPage && previewPage.read.document.records.length > 0 && (
                <div className="overflow-x-auto">
                  <table className="w-full text-body">
                    <thead>
                      <tr className="border-b text-start text-meta text-muted-foreground">
                        <th className="py-1.5 pe-3 text-start">Record</th>
                        {previewCols.map((col, colIdx) => (
                          <th key={`${col}:${colIdx}`} className="px-3 py-1.5 text-start">{col.replaceAll("_", " ")}</th>
                        ))}
                      </tr>
                    </thead>
                    <tbody className="divide-y">
                      {previewPage.read.document.records.map((rec, index) => {
                        const row: GridRow = { record: rec, at: `/records/${index}`, numbers: previewPage.read.numbers }
                        const withheld = isWithheld(row)
                        return (
                          <tr key={rec.id} data-withheld={withheld || undefined}>
                            <td className="py-1.5 pe-3">
                              {withheld ? rec.id : recordName(rec)}
                              {withheld && previewCols.length === 0 && (
                                <span className="ms-2 font-normal"><WithheldMark title={WITHHELD_HINT}>{WITHHELD_ROW}</WithheldMark></span>
                              )}
                            </td>
                            {withheld ? (
                              previewCols.length > 0 && (
                                <td colSpan={previewCols.length} className="px-3 py-1.5 text-muted-foreground">
                                  <WithheldMark title={WITHHELD_HINT}>{WITHHELD_ROW}</WithheldMark>
                                </td>
                              )
                            ) : (
                              previewCols.map((col, colIdx) => (
                                <td key={`${col}:${colIdx}`} className="px-3 py-1.5 text-muted-foreground">
                                  {propertyWithheld(rec, col) ? <WithheldMark /> : String(rec.values[col] ?? "—")}
                                </td>
                              ))
                            )}
                          </tr>
                        )
                      })}
                    </tbody>
                  </table>
                </div>
              )}
              </>,
          })}
        </div>
      </Panel>
    </div>
  )
}

/** `/views/new` and `/views/$id/edit`: build or update a server-admitted view. */
export function ViewEditorPage() {
  const workspace = useWorkspace()
  const scope = useStudioScope()
  const activeScope = scope.data ?? null
  const cellAvailable = scope.isSuccess && activeScope !== null
  const { id } = useParams({ strict: false }) as { id?: string }
  const { read, catalog } = useCatalog()
  const navigate = useNavigate()
  const client = useQueryClient()
  const concepts = useMemo(() => (catalog?.concepts ?? []).filter((c) => c.definition === "object type"), [catalog])
  const stored = useQuery({
    queryKey: ["view-edit", id, activeScope?.orgId, activeScope?.environmentId],
    queryFn: async () => (id === undefined ? null : (await views.get(id, activeScope)) ?? null),
    enabled: id !== undefined && cellAvailable,
    retry: false,
    staleTime: Infinity,
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })
  const [opened, setOpened] = useState(0)
  const fresh = id === undefined
  // Latch the loaded view so a background scope refetch shows the workspace-changed warning
  // instead of silently swapping the editor onto another workspace's definition.
  const [latched, setLatched] = useState<StoredView | null>(null)
  useEffect(() => { setLatched(null) }, [id])
  useEffect(() => {
    if (!fresh && (latched === null || latched.id !== id) && stored.data && stored.data.id === id && stored.isFetchedAfterMount) {
      setLatched(stored.data)
    }
  }, [fresh, id, latched, stored.data, stored.isFetchedAfterMount])

  const editorStored = fresh ? undefined : (latched?.id === id ? latched : null)

  return (
    <section className="space-y-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="text-title tracking-tight">{fresh ? "New view" : (editorStored?.view.title || stored.data?.view.title || "Edit view")}</h1>
        {id !== undefined && (
          <Button asChild variant="outline" size="sm" className="ms-auto">
            <Link to="/views/$id" params={{ id }}><Eye />Open view</Link>
          </Button>
        )}
      </div>
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="flex items-center gap-2 text-foreground"><span aria-hidden className="size-[6px] shrink-0 rounded-full bg-dot-bad" />{scope.error.message}</p>}
      {(scope.isPending || read.isPending || (!fresh && cellAvailable && latched === null && stored.isPending)) && <p className="text-muted-foreground">Reading view…</p>}
      {read.isError && <EmptyState icon={Bookmark}>{read.error.message}</EmptyState>}
      {latched === null && stored.isError && <EmptyState icon={Bookmark}>{failure(stored.error)}</EmptyState>}
      {!fresh && latched === null && stored.isSuccess && stored.data === null && <EmptyState icon={Bookmark}>No view {id} is saved.</EmptyState>}
      {!scope.isPending && concepts.length === 0 && catalog && (
        <EmptyState icon={Shapes}>No record types are installed in this cell yet.</EmptyState>
      )}
      {!scope.isPending && concepts.length > 0 && (fresh || editorStored !== null) && (
        <ViewEditor
          key={`${editorStored?.id ?? "new"}:${opened}`}
          concepts={concepts}
          stored={editorStored ?? undefined}
          activeScope={activeScope}
          workspace={workspace}
          cellAvailable={cellAvailable}
          onReload={() => reloadConflictingView(
            () => stored.refetch(),
            (next) => {
              setLatched(next)
              setOpened((n) => n + 1)
            },
          )}
          onDiscard={() => {
            void client.invalidateQueries({ queryKey: ["views"] })
            void client.invalidateQueries({ queryKey: ["view", id] })
            void navigate({ to: "/views", replace: true })
          }}
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
