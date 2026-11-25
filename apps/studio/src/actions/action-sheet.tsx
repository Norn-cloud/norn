import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query"
import { useEffect, useMemo, useRef, useState } from "react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { useLocale } from "@/lib/locale"
import { useView } from "@/lib/view"
import { conceptTitle } from "../lib/catalog.ts"
import { labelIndex, pick } from "../lib/labels.ts"
import { listActions, callAction, type ActionSummary, type CatalogRead, type Draft } from "../lib/actions.ts"
import { scopeFromAccount, studioScopeKey, type StudioScope } from "../lib/definitions.ts"
import { profileQueryPrefix } from "../lib/profile.ts"
import { recordsQueryPrefix } from "../lib/records-query.ts"
import { useAccount, usePinnedScope } from "../lib/session.ts"
import { useCatalog } from "../lib/use-catalog.ts"
import { ActionForm, type EnumLabelLookup } from "./action-form.tsx"
import { keptDrafts, sessionKey } from "./kept-drafts.ts"
import { parkedCalls } from "./parked-calls.ts"
import type { ParkedCall } from "../lib/screen/call.ts"

export interface ActionScopeBinding {
  readonly scope: StudioScope | null
  readonly onScopeMismatch?: (actual: StudioScope | null | undefined) => void
}

/**
 * The actions the cell offers this reader for one object type, or for one record in its current
 * state (the server filters; Studio shows what it gets). When `scopeBinding` is supplied, the
 * query is keyed by that scope and refuses to hit another scope if `/api/me` moves.
 */
export function useActions(objectKind: string, objectId?: string, enabled = true, scopeBinding?: ActionScopeBinding) {
  const scopeKey = scopeBinding === undefined ? undefined : studioScopeKey(scopeBinding.scope)
  return useQuery<CatalogRead>({
    queryKey: scopeKey === undefined ? ["actions", objectKind, objectId ?? null] : ["actions", objectKind, objectId ?? null, scopeKey],
    queryFn: ({ signal }) => listActions(
      objectKind,
      undefined,
      objectId,
      scopeBinding === undefined
        ? { signal }
        : {
          signal,
          scope: scopeBinding.scope,
          ...(scopeBinding.onScopeMismatch === undefined ? {} : { onScopeMismatch: scopeBinding.onScopeMismatch }),
        },
    ),
    retry: false,
    staleTime: objectId === undefined ? 60_000 : 0,
    enabled,
  })
}

const draftKey = (open: OpenAction) => `${open.action.actionId}:${JSON.stringify(open.prefill ?? {})}`

export interface OpenAction {
  readonly action: ActionSummary
  readonly prefill?: Readonly<Record<string, string>>
  /** Open on this parked call (the inbox's "Approved, run it"), as the cell serves it. */
  readonly parked?: ParkedCall
  /** Run the parked call as soon as the cell offers it. */
  readonly runNow?: boolean
}

export function invalidateActionReads(client: QueryClient, objectKind: string) {
  void client.invalidateQueries({ queryKey: recordsQueryPrefix(objectKind) })
  void client.invalidateQueries({ queryKey: ["facets", objectKind] })
  // Lane links and empty-state nearest jumps can change when an action writes links or intervals.
  void client.invalidateQueries({ queryKey: ["timeline-lane"] })
  void client.invalidateQueries({ queryKey: ["timeline-nearest"] })
  void client.invalidateQueries({ queryKey: ["inbox"] })
  void client.invalidateQueries({ queryKey: profileQueryPrefix(objectKind) })
  void client.invalidateQueries({ queryKey: ["quarantined", objectKind] })
}

/**
 * A governed action's form in a sheet. Closing a draft with unsent changes asks first; so does
 * leaving the page. After a commit the object type's records are read again.
 */
export function ActionSheet({ open, onClose, onView, onChanged, scopeBinding }: {
  readonly open: OpenAction | null; readonly onClose: () => void; readonly onView?: (objectId: string) => void
  /** The cell committed something: besides the type's records, read whatever else shows it again. */
  readonly onChanged?: () => void
  /** Optional verified scope binding so commits never cross onto a different cell environment. */
  readonly scopeBinding?: ActionScopeBinding
}) {
  const { view } = useView()
  const { locale } = useLocale()
  const { read: catalogRead } = useCatalog()
  const account = useAccount().data
  const pinnedScope = usePinnedScope(open === null ? false : `${open.action.actionId}:${open.prefill?.[open.action.objectIdField] ?? ""}`)
  const effectiveScope: StudioScope | null | undefined =
    scopeBinding !== undefined
      ? scopeBinding.scope
      : pinnedScope !== undefined
        ? pinnedScope
        : account === undefined ? undefined : scopeFromAccount(account)
  const boundCall = useMemo(() => {
    if (effectiveScope === undefined) return undefined
    const onScopeMismatch = scopeBinding?.onScopeMismatch
    return (actionId: string, input: string, key: string) =>
      callAction(actionId, input, key, undefined, { scope: effectiveScope, ...(onScopeMismatch === undefined ? {} : { onScopeMismatch }) })
  }, [effectiveScope, scopeBinding?.onScopeMismatch])
  const enumLabel = useMemo<EnumLabelLookup>(() => {
    const artifacts = catalogRead.data?.document
    if (open === null || artifacts === undefined) return () => undefined
    const index = labelIndex(artifacts)
    const objectKind = open.action.objectKind
    return (pointer, value) => {
      const property = pointer.split("/").filter(Boolean).at(-1)
      if (property === undefined) return undefined
      return pick(index, `enumValue:${objectKind}.${property}=${value}`, locale)?.text
    }
  }, [catalogRead.data, open, locale])
  // Kept drafts belong to the session that wrote them (#1469 S6).
  const session = sessionKey(useAccount().data)
  const kept = { get: (k: string) => keptDrafts.get(session, k), set: (k: string, d: Draft) => keptDrafts.set(session, k, d), delete: (k: string) => keptDrafts.delete(session, k) }
  const technical = view === "technical"
  const [dirty, setDirty] = useState(false)
  const [asking, setAsking] = useState(false)
  const latest = useRef<Draft | null>(null)
  const [restored, setRestored] = useState<Draft | undefined>(undefined)
  const [generation, setGeneration] = useState(0)
  const [banner, setBanner] = useState(false)
  useEffect(() => {
    const draft = open === null ? undefined : keptDrafts.get(session, draftKey(open))
    latest.current = draft ?? null
    setRestored(draft)
    setBanner(draft !== undefined)
  }, [open, session])
  const client = useQueryClient()
  // A call this form sent that parked: the form reopens on it until it commits or a new draft starts.
  // Read when the sheet opens on a form; the form reports later changes to the store, not to this value.
  const parked = useMemo<ParkedCall | undefined>(() => open === null ? undefined : open.parked ?? parkedCalls.get(session, draftKey(open)), [open, session])
  useEffect(() => { if (open === null) { setDirty(false); setAsking(false) } }, [open])
  // A form opened as one reader never stays open as another (a tenant switch, an expiry, someone
  // else signing in): its parked call and draft were that reader's, so the sheet closes.
  const openedAs = useRef(session)
  useEffect(() => { openedAs.current = session }, [open])
  useEffect(() => { if (open !== null && openedAs.current !== session) { setDirty(false); setAsking(false); onClose() } }, [session])
  useEffect(() => {
    if (!dirty) return
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault() }
    window.addEventListener("beforeunload", warn)
    return () => window.removeEventListener("beforeunload", warn)
  }, [dirty])
  const close = () => {
    if (open !== null) {
      invalidateActionReads(client, open.action.objectKind)
      // A record's offered actions follow its state.
      void client.invalidateQueries({ queryKey: ["actions", open.action.objectKind] })
      // A parked call that ran, or one that parked, moves the requester's list.
      void client.invalidateQueries({ queryKey: ["decisions"] })
    }
    onClose()
  }
  const request = () => (dirty ? setAsking(true) : close())
  return (
    <>
      <Sheet open={open !== null} onOpenChange={(next) => { if (!next) request() }}>
        <SheetContent className="w-[min(36rem,100vw)] gap-0 overflow-y-auto border-node-border shadow-float sm:max-w-xl"
          onOpenAutoFocus={(event) => {
            // Start in the first field to fill, not on the close button or the wizard's step bar.
            const first = (event.currentTarget as HTMLElement | null)?.querySelector<HTMLElement>("form input:not([readonly]), form select, form textarea")
            if (first) { event.preventDefault(); first.focus() }
          }}>
          {open !== null && <>
            <SheetHeader className="px-6 pt-6 pb-4">
              <SheetTitle>{open.action.title}</SheetTitle>
              {technical
                ? <SheetDescription className="font-mono text-xs">{open.action.actionId}</SheetDescription>
                : <SheetDescription className="sr-only">{open.action.title}</SheetDescription>}
            </SheetHeader>
            <div className="px-6">
              {banner && parked === undefined && (
                <p role="status" className="mb-4 flex flex-wrap items-center gap-2 rounded-lg border border-node-border bg-muted/50 px-3 py-2 text-sm">
                  Restored your draft from earlier in this session.
                  <span className="flex-1" />
                  <Button type="button" variant="ghost" size="sm" className="h-7" onClick={() => { kept.delete(draftKey(open)); latest.current = null; setRestored(undefined); setBanner(false); setDirty(false); setGeneration((g) => g + 1) }}>Start over</Button>
                </p>
              )}
              <ActionForm key={`${session}:${draftKey(open)}:${generation}:${restored === undefined ? "new" : "kept"}:${parked?.decision.id ?? "draft"}`} action={open.action} objectTitle={conceptTitle(open.action.objectKind)}
                remembered={parked === undefined ? restored : undefined} onDraft={(draft) => { latest.current = draft }}
                onCommitted={() => { kept.delete(draftKey(open)); parkedCalls.delete(session, draftKey(open)); setBanner(false) }}
                parked={parked} runNow={open.runNow === true && parked === open.parked}
                onParked={(call) => parkedCalls.set(session, draftKey(open), call)} onUnparked={() => parkedCalls.delete(session, draftKey(open))}
                onChanged={() => { invalidateActionReads(client, open.action.objectKind); onChanged?.() }}
                {...(boundCall === undefined ? {} : { call: boundCall })}
                enumLabel={enumLabel}
                technical={technical} prefill={open.prefill} onDirty={setDirty} onClose={close}
                onView={onView === undefined ? undefined : (id) => { close(); onView(id) }} />
            </div>
          </>}
        </SheetContent>
      </Sheet>
      <Dialog open={asking} onOpenChange={setAsking}>
        <DialogContent className="rounded-node border-node-border shadow-float">
          <DialogHeader>
            <DialogTitle>Discard this draft?</DialogTitle>
            <DialogDescription>Nothing has been sent to the cell. Keep the draft to finish it later in this session, or discard it.</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setAsking(false)}>Keep editing</Button>
            <Button variant="outline" className="border-node-border" onClick={() => {
              if (open !== null && latest.current !== null) kept.set(draftKey(open), latest.current)
              setAsking(false); setDirty(false); close()
            }}>Keep draft</Button>
            <Button variant="destructive" onClick={() => { if (open !== null) kept.delete(draftKey(open)); setAsking(false); setDirty(false); close() }}>Discard</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
