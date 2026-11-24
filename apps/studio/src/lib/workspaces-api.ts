import { type QueryClient, useQuery } from "@tanstack/react-query"
import { forgetParkedCalls } from "../actions/parked-calls.ts"
import { refreshMode } from "./live-transport.ts"
import { forgetReads } from "./session.ts"

/**
 * Client bindings for Studio's workspace listing and switcher activation (`/api/workspaces`).
 * Display and invocation only: every permission decision lives on the server.
 */

export interface WorkspaceItem {
  readonly id: string
  readonly name: string
  readonly slug: string
  readonly role: string
  readonly active: boolean
  readonly hasCell: boolean
}

export interface WorkspacesDocument {
  readonly workspaces: readonly WorkspaceItem[]
  readonly planLabel: string | null
}

export class WorkspaceError extends Error {
  constructor(readonly status: number, readonly code: string, message: string) {
    super(message)
  }
}

export class SessionRefreshError extends WorkspaceError {
  constructor(message = "The workspace switch succeeded, but refreshing your session failed. Retry refreshing below.") {
    super(503, "SESSION_REFRESH_FAILED", message)
  }
}

export const WORKSPACES_QUERY_KEY = ["workspaces"] as const

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/** Read the signed-in user's workspace document; returns null only for supported absence (401/404). */
export async function readWorkspaces(fetcher: Fetch = (input, init) => fetch(input, init)): Promise<WorkspacesDocument | null> {
  let response: Response
  try {
    response = await fetcher("/api/workspaces", { headers: { accept: "application/json" } })
  } catch {
    throw new WorkspaceError(0, "WORKSPACES_UNAVAILABLE", "Could not load workspaces. Check your connection and retry.")
  }
  if (response.status === 401 || response.status === 404) return null
  if (!response.ok) {
    throw new WorkspaceError(response.status, "WORKSPACES_UNAVAILABLE", `Could not load workspaces (${response.status}).`)
  }
  const body = (await response.json().catch(() => null)) as Partial<WorkspacesDocument> | null
  if (body === null || !Array.isArray(body.workspaces)) {
    throw new WorkspaceError(response.status, "WORKSPACES_INVALID", "Received an invalid workspace list from the server.")
  }
  return {
    workspaces: body.workspaces,
    planLabel: typeof body.planLabel === "string" ? body.planLabel : null,
  }
}

export function useWorkspaces() {
  return useQuery({ queryKey: WORKSPACES_QUERY_KEY, queryFn: () => readWorkspaces(), retry: false, staleTime: 30_000 })
}

async function mutate<T>(path: string, method: "POST", payload: unknown, fetcher: Fetch = (input, init) => fetch(input, init)): Promise<T> {
  const response = await fetcher(path, {
    method,
    headers: { accept: "application/json", "content-type": "application/json" },
    body: JSON.stringify(payload),
  })
  const parsed = (await response.json().catch(() => null)) as { code?: unknown; message?: unknown; error?: unknown } | null
  if (!response.ok) {
    const code = typeof parsed?.code === "string" ? parsed.code : `HTTP_${response.status}`
    const message = typeof parsed?.message === "string" ? parsed.message : typeof parsed?.error === "string" ? parsed.error : `Workspace request failed (${response.status}).`
    throw new WorkspaceError(response.status, code, message)
  }
  return parsed as T
}

export const workspaceApi = {
  activate: (id: string, fetcher?: Fetch) => mutate<{ readonly activeOrganizationId: string }>(`/api/workspaces/${encodeURIComponent(id)}/activate`, "POST", {}, fetcher),
}

/**
 * Forget previous workspace reads and cached cell mode, confirm the updated session from `/api/me`,
 * then re-read workspaces, scope, and mode. Throws SessionRefreshError if the session refresh fails
 * so callers never report completion or replay a committed mutation.
 */
export async function switchWorkspaceCaches(client: QueryClient): Promise<void> {
  await client.cancelQueries({ predicate: (query) => query.queryKey[0] !== "session" && query.queryKey[0] !== "auth-config" })
  forgetParkedCalls()
  refreshMode()
  try {
    await client.refetchQueries({ queryKey: ["session"] }, { throwOnError: true })
  } catch {
    throw new SessionRefreshError()
  }
  if (client.getQueryState(["session"])?.status === "error") throw new SessionRefreshError()
  refreshMode()
  forgetReads(client, { refetchActive: true })
  await Promise.all([
    client.refetchQueries({ queryKey: WORKSPACES_QUERY_KEY }),
    client.refetchQueries({ queryKey: ["studio-scope"] }),
    client.refetchQueries({ queryKey: ["studio-mode"] }),
  ])
}
