import { useQuery } from "@tanstack/react-query"
import { ShieldAlert } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { LoadState } from "@/components/load-state"
import { Panel, Well } from "@/components/system/panel"
import { CellRefusal, NotServed } from "@/lib/live-transport"
import { RecordActions } from "./actions/record-actions.tsx"
import { Findings as FindingsPanel } from "./evidence/findings.tsx"
import { History } from "./evidence/history.tsx"
import type { ScopedReader } from "./lib/workspace.tsx"
import { useEvidenceActions } from "./actions/evidence-actions.tsx"

/**
 * A record the cell does not serve. When the cell holds it in quarantine (its findings route says
 * so), the page shows why, with the findings and the actions that can set it right; otherwise the
 * record is simply not readable here.
 */
export function QuarantinedRecord({ objectType, id, reader, onDone }: {
  readonly objectType: string; readonly id: string
  /** The reader of the workspace the record page's mode decided: the findings are read and cached there (#1745). */
  readonly reader: ScopedReader
  readonly onDone: () => void
}) {
  const findings = useQuery({ ...reader.findings(objectType, id), retry: false })
  const corrections = useEvidenceActions(objectType, id)
  const absent = <p className="text-muted-foreground">This record is not available: it does not exist, or you may not read it.</p>
  if (findings.isPending) return <LoadState status="pending" skeleton="detail" loadingLabel="Reading record" onRetry={() => void findings.refetch()}>{() => null}</LoadState>
  if (findings.isError) return findings.error instanceof NotServed || (findings.error instanceof CellRefusal && findings.error.status === 404)
    ? absent
    : <LoadState status="error" error={findings.error} isFetching={findings.isFetching} onRetry={() => void findings.refetch()}>{() => null}</LoadState>
  const { quarantined } = findings.data.document
  if (!quarantined) return absent
  // The page's title already carries the state ("Quarantined", dot first). This is the one sentence that
  // says what it means, in a neutral well: no red surface, no second badge, no second heading.
  return (
    <div className="space-y-4">
      <Panel>
        <Well className="border-t-0">
          <div role="status" aria-label="Why this record is not served" data-quarantine-statement="" className="flex items-start gap-3 px-4 py-3 text-body">
            <ShieldAlert aria-hidden strokeWidth={1.6} className="mt-[3px] size-4 shrink-0 text-muted-foreground" />
            <p className="min-w-0 flex-1 text-pretty">
              <Badge variant="destructive" className="me-2 h-auto align-baseline text-body text-foreground">Not served</Badge>
              <span className="text-muted-foreground">The cell loaded this record but keeps it out of every answer and total: it breaks a law. It stays out until a correction satisfies the law; its source values stay readable here.</span>
            </p>
          </div>
        </Well>
      </Panel>
      <FindingsPanel objectType={objectType} id={id} reader={reader} actionFor={corrections.actionFor} quarantineStated />
      <History objectType={objectType} id={id} />
      <RecordActions objectType={objectType} id={id} onDone={onDone} />
      {corrections.sheet(onDone)}
    </div>
  )
}
