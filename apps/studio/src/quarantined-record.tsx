import { useQuery } from "@tanstack/react-query"
import { ShieldAlert } from "lucide-react"
import { CellRefusal, NotServed } from "@/lib/live-transport"
import { RecordActions } from "./actions/record-actions.tsx"
import { Findings as FindingsPanel } from "./evidence/findings.tsx"
import { History } from "./evidence/history.tsx"
import { readFindings } from "./evidence/review.ts"
import { useEvidenceActions } from "./actions/evidence-actions.tsx"

/**
 * A record the cell does not serve. When the cell holds it in quarantine (its findings route says
 * so), the page shows why, with the findings and the actions that can set it right; otherwise the
 * record is simply not readable here.
 */
export function QuarantinedRecord({ objectType, id, onDone }: { readonly objectType: string; readonly id: string; readonly onDone: () => void }) {
  const findings = useQuery({
    queryKey: ["findings", objectType, id],
    queryFn: () => readFindings(objectType, id),
    retry: false,
  })
  const corrections = useEvidenceActions(objectType, id)
  const absent = <p className="text-muted-foreground">This record is not available: it does not exist, or you may not read it.</p>
  if (findings.isPending) return <p className="text-muted-foreground">Reading the record…</p>
  if (findings.isError) return findings.error instanceof NotServed || (findings.error instanceof CellRefusal && findings.error.status === 404) ? absent : <p role="alert" className="text-destructive">{findings.error.message}</p>
  const { quarantined } = findings.data.document
  if (!quarantined) return absent
  return (
    <div className="space-y-4">
      <div role="status" className="flex items-start gap-3 rounded-lg border border-destructive/30 bg-destructive/5 p-4">
        <ShieldAlert aria-hidden className="mt-0.5 size-5 shrink-0 text-destructive" />
        <div className="space-y-1">
          <p className="font-medium">Held in quarantine</p>
          <p className="text-sm text-muted-foreground">The cell loaded this record but does not serve it: it breaks a law. It stays out of every answer until it is corrected.</p>
        </div>
      </div>
      <FindingsPanel objectType={objectType} id={id} actionFor={corrections.actionFor} />
      <History objectType={objectType} id={id} />
      <RecordActions objectType={objectType} id={id} onDone={onDone} />
      {corrections.sheet(onDone)}
    </div>
  )
}
