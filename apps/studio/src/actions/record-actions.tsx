import { useState } from "react"
import { Button } from "@/components/ui/button"
import { ActionSheet, useActions, type OpenAction } from "./action-sheet.tsx"

/**
 * The governed actions the cell offers this reader on one record's type, each opening its form
 * with the record's identity filled in. The cell decides every call; a refusal (a law the change
 * would break, a precondition) shows on the form with the cell's own words.
 */
export function RecordActions({ objectType, id, onDone }: { readonly objectType: string; readonly id: string; readonly onDone: () => void }) {
  const catalog = useActions(objectType)
  const [acting, setActing] = useState<OpenAction | null>(null)
  if (catalog.data?.kind !== "catalog" || catalog.data.items.length === 0) return null
  return (
    <>
      <div className="flex flex-wrap items-center gap-2" aria-label="Actions on this record">
        {catalog.data.items.map((action) => (
          <Button key={action.actionId} type="button" size="sm" variant="outline" title={action.description ?? action.title}
            onClick={() => setActing({ action, prefill: { [action.objectIdField]: id } })}>{action.title}</Button>
        ))}
      </div>
      <ActionSheet open={acting} onClose={() => { setActing(null); onDone() }} />
    </>
  )
}
