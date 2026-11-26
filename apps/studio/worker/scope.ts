import type { Auth } from "./auth.ts"

/**
 * The signed-in session's cell scope, resolved on the server: the active organization and the cell
 * environment it lives in. This module is the only place that maps an organization to its
 * environment; `/api/me` and every cell call (definitions, bindings) use it, never a constant.
 *
 * The environment is the `environmentId` in the organization's metadata
 * (`{"environmentId": "analytics"}` for org_demo). An organization that names none has no cell:
 * `environmentId` is null, and callers refuse with a typed error rather than guess, as the cell does.
 */
export interface CellScope {
  readonly orgId: string
  readonly environmentId: string | null
}

const ENVIRONMENT = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/

/** Extract and validate `environmentId` from organization metadata (object or JSON string). */
export function environmentOf(metadata: unknown): string | null {
  let value = metadata
  if (typeof value === "string") {
    try {
      value = JSON.parse(value)
    } catch {
      return null
    }
  }
  const environmentId = (value as { environmentId?: unknown } | null)?.environmentId
  return typeof environmentId === "string" && ENVIRONMENT.test(environmentId) ? environmentId : null
}

export async function cellScope(auth: Auth, orgId: string): Promise<CellScope> {
  const { adapter } = await auth.$context
  const org = await adapter.findOne<{ metadata?: unknown }>({ model: "organization", where: [{ field: "id", value: orgId }] })
  return { orgId, environmentId: environmentOf(org?.metadata) }
}

/** `orgId=…&environmentId=…` for a cell route; only for a scope that has an environment. */
export const scopeQuery = (scope: CellScope & { readonly environmentId: string }) =>
  new URLSearchParams({ orgId: scope.orgId, environmentId: scope.environmentId }).toString()
