import { useQueryClient, type QueryKey } from "@tanstack/react-query"
import { useEffect } from "react"
import { LiveClient } from "../../../../packages/norn-demo-ui/src/live/client.ts"
import { type StudioScope, useStudioScope } from "./definitions.ts"

/**
 * Live reads (UI-LIVE-1): Studio holds one socket to the cell through its Worker
 * (`/api/host/v1/subscribe`), which mints the host token for each upgrade. Pages never poll: when
 * the cell's answer to a route changes after a commit, the page reads it again through its usual
 * typed read, once. The socket carries nothing the GET would not.
 */
let client: LiveClient | null = null
let scopeOf: StudioScope | null = null

function live(scope: StudioScope): LiveClient {
  if (client === null || scopeOf?.orgId !== scope.orgId || scopeOf.environmentId !== scope.environmentId) {
    client?.close()
    scopeOf = scope
    const query = new URLSearchParams({ orgId: scope.orgId, environmentId: scope.environmentId })
    client = new LiveClient({ url: () => `${location.origin.replace(/^http/, "ws")}/api/host/v1/subscribe?${query}` })
  }
  return client
}

/** The route with the active scope, as the cell's GET would be asked. */
export function scopedPath(path: string, scope: StudioScope): string {
  const [pathname = "", search = ""] = path.split("?", 2)
  const query = new URLSearchParams(search)
  query.set("orgId", scope.orgId)
  query.set("environmentId", scope.environmentId)
  return `${pathname}?${query}`
}

/**
 * Read `queryKey` again whenever the cell's answer to `path` changes. The first answer only marks
 * where the page already is; `null` subscribes to nothing (a page that has ended, or no cell).
 */
export function useLiveRefresh(path: string | null, queryKey: QueryKey): void {
  const queries = useQueryClient()
  const scope = useStudioScope().data ?? null
  const key = JSON.stringify(queryKey)
  useEffect(() => {
    if (path === null || scope === null || typeof WebSocket === "undefined") return
    let seen: string | undefined
    return live(scope).subscribe(scopedPath(path, scope), (snapshot) => {
      if (seen !== undefined && snapshot.digest !== seen) void queries.invalidateQueries({ queryKey })
      seen = snapshot.digest
    })
    // The key's value, not its identity, decides the subscription.
  }, [path, scope?.orgId, scope?.environmentId, key])
}
