import { useQuery } from "@tanstack/react-query"

/** Client bindings for Studio's read-only workspace listing. */

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
}

export class WorkspaceError extends Error {
  constructor(readonly status: number, readonly code: string, message: string) {
    super(message)
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
  return { workspaces: body.workspaces }
}

export function useWorkspaces() {
  return useQuery({ queryKey: WORKSPACES_QUERY_KEY, queryFn: () => readWorkspaces(), retry: false, staleTime: 30_000 })
}
