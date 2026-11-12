import { useQuery } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { ShieldAlert } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { EmptyState, Panel, Well } from "@/components/panel"
import { NotServed, cellRead } from "@/lib/live-transport"

/** `GET /v1/objects/{type}:quarantined`: the rows the cell holds back that this reader may read. */
interface Quarantined {
  readonly objectType: string
  readonly items: readonly { readonly id: string; readonly openFindings: number }[]
  readonly approximate: boolean
  readonly scanLimit: number
}

/**
 * The records of one type the cell holds in quarantine instead of serving: each with its open
 * findings, opening to the record and why it is held. Only rows whose values this reader may
 * read are named; the cell decides which.
 */
export function QuarantinePanel({ objectType }: { readonly objectType: string }) {
  const held = useQuery({
    queryKey: ["quarantined", objectType],
    queryFn: () => cellRead<Quarantined>(`/v1/objects/${encodeURIComponent(objectType)}:quarantined`, "Quarantined records are"),
    retry: false,
  })
  if (held.isPending) return <p className="text-sm text-muted-foreground">Reading what the cell holds back…</p>
  if (held.isError) return held.error instanceof NotServed
    ? <EmptyState icon={ShieldAlert} title="Not served">{held.error.message}</EmptyState>
    : <p role="alert" className="text-sm text-destructive">{held.error.message}</p>
  const { items, approximate, scanLimit } = held.data.document
  return (
    <Panel icon={ShieldAlert} label="In quarantine">
      <Well className="divide-y">
        {items.length === 0 && <EmptyState icon={ShieldAlert} title="Nothing held back">No record of this type is in quarantine.</EmptyState>}
        {items.map(({ id, openFindings }) => (
          <div key={id} className="flex items-center justify-between gap-3 px-4 py-2.5 text-sm">
            <Link to="/sources/$concept/$" params={{ concept: objectType, _splat: id }} className="font-mono underline-offset-4 hover:underline">{id}</Link>
            <Badge variant={openFindings > 0 ? "destructive" : "outline"}>{openFindings} open {openFindings === 1 ? "finding" : "findings"}</Badge>
          </div>
        ))}
        {approximate && <p className="px-4 py-2 text-meta text-muted-foreground">Only the first {scanLimit} records were scanned.</p>}
      </Well>
    </Panel>
  )
}
