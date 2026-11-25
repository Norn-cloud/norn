import { useQueryClient } from "@tanstack/react-query"
import type { LucideIcon } from "lucide-react"
import { Fragment, createContext, use, useCallback, type ReactNode } from "react"
import { PageHeader } from "@/components/panel"
import { EmptyState } from "@/components/system/panel"
import { Button } from "@/components/ui/button"
import { studioScopeKey, type StudioScope } from "../lib/definitions.ts"
import { REQUIRED_RESOURCES, useLearningCapabilities, type LearningSurface } from "../lib/learning/capabilities.ts"
import { syncStudioScopeChange } from "../lib/live-transport.ts"

const LearningScopeContext = createContext<StudioScope | null>(null)

/**
 * The verified server-reported scope whose `/v1/definitions` catalog opened the enclosing
 * `<LearningGate>`, or null outside a gate.
 */
export function useLearningScope(): StudioScope | null {
  return use(LearningScopeContext)
}

/**
 * Bind downstream Learning reads and actions inside a gated page to the gate's verified scope.
 * If `/api/me` moves to another scope before a read or action runs, `onScopeMismatch` immediately
 * invalidates the old capability state (unmounting the page) without touching the new cell.
 */
export function useBoundLearningRead(): {
  readonly scope: StudioScope | null
  readonly scopeKey: string
  readonly onScopeMismatch: (actual: StudioScope | null | undefined) => void
} {
  const scope = useLearningScope()
  const client = useQueryClient()
  const onScopeMismatch = useCallback((actual: StudioScope | null | undefined) => {
    syncStudioScopeChange(client, actual)
  }, [client])
  return { scope, scopeKey: studioScopeKey(scope), onScopeMismatch }
}

/**
 * Fail-closed route gate for all Learning destinations (#1220, PR #1631 S1).
 *
 * Uses the single scoped capability read (`useLearningCapabilities` → `GET /v1/definitions` keyed
 * by org + environment). While the read is missing, pending, failed, or for another scope, or the
 * current scoped cell does not advertise all required resources for `surface`, this gate renders an
 * unavailable state and NEVER mounts `children`, guaranteeing zero page data fetches before
 * capability is confirmed for that exact scope. Keying `children` by scope guarantees a scope
 * switch unmounts old page state and drops any in-flight reads from the previous cell.
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
  if (capabilities[surface] && capabilities.scope !== null) {
    return (
      <LearningScopeContext.Provider value={capabilities.scope}>
        <Fragment key={studioScopeKey(capabilities.scope)}>{children}</Fragment>
      </LearningScopeContext.Provider>
    )
  }

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
