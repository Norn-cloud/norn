import { useQueryClient } from "@tanstack/react-query"
import { useEffect, useRef, useState } from "react"
import { useRouterState } from "@tanstack/react-router"
import { Button } from "@/components/ui/button"
import { suggestedFixInputs, type SuggestedFixIntent } from "../lib/actions.ts"
import { ActionSheet, actionsQueryOptions, useActions, type OpenAction } from "./action-sheet.tsx"

/**
 * The governed actions the cell offers this reader on one record's type, each opening its form
 * with the record's identity filled in. When navigated from a server-offered fix, opens that
 * exact offered action with the server's derived arguments once the cell confirms it is offered.
 * The cell decides every call; a refusal (a law the change would break, a precondition) shows on
 * the form with the cell's own words.
 */
export function RecordActions({ objectType, id, onDone }: { readonly objectType: string; readonly id: string; readonly onDone: () => void }) {
  // The cell lists what can act on this record now: a transition its state can't take is not offered.
  const client = useQueryClient()
  const catalog = useActions(objectType, id)
  const [acting, setActing] = useState<OpenAction | null>(null)
  const items = catalog.data?.kind === "catalog" ? catalog.data.items : []
  const intent = useRouterState({ select: (state) => (state.location.state as { readonly suggestedFix?: SuggestedFixIntent } | undefined)?.suggestedFix })
  const matches = intent !== undefined && intent.objectType === objectType && intent.id === id
  const handled = useRef<SuggestedFixIntent | null>(null)
  useEffect(() => {
    if (!matches || handled.current === intent) return
    let cancelled = false
    // Always await a live catalog read for this record: cached data from an earlier visit must not
    // consume the intent or open a stale action.
    void client.fetchQuery(actionsQueryOptions(objectType, id)).then((live) => {
      if (cancelled) return
      handled.current = intent
      if (live.kind !== "catalog") return
      const action = live.items.find((item) => item.actionId === intent.fix.actionId)
      if (action === undefined) return
      const inputs = suggestedFixInputs(action, id, intent.fix)
      if (inputs === null) return
      setActing({ action, ...inputs })
    }).catch(() => {
      // A failed live catalog read leaves manual action buttons intact without consuming stale cache.
    })
    return () => { cancelled = true }
  }, [client, matches, intent, objectType, id])
  // The sheet stays mounted while open: a refetch that offers nothing (the record moved on) must
  // not discard an open form or its receipt.
  if (items.length === 0 && acting === null) return null
  return (
    <>
      {items.length > 0 && <div className="flex flex-wrap items-center gap-2" aria-label="Actions on this record">
        {items.map((action) => (
          <Button key={action.actionId} type="button" size="sm" variant="outline" title={action.description ?? action.title}
            onClick={() => setActing({ action, prefill: { [action.objectIdField]: id } })}>{action.title}</Button>
        ))}
      </div>}
      <ActionSheet open={acting} onClose={() => { setActing(null); onDone() }} />
    </>
  )
}
