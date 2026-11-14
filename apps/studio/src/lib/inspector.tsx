import { X } from "lucide-react"
import { createContext, use, useState, type ReactNode } from "react"
import { Button } from "@/components/ui/button"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { useIsMobile } from "@/hooks/use-mobile"

/** What the inspector shows: one value or record, with where it came from. */
export interface Inspected {
  readonly title: ReactNode
  readonly body: ReactNode
}

const InspectorContext = createContext<{ readonly inspected?: Inspected; readonly inspect: (inspected?: Inspected) => void }>({ inspect: () => {} })

export function InspectorProvider({ children }: { readonly children: ReactNode }) {
  const [inspected, inspect] = useState<Inspected>()
  return <InspectorContext value={{ inspected, inspect }}>{children}</InspectorContext>
}

/** Open the inspector on something, or close it with no argument. */
export const useInspector = () => use(InspectorContext)

/** Docked to the right of the page on wide screens, a sheet on phones; nothing until something is inspected. */
export function Inspector() {
  const { inspected, inspect } = useInspector()
  const mobile = useIsMobile()
  if (inspected === undefined) return null
  if (mobile) {
    return (
      <Sheet open onOpenChange={(open) => { if (!open) inspect() }}>
        <SheetContent>
          <SheetHeader><SheetTitle>{inspected.title}</SheetTitle><SheetDescription className="sr-only">Inspector</SheetDescription></SheetHeader>
          <div className="overflow-y-auto px-4 pb-4">{inspected.body}</div>
        </SheetContent>
      </Sheet>
    )
  }
  return (
    <aside aria-label="Inspector" className="sticky top-0 hidden h-svh w-80 shrink-0 flex-col border-s bg-background md:flex">
      <div className="flex h-14 items-center gap-2 border-b px-4">
        <div className="min-w-0 flex-1 truncate font-medium">{inspected.title}</div>
        <Button variant="ghost" size="icon" onClick={() => inspect()}><X /><span className="sr-only">Close inspector</span></Button>
      </div>
      <div className="flex-1 overflow-y-auto p-4">{inspected.body}</div>
    </aside>
  )
}
