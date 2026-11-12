import { useQuery } from "@tanstack/react-query"
import { Workflow } from "lucide-react"
import { EmptyState, Panel, Well } from "@/components/panel"
import { CellRefusal, NotServed, cellRead, useMode } from "@/lib/live-transport"
import { RunButton } from "./runs/run-page.tsx"

/** One installed flow as `GET /v1/definitions:list?kind=flow` lists it. */
interface FlowItem {
  readonly id: string
  readonly version?: number
  readonly title?: string | null
  readonly flow?: { readonly flowId?: string; readonly version?: string; readonly title?: string | null }
}

/**
 * The automations installed in the connected cell, each startable here: Run asks the cell to start
 * it and opens the run. Whether this reader may start one is the cell's decision, read in place.
 */
export function Automations() {
  const live = useMode().data?.kind === "live"
  const flows = useQuery({
    queryKey: ["flows"],
    queryFn: () => cellRead<{ readonly items: readonly FlowItem[] }>("/v1/definitions:list?kind=flow&limit=50", "Automations are"),
    enabled: live,
    retry: false,
  })
  if (!live) return null
  return (
    <Panel icon={Workflow} label="Automations">
      <Well className="divide-y">
        {flows.isPending && <p className="px-4 py-3 text-sm text-muted-foreground">Reading the installed automations…</p>}
        {flows.isError && (flows.error instanceof NotServed || (flows.error instanceof CellRefusal && flows.error.status === 404)
          ? <EmptyState icon={Workflow} title="Not served">Automations are not served by this cell yet.</EmptyState>
          : <p role="alert" className="px-4 py-3 text-sm text-destructive">{flows.error.message}</p>)}
        {flows.data && flows.data.document.items.length === 0 && <EmptyState icon={Workflow} title="None installed">No automation is installed in this cell yet.</EmptyState>}
        {flows.data?.document.items.map((item) => (
          <div key={item.id} className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
            <div className="min-w-0">
              <div className="font-medium">{item.flow?.title ?? item.title ?? item.id}</div>
              <div className="font-mono text-xs text-muted-foreground">{item.id}{item.flow?.version ? ` · ${item.flow.version}` : ""}</div>
            </div>
            <RunButton flow={item.id} />
          </div>
        ))}
      </Well>
    </Panel>
  )
}
