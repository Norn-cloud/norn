import { useQueryClient } from "@tanstack/react-query"
import { ChevronDown, Play, Plus } from "lucide-react"
import { useState } from "react"
import { ActionSheet, useActions, type OpenAction } from "../actions/action-sheet.tsx"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Skeleton } from "@/components/ui/skeleton"
import { ACTIONS_UNAVAILABLE, writesWhole, type ActionSummary } from "../lib/actions.ts"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import { offeredFor, recordCatalog } from "../lib/screen/call.ts"
import type { ObjectRecord } from "../lib/transport.ts"

/**
 * Type-level creation actions the cell offers for `objectKind` (`writesWhole`). Studio never
 * fabricates an action or hides one on its own: it renders only what `GET /v1/actions` serves,
 * and opens each in the server-built `ActionSheet`.
 */
export function TypeActions({ objectKind, prefill, onChanged, label, primary = false, showUnavailable = false }: {
  readonly objectKind: string
  readonly prefill?: Readonly<Record<string, string>>
  readonly onChanged?: () => void
  /** Override label when several create actions group behind one menu (e.g. "New eval set"). */
  readonly label?: string
  readonly primary?: boolean
  readonly showUnavailable?: boolean
}) {
  const catalog = useActions(objectKind)
  const [acting, setActing] = useState<OpenAction | null>(null)
  const off = catalog.isError
    ? (catalog.error instanceof Error ? catalog.error.message : ACTIONS_UNAVAILABLE)
    : catalog.data?.kind === "unavailable" ? ACTIONS_UNAVAILABLE
      : catalog.data?.kind === "refused" ? catalog.data.message
        : catalog.data?.kind === "no-cell" ? NO_DATA_CELL : undefined
  const creates = catalog.data?.kind === "catalog" ? catalog.data.items.filter(writesWhole) : []
  const open = (action: ActionSummary) => setActing({ action, ...(prefill === undefined ? {} : { prefill }) })
  return (
    <>
      {off !== undefined
        ? (showUnavailable ? <Button type="button" size="sm" variant={primary ? "default" : "outline"} disabled title={off}><Plus /> {label ?? "New"}</Button> : null)
        : catalog.isPending || creates.length === 0 ? null
          : creates.length === 1
            ? (
              <Button type="button" size="sm" variant={primary ? "default" : "outline"} onClick={() => open(creates[0]!)}>
                <Plus /> {label ?? creates[0]!.title}
              </Button>
            )
            : (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button type="button" size="sm" variant={primary ? "default" : "outline"}>
                    <Plus /> {label ?? "Actions"}<ChevronDown />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="rounded-node border-node-border shadow-float">
                  {creates.map((action) => (
                    <DropdownMenuItem key={action.actionId} onSelect={() => open(action)}>{action.title}</DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>
            )}
      <ActionSheet open={acting} onClose={() => setActing(null)} onChanged={onChanged} />
    </>
  )
}

/**
 * Per-record governed actions offered by the cell for one record in its current state:
 * `GET /v1/actions?objectKind={objectKind}&objectId={record.id}` (ACTION-CATALOG-STATE-1),
 * further narrowed by `record.offers` when the record carries it. Studio holds no permission or
 * lifecycle logic; the cell decides which buttons appear and enforces every call at commit time.
 *
 * The `ActionSheet` stays mounted across catalog refetches so receipts and server-offered Undo
 * remain usable immediately after commit.
 */
export function RecordActions({ objectKind, record, onChanged, emptyText, ariaLabel = "Record actions", prefillExtra, icon = false }: {
  readonly objectKind: string
  readonly record: ObjectRecord
  readonly onChanged?: () => void
  readonly emptyText?: string
  readonly ariaLabel?: string
  readonly prefillExtra?: Readonly<Record<string, string>>
  readonly icon?: boolean
}) {
  const catalog = useActions(objectKind, record.id)
  const client = useQueryClient()
  const [acting, setActing] = useState<OpenAction | null>(null)
  const off = catalog.isError
    ? (catalog.error instanceof Error ? catalog.error.message : ACTIONS_UNAVAILABLE)
    : catalog.data?.kind === "unavailable" ? ACTIONS_UNAVAILABLE
      : catalog.data?.kind === "refused" ? catalog.data.message
        : catalog.data?.kind === "no-cell" ? NO_DATA_CELL : undefined
  const offer = recordCatalog(true, catalog, [])
  const actions = offer.kind === "catalog" ? offeredFor(offer.items, record) : []
  const handleChanged = () => {
    void client.invalidateQueries({ queryKey: ["actions", objectKind, record.id] })
    onChanged?.()
  }
  return (
    <div className="grid gap-2" aria-label={ariaLabel} role="group">
      {off !== undefined
        ? <p className="text-meta text-muted-foreground">{off}</p>
        : offer.kind === "reading"
          ? <Skeleton className="h-8" aria-label="Reading offered actions" />
          : actions.length === 0
            ? <p className="text-meta text-muted-foreground">{emptyText ?? "The cell offers you no actions on this record."}</p>
            : (
              <div className="flex flex-wrap gap-2">
                {actions.map((action) => (
                  <Button key={action.actionId} type="button" variant="outline" size="sm"
                    onClick={() => setActing({ action, prefill: { ...prefillExtra, [action.objectIdField]: record.id } })}>
                    {icon && <Play aria-hidden />}{action.title}
                  </Button>
                ))}
              </div>
            )}
      <ActionSheet open={acting} onClose={() => setActing(null)} onChanged={handleChanged} />
    </div>
  )
}
