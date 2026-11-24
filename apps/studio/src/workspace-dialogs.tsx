import { useQueryClient } from "@tanstack/react-query"
import { useNavigate } from "@tanstack/react-router"
import { ArchiveRestore, Check, ExternalLink, Trash2, X } from "lucide-react"
import { useState, type FormEvent } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import {
  WORKSPACES_QUERY_KEY,
  WorkspaceError,
  switchWorkspaceCaches,
  workspaceApi,
  type WorkspaceItem,
  type WorkspaceRequestItem,
} from "@/lib/workspaces-api"

export type WorkspaceDialogState =
  | { readonly kind: "closed" }
  | { readonly kind: "create" }
  | { readonly kind: "plan-limit"; readonly reason: string; readonly upgradeUrl: string | null }
  | { readonly kind: "rename"; readonly workspace: WorkspaceItem }
  | { readonly kind: "archive"; readonly workspace: WorkspaceItem }
  | { readonly kind: "delete"; readonly workspace: WorkspaceItem }
  | { readonly kind: "archived" }
  | { readonly kind: "request" }

function ErrorLine({ error }: { readonly error: string | null }) {
  if (error === null) return null
  return <p role="alert" className="text-meta text-destructive">{error}</p>
}

function errorText(error: unknown): string {
  return error instanceof WorkspaceError ? error.message : "The server did not answer. Try again."
}

export function WorkspaceDialogs({
  state,
  archived,
  requests,
  onClose,
  onArchived,
  onCleared,
}: {
  readonly state: WorkspaceDialogState
  readonly archived: readonly WorkspaceItem[]
  readonly requests: readonly WorkspaceRequestItem[]
  readonly onClose: () => void
  readonly onArchived: (workspace: { readonly id: string; readonly name: string }) => void
  readonly onCleared: (id: string) => void
}) {
  return (
    <>
      {state.kind === "create" && <CreateDialog onClose={onClose} />}
      {state.kind === "plan-limit" && <PlanLimitDialog reason={state.reason} upgradeUrl={state.upgradeUrl} onClose={onClose} />}
      {state.kind === "rename" && <RenameDialog workspace={state.workspace} onClose={onClose} />}
      {state.kind === "archive" && <ArchiveDialog workspace={state.workspace} onClose={onClose} onArchived={onArchived} />}
      {state.kind === "delete" && <DeleteDialog workspace={state.workspace} onClose={onClose} onCleared={onCleared} />}
      {state.kind === "archived" && <ArchivedDialog archived={archived} onClose={onClose} onCleared={onCleared} />}
      {state.kind === "request" && <RequestDialog requests={requests} onClose={onClose} />}
    </>
  )
}

function CreateDialog({ onClose }: { readonly onClose: () => void }) {
  const [name, setName] = useState("")
  const [pending, setPending] = useState(false)
  const [committed, setCommitted] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const client = useQueryClient()
  const navigate = useNavigate()

  async function refreshAfterCreate() {
    setPending(true)
    setError(null)
    try {
      await switchWorkspaceCaches(client)
      onClose()
      await navigate({ to: "/" })
    } catch (err) {
      setPending(false)
      setError(errorText(err))
    }
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (committed) { void refreshAfterCreate(); return }
    const trimmed = name.trim()
    if (trimmed === "") return
    setPending(true)
    setError(null)
    try {
      await workspaceApi.create(trimmed)
      setCommitted(true)
    } catch (err) {
      setPending(false)
      setError(errorText(err))
      return
    }
    await refreshAfterCreate()
  }

  return (
    <Dialog open onOpenChange={(open) => { if (!open && !pending) onClose() }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New workspace</DialogTitle>
          <DialogDescription>Create an isolated workspace with its own cell. You will start as its owner.</DialogDescription>
        </DialogHeader>
        <form onSubmit={(event) => void submit(event)} className="grid gap-4">
          <label className="grid gap-1.5 text-sm">
            <span className="font-medium">Workspace name</span>
            <Input required autoFocus maxLength={80} disabled={committed} placeholder="e.g. Operations" value={name} onChange={(event) => setName(event.target.value)} />
          </label>
          <ErrorLine error={error} />
          <DialogFooter>
            <Button type="button" variant="outline" disabled={pending} onClick={onClose}>Cancel</Button>
            <Button type="submit" disabled={pending || (!committed && name.trim() === "")}>
              {pending ? (committed ? "Refreshing…" : "Creating…") : committed ? "Retry refresh" : "Create workspace"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function PlanLimitDialog({ reason, upgradeUrl, onClose }: { readonly reason: string; readonly upgradeUrl: string | null; readonly onClose: () => void }) {
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose() }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Workspace limit reached</DialogTitle>
          <DialogDescription>{reason}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose}>Close</Button>
          {upgradeUrl && (
            <Button asChild>
              <a href={upgradeUrl}>Upgrade plan<ExternalLink aria-hidden className="size-3.5" /></a>
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function RenameDialog({ workspace, onClose }: { readonly workspace: WorkspaceItem; readonly onClose: () => void }) {
  const [name, setName] = useState(workspace.name)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const client = useQueryClient()

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const trimmed = name.trim()
    if (trimmed === "") return
    setPending(true)
    setError(null)
    try {
      await workspaceApi.rename(workspace.id, trimmed)
      await Promise.all([
        client.invalidateQueries({ queryKey: ["session"] }),
        client.invalidateQueries({ queryKey: WORKSPACES_QUERY_KEY }),
      ])
      onClose()
    } catch (err) {
      setPending(false)
      setError(errorText(err))
    }
  }

  return (
    <Dialog open onOpenChange={(open) => { if (!open && !pending) onClose() }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Rename workspace</DialogTitle>
          <DialogDescription>Choose a clear name for this workspace. Its cell and records stay untouched.</DialogDescription>
        </DialogHeader>
        <form onSubmit={(event) => void submit(event)} className="grid gap-4">
          <label className="grid gap-1.5 text-sm">
            <span className="font-medium">Workspace name</span>
            <Input required autoFocus maxLength={80} value={name} onChange={(event) => setName(event.target.value)} />
          </label>
          <ErrorLine error={error} />
          <DialogFooter>
            <Button type="button" variant="outline" disabled={pending} onClick={onClose}>Cancel</Button>
            <Button type="submit" disabled={pending || name.trim() === "" || name.trim() === workspace.name}>{pending ? "Saving…" : "Save name"}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function ArchiveDialog({ workspace, onClose, onArchived }: {
  readonly workspace: WorkspaceItem
  readonly onClose: () => void
  readonly onArchived: (workspace: { readonly id: string; readonly name: string }) => void
}) {
  const [pending, setPending] = useState(false)
  const [committed, setCommitted] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const client = useQueryClient()
  const navigate = useNavigate()

  async function refreshAfterArchive() {
    setPending(true)
    setError(null)
    try {
      await switchWorkspaceCaches(client)
      onArchived({ id: workspace.id, name: workspace.name })
      onClose()
      await navigate({ to: "/" })
    } catch (err) {
      setPending(false)
      setError(errorText(err))
    }
  }

  async function confirm() {
    if (committed) { void refreshAfterArchive(); return }
    setPending(true)
    setError(null)
    try {
      await workspaceApi.archive(workspace.id)
      setCommitted(true)
    } catch (err) {
      setPending(false)
      setError(errorText(err))
      return
    }
    await refreshAfterArchive()
  }

  return (
    <Dialog open onOpenChange={(open) => { if (!open && !pending) onClose() }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Archive “{workspace.name}”?</DialogTitle>
          <DialogDescription>Archiving hides this workspace from your switcher and suspends access to it. You can restore it at any time from Archived workspaces.</DialogDescription>
        </DialogHeader>
        <ErrorLine error={error} />
        <DialogFooter>
          <Button type="button" variant="outline" disabled={pending} onClick={onClose}>Cancel</Button>
          <Button type="button" variant={committed ? "default" : "destructive"} disabled={pending} onClick={() => void confirm()}>
            {pending ? (committed ? "Refreshing…" : "Archiving…") : committed ? "Retry refresh" : "Archive workspace"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function DeleteDialog({ workspace, onClose, onCleared }: { readonly workspace: WorkspaceItem; readonly onClose: () => void; readonly onCleared: (id: string) => void }) {
  const [pending, setPending] = useState(false)
  const [committed, setCommitted] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const client = useQueryClient()
  const navigate = useNavigate()

  async function refreshAfterDelete() {
    setPending(true)
    setError(null)
    try {
      await switchWorkspaceCaches(client)
      onCleared(workspace.id)
      onClose()
      await navigate({ to: "/" })
    } catch (err) {
      setPending(false)
      setError(errorText(err))
    }
  }

  async function confirm() {
    if (committed) { void refreshAfterDelete(); return }
    setPending(true)
    setError(null)
    try {
      await workspaceApi.remove(workspace.id)
      setCommitted(true)
      onCleared(workspace.id)
    } catch (err) {
      setPending(false)
      setError(errorText(err))
      return
    }
    await refreshAfterDelete()
  }

  return (
    <Dialog open onOpenChange={(open) => { if (!open && !pending) onClose() }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete “{workspace.name}” permanently?</DialogTitle>
          <DialogDescription>This removes the workspace, its memberships, invitations, and API keys permanently. This action cannot be undone.</DialogDescription>
        </DialogHeader>
        <ErrorLine error={error} />
        <DialogFooter>
          <Button type="button" variant="outline" disabled={pending} onClick={onClose}>Cancel</Button>
          <Button type="button" variant={committed ? "default" : "destructive"} disabled={pending} onClick={() => void confirm()}>
            {pending ? (committed ? "Refreshing…" : "Deleting…") : committed ? "Retry refresh" : "Delete permanently"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ArchivedDialog({ archived, onClose, onCleared }: { readonly archived: readonly WorkspaceItem[]; readonly onClose: () => void; readonly onCleared: (id: string) => void }) {
  const [busyId, setBusyId] = useState<string | null>(null)
  const [restoredId, setRestoredId] = useState<string | null>(null)
  const [confirmDelete, setConfirmDelete] = useState<WorkspaceItem | null>(null)
  const [error, setError] = useState<string | null>(null)
  const client = useQueryClient()
  const navigate = useNavigate()

  async function refreshAfterRestore(id: string) {
    setBusyId(id)
    setError(null)
    try {
      await switchWorkspaceCaches(client)
      onCleared(id)
      onClose()
      await navigate({ to: "/" })
    } catch (err) {
      setBusyId(null)
      setError(errorText(err))
    }
  }

  async function restore(item: WorkspaceItem) {
    if (restoredId === item.id) { void refreshAfterRestore(item.id); return }
    setBusyId(item.id)
    setError(null)
    try {
      await workspaceApi.restore(item.id, true)
      setRestoredId(item.id)
      onCleared(item.id)
    } catch (err) {
      setBusyId(null)
      setError(errorText(err))
      return
    }
    await refreshAfterRestore(item.id)
  }

  async function remove(item: WorkspaceItem) {
    setBusyId(item.id)
    setError(null)
    try {
      await workspaceApi.remove(item.id)
      onCleared(item.id)
      await client.invalidateQueries({ queryKey: WORKSPACES_QUERY_KEY })
      setConfirmDelete(null)
      setBusyId(null)
    } catch (err) {
      setBusyId(null)
      setError(errorText(err))
    }
  }

  return (
    <Dialog open onOpenChange={(open) => { if (!open && busyId === null) onClose() }}>
      <DialogContent className="sm:max-w-lg">
        {confirmDelete !== null ? (
          <>
            <DialogHeader>
              <DialogTitle>Delete “{confirmDelete.name}” permanently?</DialogTitle>
              <DialogDescription>This removes the workspace, its memberships, invitations, and API keys permanently. This action cannot be undone.</DialogDescription>
            </DialogHeader>
            <ErrorLine error={error} />
            <DialogFooter>
              <Button type="button" variant="outline" disabled={busyId !== null} onClick={() => { setConfirmDelete(null); setError(null) }}>Cancel</Button>
              <Button type="button" variant="destructive" disabled={busyId !== null} onClick={() => void remove(confirmDelete)}>
                {busyId === confirmDelete.id ? "Deleting…" : "Delete permanently"}
              </Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Archived workspaces</DialogTitle>
              <DialogDescription>Restore a workspace to reopen it, or delete it permanently.</DialogDescription>
            </DialogHeader>
            <ErrorLine error={error} />
            {archived.length === 0 ? (
              <p className="text-sm text-muted-foreground">No archived workspaces.</p>
            ) : (
              <ul className="divide-y rounded-lg border">
                {archived.map((item) => (
                  <li key={item.id} className="flex items-center justify-between gap-3 px-3 py-2.5 text-sm">
                    <span className="min-w-0 truncate font-medium">{item.name}</span>
                    <div className="flex shrink-0 items-center gap-1.5">
                      <Button type="button" size="xs" variant="outline" disabled={busyId !== null} onClick={() => void restore(item)}>
                        <ArchiveRestore aria-hidden className="size-3.5" />{restoredId === item.id ? "Retry refresh" : "Restore"}
                      </Button>
                      {item.canDelete && restoredId !== item.id && (
                        <Button type="button" size="xs" variant="destructive" disabled={busyId !== null} onClick={() => { setError(null); setConfirmDelete(item) }}>
                          <Trash2 aria-hidden className="size-3.5" />Delete
                        </Button>
                      )}
                    </div>
                  </li>
                ))}
              </ul>
            )}
            <DialogFooter>
              <Button type="button" variant="outline" disabled={busyId !== null} onClick={onClose}>Close</Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  )
}

function RequestDialog({ requests, onClose }: { readonly requests: readonly WorkspaceRequestItem[]; readonly onClose: () => void }) {
  const [name, setName] = useState("")
  const [note, setNote] = useState("")
  const [pending, setPending] = useState(false)
  const [busyId, setBusyId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [submitted, setSubmitted] = useState(false)
  const client = useQueryClient()

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const trimmed = name.trim()
    if (trimmed === "") return
    setPending(true)
    setError(null)
    setSubmitted(false)
    try {
      await workspaceApi.requestWorkspace(trimmed, note.trim())
      setName("")
      setNote("")
      setSubmitted(true)
      await client.invalidateQueries({ queryKey: WORKSPACES_QUERY_KEY })
    } catch (err) {
      setError(errorText(err))
    } finally {
      setPending(false)
    }
  }

  async function decide(id: string, decision: "approved" | "dismissed") {
    setBusyId(id)
    setError(null)
    try {
      await workspaceApi.decideRequest(id, decision)
      await client.invalidateQueries({ queryKey: WORKSPACES_QUERY_KEY })
    } catch (err) {
      setError(errorText(err))
    } finally {
      setBusyId(null)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => { if (!open && !pending && busyId === null) onClose() }}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Request a workspace</DialogTitle>
          <DialogDescription>Self-serve workspace provisioning is not enabled on this deployment. Record a request for your admin to provision a workspace and cell.</DialogDescription>
        </DialogHeader>
        <form onSubmit={(event) => void submit(event)} className="grid gap-3">
          <label className="grid gap-1.5 text-sm">
            <span className="font-medium">Workspace name</span>
            <Input required autoFocus maxLength={80} placeholder="e.g. Regional Operations" value={name} onChange={(event) => setName(event.target.value)} />
          </label>
          <label className="grid gap-1.5 text-sm">
            <span className="font-medium">Note (optional)</span>
            <Textarea maxLength={500} rows={2} placeholder="What this workspace will be used for" value={note} onChange={(event) => setNote(event.target.value)} />
          </label>
          {submitted && <p role="status" className="text-meta text-muted-foreground">Request recorded. An admin will review it.</p>}
          <ErrorLine error={error} />
          <div className="flex justify-end">
            <Button type="submit" disabled={pending || name.trim() === ""}>{pending ? "Sending…" : "Submit request"}</Button>
          </div>
        </form>
        {requests.length > 0 && (
          <div className="space-y-2 border-t pt-3">
            <h3 className="text-xs font-medium text-muted-foreground">Workspace requests</h3>
            <ul className="max-h-56 divide-y overflow-y-auto rounded-lg border">
              {requests.map((item) => (
                <li key={item.id} className="flex items-start justify-between gap-3 px-3 py-2 text-sm">
                  <div className="min-w-0 space-y-0.5">
                    <div className="flex items-center gap-2">
                      <span className="truncate font-medium">{item.name}</span>
                      <Badge variant={item.status === "pending" ? "secondary" : "outline"}>{item.status}</Badge>
                    </div>
                    <p className="truncate text-meta text-muted-foreground">{item.requesterName || item.requesterEmail}</p>
                    {item.note && <p className="text-meta text-muted-foreground">{item.note}</p>}
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    {item.canManage && (
                      <>
                        <Button type="button" size="xs" variant="outline" disabled={busyId !== null} onClick={() => void decide(item.id, "approved")}>
                          <Check aria-hidden className="size-3.5" />Approve
                        </Button>
                        <Button type="button" size="xs" variant="ghost" disabled={busyId !== null} onClick={() => void decide(item.id, "dismissed")}>
                          <X aria-hidden className="size-3.5" />Dismiss
                        </Button>
                      </>
                    )}
                    {!item.canManage && item.canWithdraw && (
                      <Button type="button" size="xs" variant="ghost" disabled={busyId !== null} onClick={() => void decide(item.id, "dismissed")}>
                        Withdraw
                      </Button>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          </div>
        )}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={pending || busyId !== null} onClick={onClose}>Close</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
