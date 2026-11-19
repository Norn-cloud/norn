import { hostToken, type NornActor } from "./actor.ts"
import type { Auth } from "./auth.ts"
import { cellScope, scopeQuery } from "./scope.ts"

/**
 * The Worker's own calls to the cell, made as the signed-in actor in that
 * actor's workspace scope: the same host token and route the browser's
 * `/api/host` forwarding uses, so the cell's authority decides each one and
 * the Worker gains no right of its own. Used by server-side routes (the
 * question catalog and check, the dashboard agent).
 */
export type CellAnswer =
  | { readonly kind: "answer"; readonly status: number; readonly body: unknown }
  /** The workspace has no data environment: nothing was asked. */
  | { readonly kind: "no-cell" }

export interface Cell {
  get(path: string): Promise<CellAnswer>
  post(path: string, body: unknown): Promise<CellAnswer>
}

export function cellFor(host: Fetcher, auth: Auth, actor: NornActor): Cell {
  const call = async (method: "GET" | "POST", path: string, body?: unknown): Promise<CellAnswer> => {
    const scope = await cellScope(auth, actor.orgId)
    if (scope.environmentId === null) return { kind: "no-cell" }
    const headers = new Headers({ authorization: `Bearer ${await hostToken(auth, actor)}`, accept: "application/json" })
    if (body !== undefined) headers.set("content-type", "application/json")
    const response = await host.fetch(new Request(`https://norn-host${path}?${scopeQuery({ ...scope, environmentId: scope.environmentId })}`, { method, headers, body: body === undefined ? null : JSON.stringify(body) }))
    const text = await response.text()
    let parsed: unknown = null
    try { parsed = JSON.parse(text) } catch { parsed = null }
    return { kind: "answer", status: response.status, body: parsed }
  }
  return { get: (path) => call("GET", path), post: (path, body) => call("POST", path, body) }
}
