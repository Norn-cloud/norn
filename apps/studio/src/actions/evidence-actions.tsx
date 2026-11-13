import { useQuery } from "@tanstack/react-query"
import { useState, type ReactNode } from "react"
import { Pencil } from "lucide-react"
import { Button } from "@/components/ui/button"
import { transport, type Artifact } from "@/lib/transport"
import { listActions } from "@/lib/actions"
import { ActionSheet, type OpenAction } from "./action-sheet.tsx"

/** An input field an action keys a slice by: the id of a record of `objectType` it reads in the same commit. */
interface Reference { readonly actionId: string; readonly field: string; readonly objectType: string }

/** Every input-keyed slice the admitted actions declare, read from the cell's catalog. */
function referencesOf(artifacts: readonly Artifact[]): readonly Reference[] {
  return artifacts.flatMap((artifact) => {
    if (artifact.kind !== "action") return []
    const payload = artifact.payload as { actionId?: string; slice?: readonly { objectType?: string; key?: { property?: { from?: string; property?: string } } }[] }
    return (payload.slice ?? []).flatMap((slice) => slice.key?.property?.from === "input" && slice.key.property.property && slice.objectType && payload.actionId
      ? [{ actionId: payload.actionId, field: slice.key.property.property, objectType: slice.objectType }]
      : [])
  })
}

/**
 * For a record's evidence, the actions on the evidence's subject (the record, or the row it names,
 * such as a printed line) that read an evidence object by an input field and take the value's
 * property: each opens with the subject and that evidence filled in and locked. Nothing names a
 * pack: the link is the slice the action declares. The cell decides the call.
 */
export function useEvidenceActions(objectType: string, id: string) {
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const actions = useQuery({ queryKey: ["actions", "*"], queryFn: () => listActions(undefined), retry: false, staleTime: 60_000 })
  const [acting, setActing] = useState<OpenAction | null>(null)
  const references = catalog.data ? referencesOf(catalog.data.document) : []
  const offered = actions.data?.kind === "catalog" ? actions.data.items : []
  const actionFor = (evidence: { readonly ref: { readonly objectType: string; readonly id: string }; readonly subjectProperty: string; readonly subject: { readonly objectType: string; readonly id: string } | null }): ReactNode => {
    const subject = evidence.subject ?? { objectType, id }
    // Offered only where the action is on the value's subject and takes the very value the evidence is for.
    const takes = (action: (typeof offered)[number]) => {
      const properties = (action.inputSchema as { properties?: Record<string, unknown> } | null)?.properties ?? {}
      return action.objectKind === subject.objectType && Object.hasOwn(properties, evidence.subjectProperty)
    }
    const fits = offered.filter(takes).flatMap((action) => references
      .filter((reference) => reference.actionId === action.actionId && reference.objectType === evidence.ref.objectType)
      .map((reference) => ({ action, reference })))
    return fits.length === 0 ? null : (
      <span className="inline-flex gap-1">
        {fits.map(({ action, reference }) => {
          const name = fits.length === 1 ? "Correct this value" : action.title
          return (
            <Button key={`${action.actionId}/${reference.field}`} type="button" size="icon-sm" variant="ghost" aria-label={name} title={name} className="text-muted-foreground hover:text-foreground"
              onClick={() => setActing({ action, prefill: { [action.objectIdField]: subject.id, [reference.field]: evidence.ref.id } })}>
              <Pencil />
            </Button>
          )
        })}
      </span>
    )
  }
  return { actionFor, sheet: (onDone: () => void) => <ActionSheet open={acting} onClose={() => { setActing(null); onDone() }} /> }
}
