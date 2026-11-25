import { useQueryClient, type QueryKey } from "@tanstack/react-query"
import { useEffect } from "react"
import { LiveClient, type LiveRefusal } from "../../../../packages/norn-sdk/src/index.ts"
import { type StudioScope, studioScopeKey } from "./definitions.ts"
import { useWorkspace } from "./workspace.tsx"

/**
 * Live reads (UI-LIVE-1): Studio holds one socket per scope to the cell through its Worker
 * (`/api/host/v1/subscribe`), which mints the host token for each upgrade. Pages never poll: when
 * the cell's answer to a route changes after a commit, the page reads it again through its usual
 * typed read, once. The socket carries nothing the GET would not.
 */
const clients = new Map<string, LiveClient>()

/**
 * The page's live client for `scope`: one per scope, so a read of one scope never loses its socket
 * to a read of another. Each client opens its socket for its first subscription and closes it when
 * its last one leaves (`LiveClient`); a workspace switch unmounts the old scope's pages, so their
 * socket closes with them. The Worker decides every upgrade: a scope the session no longer has is
 * refused, and that client's readers are told `LIVE_UNAVAILABLE` once it stops reconnecting.
 */
export function live(scope: StudioScope): LiveClient {
  const key = studioScopeKey(scope)
  let client = clients.get(key)
  if (client === undefined) {
    const query = new URLSearchParams({ orgId: scope.orgId, environmentId: scope.environmentId })
    client = new LiveClient({ url: () => `${location.origin.replace(/^http/, "ws")}/api/host/v1/subscribe?${query}` })
    clients.set(key, client)
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
 * where the page already is; `null` subscribes to nothing (a page that has ended, or no cell). It
 * subscribes only at the workspace the page's mode decided (`lib/workspace.tsx`), never at the
 * session's scope while the mode is undecided or failed, and never in the recording.
 */
export function useLiveRefresh(path: string | null, queryKey: QueryKey): void {
  const queries = useQueryClient()
  const workspace = useWorkspace()
  const scope = workspace.kind === "live" ? workspace.scope : null
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

/**
 * Listen to the cell's own answer to `path` (status and body, as the subscriber, now): for views
 * that show exactly what the cell answers, such as a lifecycle's facet counts. Returns the
 * unsubscribe; without a socket it never calls `listener`.
 */
export function liveAnswer(
  path: string,
  scope: StudioScope,
  listener: (answer: { readonly status: number; readonly body: unknown; readonly digest?: string }) => void,
  onRefused?: (refusal: LiveRefusal) => void,
): () => void {
  if (typeof WebSocket === "undefined") return () => {}
  return live(scope).subscribe(
    scopedPath(path, scope),
    (snapshot) => listener({ status: snapshot.status, body: snapshot.body, digest: snapshot.digest }),
    onRefused ? { onRefused } : undefined,
  )
}
