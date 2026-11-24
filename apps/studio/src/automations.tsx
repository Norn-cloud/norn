import { useQuery } from "@tanstack/react-query"
import { Workflow } from "lucide-react"
import { LoadState } from "@/components/load-state"
import { Panel, Well } from "@/components/panel"
import { CellRefusal, NotServed, useMode } from "@/lib/live-transport"
import { readFlows } from "@/lib/flows"
import { RunButton } from "./runs/run-page.tsx"

/**
 * The automations installed in the connected cell. Run asks the cell to start one and opens the run;
 * it shows unless the cell says this reader may not start that flow (`canStart: false`), and the
 * cell decides again at the call.
 */
export function Automations() {
  const live = useMode().data?.kind === "live"
  const flows = useQuery({
    queryKey: ["flows"],
    queryFn: () => readFlows(),
    enabled: live,
    retry: false,
  })
  if (!live) return null
  const error = flows.error instanceof CellRefusal && flows.error.status === 404
    ? new NotServed("Automations are not served by this cell yet.")
    : flows.error
  return (
    <Panel icon={Workflow} label="Automations">
      <LoadState
        status={flows.status}
        data={flows.data}
        error={error}
        isFetching={flows.isFetching}
        onRetry={() => void flows.refetch()}
        skeleton="list"
        loadingLabel="Reading automations"
        notServed={{ icon: Workflow, title: "Not served", description: "Automations are not served by this cell yet." }}
        isEmpty={(items) => items.length === 0}
        empty={{ icon: Workflow, title: "None installed", description: "No automation is installed in this cell yet." }}
      >
        {(items) => (
          <Well className="divide-y">
            {items.map((item) => (
              <div key={item.id} className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
                <div className="min-w-0">
                  <div className="font-medium">{item.flow?.title ?? item.title ?? item.id}</div>
                  <div className="font-mono text-xs text-muted-foreground">{item.id}{item.flow?.version ? ` · ${item.flow.version}` : ""}</div>
                </div>
                {item.canStart === false
                  ? <span className="text-meta text-muted-foreground">Your role cannot start this.</span>
                  : <RunButton flow={item.id} />}
              </div>
            ))}
          </Well>
        )}
      </LoadState>
    </Panel>
  )
}
