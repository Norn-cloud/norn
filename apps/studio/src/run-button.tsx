import { useMutation } from "@tanstack/react-query"
import { useNavigate } from "@tanstack/react-router"
import { Loader2, Play } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { flowStore, newRunId } from "./lib/canvas/flows.ts"
import { scopeOf } from "./lib/scope.ts"
import { useAccount } from "./lib/session.ts"

/**
 * Start a run of an installed flow and open it at /runs/{id}. A thin stand-in with the props of lane GRID's
 * `RunButton` (studio/decisions, src/runs/run-page.tsx): when the stacks meet, callers switch to GRID's and this
 * file goes. The cell decides; a refusal is shown as it says it.
 */
export function RunButton({ flow, label = "Run", variant = "default" }: { readonly flow: string; readonly label?: string; readonly variant?: "default" | "outline" }) {
  const navigate = useNavigate()
  const account = useAccount()
  const scope = scopeOf(account.data)
  const start = useMutation({
    mutationFn: async () => {
      if (!scope) throw new Error("This workspace has no data cell yet")
      return flowStore(scope).start(flow, newRunId())
    },
    onSuccess: (run) => void navigate({ to: "/runs/$id", params: { id: run.id } }),
  })
  return (
    <span className="inline-flex flex-wrap items-center gap-2">
      <Button size="sm" variant={variant} className={cn(variant === "outline" && "border-node-border")} disabled={start.isPending} onClick={() => start.mutate()}>
        {start.isPending ? <Loader2 className="animate-spin" /> : <Play />} {label}
      </Button>
      {start.isError && <span role="alert" className="text-meta text-destructive">{start.error instanceof Error ? start.error.message : "The cell did not answer."}</span>}
    </span>
  )
}
