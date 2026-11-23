import { type QueryClient, useQuery } from "@tanstack/react-query"
import { forgetParkedCalls } from "../actions/parked-calls.ts"
import { refreshMode } from "./live-transport.ts"
import { forgetReads } from "./session.ts"

/**
 * Client bindings for Studio's governed workspace routes (`/api/workspaces`).
 * Display and invocation only: every permission and quota decision lives on the server.
 */

export interface WorkspaceItem {
  readonly id: string
  readonly name: string
  readonly slug: string
  readonly role: string
  readonly active: boolean
  readonly hasCell: boolean
  readonly archived: boolean
  readonly archivedAt: string | null
  readonly canRename: boolean
  readonly canArchive: boolean
  readonly canDelete: boolean
}

export interface WorkspaceRequestItem {
  readonly id: string
  readonly name: string
  readonly note: string
  readonly status: "pending" | "approved" | "dismissed"
  readonly requesterName: string
  readonly requesterEmail: string
  readonly createdAt: string
  readonly canManage: boolean
  readonly canWithdraw: boolean
}

export type WorkspaceCreation =
  | { readonly mode: "self-serve"; readonly allowed: true }
  | { readonly mode: "self-serve"; readonly allowed: false; readonly reason: string; readonly upgradeUrl: string | null }
  | { readonly mode: "request" }

export interface WorkspacesDocument {
  readonly workspaces: readonly WorkspaceItem[]
  readonly archived: readonly WorkspaceItem[]
  readonly creation: WorkspaceCreation
  readonly requests: readonly WorkspaceRequestItem[]
  readonly planLabel: string | null
}

export class WorkspaceError extends Error {
  constructor(readonly status: number, readonly code: string, message: string) {
    super(message)
  }
}

export const WORKSPACES_QUERY_KEY = ["workspaces"] as const

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/** Read the signed-in user's workspace document; returns null when no session server answers. */
export async function readWorkspaces(fetcher: Fetch = (input, init) => fetch(input, init)): Promise<WorkspacesDocument | null> {
  const response = await fetcher("/api/workspaces", { headers: { accept: "application/json" } }).catch(() => null)
  if (response === null || response.status === 401 || response.status === 404) return null
  if (!response.ok) return null
  const body = (await response.json().catch(() => null)) as Partial<WorkspacesDocument> | null
  if (body === null || !Array.isArray(body.workspaces) || !Array.isArray(body.archived) || body.creation === undefined) return null
  return {
    workspaces: body.workspaces,
    archived: body.archived,
    creation: body.creation,
    requests: Array.isArray(body.requests) ? body.requests : [],
    planLabel: typeof body.planLabel === "string" ? body.planLabel : null,
  }
}

export function useWorkspaces() {
  return useQuery({ queryKey: WORKSPACES_QUERY_KEY, queryFn: () => readWorkspaces(), retry: false, staleTime: 30_000 })
}

async function mutate<T>(path: string, method: "POST" | "PATCH" | "DELETE", payload?: unknown, fetcher: Fetch = (input, init) => fetch(input, init)): Promise<T> {
  const response = await fetcher(path, {
    method,
    headers: { accept: "application/json", ...payload === undefined ? {} : { "content-type": "application/json" } },
    body: payload === undefined ? undefined : JSON.stringify(payload),
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
  create: (name: string, fetcher?: Fetch) => mutate<{ readonly id: string; readonly name: string }>("/api/workspaces", "POST", { name }, fetcher),
  activate: (id: string, fetcher?: Fetch) => mutate<{ readonly activeOrganizationId: string }>(`/api/workspaces/${encodeURIComponent(id)}/activate`, "POST", {}, fetcher),
  rename: (id: string, name: string, fetcher?: Fetch) => mutate<{ readonly id: string; readonly name: string }>(`/api/workspaces/${encodeURIComponent(id)}`, "PATCH", { name }, fetcher),
  archive: (id: string, fetcher?: Fetch) => mutate<{ readonly id: string; readonly archived: true; readonly nextActiveOrganizationId: string | null }>(`/api/workspaces/${encodeURIComponent(id)}/archive`, "POST", {}, fetcher),
  restore: (id: string, activate = true, fetcher?: Fetch) => mutate<{ readonly id: string; readonly archived: false }>(`/api/workspaces/${encodeURIComponent(id)}/restore`, "POST", { activate }, fetcher),
  remove: (id: string, fetcher?: Fetch) => mutate<{ readonly id: string; readonly deleted: true; readonly nextActiveOrganizationId: string | null }>(`/api/workspaces/${encodeURIComponent(id)}`, "DELETE", undefined, fetcher),
  requestWorkspace: (name: string, note: string, fetcher?: Fetch) => mutate<{ readonly id: string; readonly name: string; readonly status: "pending" }>("/api/workspaces/requests", "POST", { name, note }, fetcher),
  decideRequest: (id: string, decision: "approved" | "dismissed", fetcher?: Fetch) => mutate<{ readonly id: string; readonly status: string }>(`/api/workspaces/requests/${encodeURIComponent(id)}/decide`, "POST", { decision }, fetcher),
}

/** Forget previous workspace reads and cached cell mode, then re-read session, scope, mode and workspaces. */
export async function switchWorkspaceCaches(client: QueryClient): Promise<void> {
  await client.cancelQueries({ predicate: (query) => query.queryKey[0] !== "session" && query.queryKey[0] !== "auth-config" })
  forgetParkedCalls()
  refreshMode()
  await client.refetchQueries({ queryKey: ["session"] })
  refreshMode()
  forgetReads(client, { refetchActive: true })
  await Promise.all([
    client.refetchQueries({ queryKey: WORKSPACES_QUERY_KEY }),
    client.refetchQueries({ queryKey: ["studio-scope"] }),
    client.refetchQueries({ queryKey: ["studio-mode"] }),
  ])
}
