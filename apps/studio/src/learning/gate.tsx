import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { EmptyState, PageHeader } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { REQUIRED_RESOURCES, useLearningCapabilities, type LearningSurface } from "../lib/learning/capabilities.ts"

/**
 * Fail-closed route gate for all Learning destinations (#1220, PR #1631 S1).
 *
 * Uses the single scoped capability read (`useLearningCapabilities` → `GET /v1/definitions`).
 * While the read is missing, pending, or failed, or the current scoped cell does not advertise all
 * required resources for `surface`, this gate renders an unavailable state and NEVER mounts
 * `children`, guaranteeing zero page data fetches before capability is confirmed.
 */
export function LearningGate({
  surface,
  icon,
  title,
  children,
}: {
  readonly surface: LearningSurface
  readonly icon: LucideIcon
  readonly title: string
  readonly children: ReactNode
}) {
  const capabilities = useLearningCapabilities()
  if (capabilities[surface]) return <>{children}</>

  const required = REQUIRED_RESOURCES[surface].join(", ")
  const message = capabilities.status === "error"
    ? "Studio couldn't read this workspace's capabilities from the cell, so this surface stays unavailable."
    : `This workspace's cell doesn't advertise ${required} yet.`

  return (
    <div className="grid max-w-5xl gap-5" data-learning-gate="unavailable" data-gate-surface={surface} data-gate-status={capabilities.status}>
      <PageHeader title={title} />
      <EmptyState
        icon={icon}
        title="Unavailable in this workspace"
        next={capabilities.status === "error"
          ? <Button type="button" variant="outline" size="sm" onClick={capabilities.refetch}>Retry</Button>
          : undefined}
      >
        {message}
      </EmptyState>
    </div>
  )
}
