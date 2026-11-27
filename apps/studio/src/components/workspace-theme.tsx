import { useEffect, useLayoutEffect } from "react"
import { Dot } from "@/components/system/status-dot"
import { clearPublishedServedTheme, useAppearance } from "@/lib/appearance"
import { studioScopeKey } from "@/lib/definitions"
import { useServedTheme } from "@/lib/workspace-theme"

const useIsomorphicLayoutEffect = typeof globalThis.window === "undefined" ? useEffect : useLayoutEffect

/**
 * Follows the theme the server serves for the decided workspace and paints it synchronously before
 * browser paint (themes as data): a theme an agent applies repaints this page with no reload,
 * switching workspace or signing out clears the prior workspace's overrides immediately with zero
 * bleed, and a served theme the shared gate refuses never paints.
 */
export function WorkspaceThemeFeed() {
  const { scope, served } = useServedTheme()
  const { setServed } = useAppearance()
  const scopeKey = scope === null ? null : studioScopeKey(scope)

  useIsomorphicLayoutEffect(() => {
    setServed(served, scopeKey)
  }, [scopeKey, served, setServed])

  // Clear synchronously whenever the decided workspace changes or the signed-in shell unmounts.
  useIsomorphicLayoutEffect(() => () => {
    clearPublishedServedTheme(scopeKey)
  }, [scopeKey])

  if (served.kind !== "invalid") return null
  const first = served.issues[0]
  return (
    <div role="status" data-workspace-theme="invalid" className="flex min-h-[44px] flex-nowrap items-center gap-2 px-4 py-1.5 text-body md:px-8">
      <Dot tone="held">
        <span className="shrink-0 text-foreground">Workspace theme not applied</span>
      </Dot>
      <span className="min-w-0 truncate text-muted-foreground" title={served.issues.map((issue) => issue.message).join("\n")}>
        {first?.message ?? "It did not pass the theme checks."} The default theme is shown.
      </span>
    </div>
  )
}
