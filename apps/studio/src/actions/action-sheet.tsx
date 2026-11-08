import { useQuery, useQueryClient } from "@tanstack/react-query"
import { useEffect, useState } from "react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { useView } from "@/lib/view"
import { conceptTitle } from "../lib/catalog.ts"
import { listActions, type ActionSummary, type CatalogRead } from "../lib/actions.ts"
import { ActionForm } from "./action-form.tsx"

/** The actions the cell offers this reader for one object type (the server filters; Studio shows what it gets). */
export function useActions(objectKind: string) {
  return useQuery<CatalogRead>({ queryKey: ["actions", objectKind], queryFn: () => listActions(objectKind), retry: false, staleTime: 60_000 })
}

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
        <SheetContent className="w-[min(36rem,100vw)] gap-0 overflow-y-auto border-node-border shadow-float sm:max-w-xl">
          {open !== null && <>
            <SheetHeader className="px-6 pt-6 pb-4">
              <SheetTitle>{open.action.title}</SheetTitle>
              {technical
                ? <SheetDescription className="font-mono text-xs">{open.action.actionId}</SheetDescription>
                : <SheetDescription className="sr-only">{open.action.title}</SheetDescription>}
            </SheetHeader>
            <div className="px-6">
              <ActionForm key={`${open.action.actionId}:${JSON.stringify(open.prefill ?? {})}`} action={open.action} objectTitle={conceptTitle(open.action.objectKind)}
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
            <DialogDescription>Nothing has been sent to the cell. Your changes to "{open?.action.title}" will be lost.</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setAsking(false)}>Keep editing</Button>
            <Button variant="destructive" onClick={() => { setAsking(false); setDirty(false); close() }}>Discard</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
