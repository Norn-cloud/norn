import { useEffect } from "react"
import { Dot } from "@/components/system/status-dot"
import { useAppearance } from "@/lib/appearance"
import { useServedTheme } from "@/lib/workspace-theme"

/**
 * Follows the theme the server serves for the shown workspace and paints it (themes as data): a
 * theme an agent applies repaints this page with no reload, and switching workspace switches the
 * theme. A served theme the shared gate refuses never paints; this line says why, in the gate's words.
 */
export function WorkspaceThemeFeed() {
  const { served } = useServedTheme()
  const { setServed } = useAppearance()
  useEffect(() => { setServed(served) }, [served, setServed])
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
