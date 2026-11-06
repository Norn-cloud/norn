import { useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { Activity } from "lucide-react"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { flowStore } from "./lib/canvas/flows.ts"
import { scopeOf } from "./lib/scope.ts"
import { useAccount } from "./lib/session.ts"

/**
 * A run, minimally: what the cell says about it (its id, flow and state), read as the signed-in account. The full
 * run view (steps, receipts, decisions) is lane GRID's and replaces this page at the same address.
 */
export function RunPage() {
  const { id } = useParams({ strict: false }) as { id: string }
  const account = useAccount()
  const scope = scopeOf(account.data)
  const run = useQuery({ queryKey: ["run", id, scope?.orgId, scope?.environmentId], queryFn: async () => (await flowStore(scope!).read(id)) ?? null, enabled: scope !== null, retry: false })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Run {id}</h1>
        <p className="max-w-2xl text-muted-foreground">A run of an automation in the cell. The full run view, with its steps, receipts and decisions, comes here next.</p>
      </div>
      {account.isSuccess && scope === null && <p role="status" className="text-muted-foreground">This workspace has no data cell yet</p>}
      {run.isLoading && <p role="status" className="text-muted-foreground">Reading the run…</p>}
      {run.isError && <p role="alert" className="text-destructive">{run.error.message}</p>}
      {run.isSuccess && run.data === null && <EmptyState icon={Activity}>The cell has no run {id} you can read.</EmptyState>}
      {run.data && (
        <Panel icon={Activity} label="In the cell" action={<Badge variant="outline" data-testid="run-state">{run.data.state ?? "started"}</Badge>}>
          <Well className="space-y-1 p-3 text-sm">
            <p>Run <span className="font-mono">{run.data.id}</span></p>
            <p className="text-muted-foreground">Flow <span className="font-mono">{typeof run.data.flow === "string" ? run.data.flow : JSON.stringify(run.data.flow)}</span></p>
          </Well>
        </Panel>
      )}
      <Link to="/canvas" className="text-sm underline-offset-4 hover:underline">Back to the canvas</Link>
    </section>
  )
}
