import { useQueryClient } from "@tanstack/react-query"
import { ChevronDown, Play, Plus } from "lucide-react"
import { useState } from "react"
import { ActionSheet, useActions, type OpenAction } from "../actions/action-sheet.tsx"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Skeleton } from "@/components/ui/skeleton"
import { ACTIONS_UNAVAILABLE, writesWhole } from "../lib/actions.ts"
import { type ActionSummary } from "../../../../packages/norn-sdk/src/index.ts"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import { offeredFor, recordCatalog } from "../lib/screen/call.ts"
import type { ObjectRecord } from "../lib/transport.ts"
import { useBoundLearningRead } from "./gate.tsx"

/**
 * Type-level creation actions the cell offers for `objectKind` (`writesWhole`). Studio never
 * fabricates an action or hides one on its own: it renders only what `GET /v1/actions` serves,
 * and opens each in the server-built `ActionSheet`.
 */
export function TypeActions({ objectKind, prefill, onChanged, onOpen, label, primary = false, showUnavailable = false }: {
  readonly objectKind: string
  readonly prefill?: Readonly<Record<string, string>>
  readonly onChanged?: () => void
  /**
   * Optional parent-owned action sheet opener. When supplied, opening an action delegates to the
   * parent so the sheet's receipt and Undo state stay mounted independently of list/detail state.
   */
  readonly onOpen?: (open: OpenAction) => void
  /** Override label when several create actions group behind one menu (e.g. "New eval set"). */
  readonly label?: string
  readonly primary?: boolean
  readonly showUnavailable?: boolean
}) {
  const bound = useBoundLearningRead()
  const catalog = useActions(objectKind, undefined, true, bound)
  const [acting, setActing] = useState<OpenAction | null>(null)
  if (catalog.data?.kind === "scope-changed") return null
  const off = catalog.isError
    ? (catalog.error instanceof Error ? catalog.error.message : ACTIONS_UNAVAILABLE)
    : catalog.data?.kind === "unavailable" ? ACTIONS_UNAVAILABLE
      : catalog.data?.kind === "refused" ? catalog.data.message
        : catalog.data?.kind === "no-cell" ? NO_DATA_CELL : undefined
  const creates = catalog.data?.kind === "catalog" ? catalog.data.items.filter(writesWhole) : []
  const open = (action: ActionSummary) => {
    const next: OpenAction = { action, ...(prefill === undefined ? {} : { prefill }) }
    if (onOpen !== undefined) onOpen(next)
    else setActing(next)
  }
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
      {onOpen === undefined && <ActionSheet open={acting} onClose={() => setActing(null)} onChanged={onChanged} scopeBinding={bound} />}
    </>
  )
}

/**
 * Per-record governed actions offered by the cell for one record in its current state:
 * `GET /v1/actions?objectKind={objectKind}&objectId={record.id}` (ACTION-CATALOG-STATE-1),
 * further narrowed by `record.offers` when the record carries it. Studio holds no permission or
 * lifecycle logic; the cell decides which buttons appear and enforces every call at commit time.
 *
 * When `onOpen` is supplied, the parent owns the `ActionSheet` so receipts and server-offered Undo
 * stay independent of whether the record remains visible after a refetch or deletion.
 */
export function RecordActions({ objectKind, record, onChanged, onOpen, emptyText, ariaLabel = "Record actions", prefillExtra, icon = false }: {
  readonly objectKind: string
  readonly record: ObjectRecord
  readonly onChanged?: () => void
  readonly onOpen?: (open: OpenAction) => void
  readonly emptyText?: string
  readonly ariaLabel?: string
  readonly prefillExtra?: Readonly<Record<string, string>>
  readonly icon?: boolean
}) {
  const bound = useBoundLearningRead()
  const catalog = useActions(objectKind, record.id, true, bound)
  const client = useQueryClient()
  const [acting, setActing] = useState<OpenAction | null>(null)
  if (catalog.data?.kind === "scope-changed") return null
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
  const open = (action: ActionSummary) => {
    const next: OpenAction = { action, prefill: { ...prefillExtra, [action.objectIdField]: record.id } }
    if (onOpen !== undefined) onOpen(next)
    else setActing(next)
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
                    onClick={() => open(action)}>
                    {icon && <Play aria-hidden />}{action.title}
                  </Button>
                ))}
              </div>
            )}
      {onOpen === undefined && <ActionSheet open={acting} onClose={() => setActing(null)} onChanged={handleChanged} scopeBinding={bound} />}
    </div>
  )
}
