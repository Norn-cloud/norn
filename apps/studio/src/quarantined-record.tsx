import { useQuery } from "@tanstack/react-query"
import { ShieldAlert } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Panel, Well } from "@/components/panel"
import { CellRefusal, NotServed, cellRead } from "@/lib/live-transport"
import { RecordActions } from "./actions/record-actions.tsx"

/** One data-quality finding as `GET /v1/objects/{t}/{id}/findings` sends it. */
interface Finding {
  readonly id: string
  readonly ruleRef: { readonly kind: string; readonly artifactId?: string; readonly lawId?: string; readonly version?: string }
  readonly severity: string
  readonly message: string
  readonly status: string
}

interface Findings { readonly quarantined: boolean; readonly findings: readonly Finding[] }

/**
 * A record the cell does not serve. When the cell holds it in quarantine (its findings route says
 * so), the page shows why, with the findings and the actions that can set it right; otherwise the
 * record is simply not readable here.
 */
export function QuarantinedRecord({ objectType, id, onDone }: { readonly objectType: string; readonly id: string; readonly onDone: () => void }) {
  const findings = useQuery({
    queryKey: ["findings", objectType, id],
    queryFn: () => cellRead<Findings>(`/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}/findings`, "Findings are"),
    retry: false,
  })
  const absent = <p className="text-muted-foreground">This record is not available: it does not exist, or you may not read it.</p>
  if (findings.isPending) return <p className="text-muted-foreground">Reading the record…</p>
  if (findings.isError) return findings.error instanceof NotServed || (findings.error instanceof CellRefusal && findings.error.status === 404) ? absent : <p role="alert" className="text-destructive">{findings.error.message}</p>
  const { quarantined, findings: list } = findings.data.document
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
      <Panel icon={ShieldAlert} label="Why it is held">
        <Well className="divide-y">
          {list.map((finding) => (
            <div key={finding.id} className="space-y-1 px-4 py-3 text-sm">
              <div className="flex flex-wrap items-center gap-2">
                <Badge variant={finding.status === "open" ? "destructive" : "outline"}>{finding.status}</Badge>
                {finding.ruleRef.lawId && <code className="font-mono text-xs">{finding.ruleRef.lawId}</code>}
              </div>
              <p>{finding.message}</p>
            </div>
          ))}
        </Well>
      </Panel>
      <RecordActions objectType={objectType} id={id} onDone={onDone} />
    </div>
  )
}
