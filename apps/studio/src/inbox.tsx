import { useQuery } from "@tanstack/react-query"
import { Inbox as InboxIcon } from "lucide-react"
import { EmptyState } from "@/components/panel"
import { Skeleton } from "@/components/ui/skeleton"
import { cellRead, NotServed, useMode } from "./lib/live-transport.ts"

/** The cell's work queue for this caller (`GET /v1/inbox`): what needs them, each item one click from where to act. */
export function Inbox() {
  const live = useMode().data?.kind === "live"
  const inbox = useQuery({ queryKey: ["inbox"], queryFn: () => cellRead<unknown>("/v1/inbox", "The inbox is"), retry: false, enabled: live })
  if (!live) return null
  return (
    <section aria-labelledby="needs-attention" className="space-y-3">
      <h2 id="needs-attention" className="text-section font-medium">Needs attention</h2>
      {inbox.isPending && <Skeleton className="h-24 rounded-xl" />}
      {inbox.isError && (inbox.error instanceof NotServed
        ? <EmptyState icon={InboxIcon} title="Not served yet">This cell does not serve a work queue yet.</EmptyState>
        : <p role="alert" className="text-sm text-destructive">{inbox.error.message}</p>)}
    </section>
  )
}
