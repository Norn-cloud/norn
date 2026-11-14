import { useQuery, useQueryClient } from "@tanstack/react-query"
import { useEffect, useRef, useState } from "react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { useView } from "@/lib/view"
import { conceptTitle } from "../lib/catalog.ts"
import { listActions, type ActionSummary, type CatalogRead, type Draft } from "../lib/actions.ts"
import { ActionForm } from "./action-form.tsx"

/** The actions the cell offers this reader for one object type (the server filters; Studio shows what it gets). */
export function useActions(objectKind: string) {
  return useQuery<CatalogRead>({ queryKey: ["actions", objectKind], queryFn: () => listActions(objectKind), retry: false, staleTime: 60_000 })
}

/**
 * Drafts kept for later in this tab, by action and the record it was opened on. Memory only:
 * nothing is written to the cell or the browser's storage, and a reload forgets them.
 */
const kept = new Map<string, Draft>()
const draftKey = (open: OpenAction) => `${open.action.actionId}:${JSON.stringify(open.prefill ?? {})}`

export interface OpenAction { readonly action: ActionSummary; readonly prefill?: Readonly<Record<string, string>> }

/**
 * A governed action's form in a sheet. Closing a draft with unsent changes asks first; so does
 * leaving the page. After a commit the object type's records are read again.
 */
export function ActionSheet({ open, onClose, onView }: { readonly open: OpenAction | null; readonly onClose: () => void; readonly onView?: (objectId: string) => void }) {
  const { view } = useView()
  const technical = view === "technical"
  const [dirty, setDirty] = useState(false)
  const [asking, setAsking] = useState(false)
  const latest = useRef<Draft | null>(null)
  const [restored, setRestored] = useState<Draft | undefined>(undefined)
  const [generation, setGeneration] = useState(0)
  const [banner, setBanner] = useState(false)
  useEffect(() => {
    const draft = open === null ? undefined : kept.get(draftKey(open))
    latest.current = draft ?? null
    setRestored(draft)
    setBanner(draft !== undefined)
  }, [open])
  const client = useQueryClient()
  useEffect(() => { if (open === null) { setDirty(false); setAsking(false) } }, [open])
  useEffect(() => {
    if (!dirty) return
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault() }
    window.addEventListener("beforeunload", warn)
    return () => window.removeEventListener("beforeunload", warn)
  }, [dirty])
  const close = () => {
    if (open !== null) void client.invalidateQueries({ queryKey: ["records", open.action.objectKind] })
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
              {banner && (
                <p role="status" className="mb-4 flex flex-wrap items-center gap-2 rounded-lg border border-node-border bg-muted/50 px-3 py-2 text-sm">
                  Restored your draft from earlier in this session.
                  <span className="flex-1" />
                  <Button type="button" variant="ghost" size="sm" className="h-7" onClick={() => { kept.delete(draftKey(open)); latest.current = null; setRestored(undefined); setBanner(false); setDirty(false); setGeneration((g) => g + 1) }}>Start over</Button>
                </p>
              )}
              <ActionForm key={`${draftKey(open)}:${generation}:${restored === undefined ? "new" : "kept"}`} action={open.action} objectTitle={conceptTitle(open.action.objectKind)}
                remembered={restored} onDraft={(draft) => { latest.current = draft }} onCommitted={() => { kept.delete(draftKey(open)); setBanner(false) }}
                onChanged={() => void client.invalidateQueries({ queryKey: ["records", open.action.objectKind] })}
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
