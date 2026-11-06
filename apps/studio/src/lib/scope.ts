/**
 * The Studio scope a request acts in: which organization and environment the cell reads and writes. The cell
 * fails closed (403) on a definitions call without one. The server resolves it: `/api/me` names the signed-in
 * organization and its data environment (`environmentId`, or null when the workspace has no data cell yet).
 * Studio never maps an organization to an environment itself.
 */
import type { Account } from "./session.ts"

export interface StudioScope { readonly orgId: string; readonly environmentId: string }

/** The account's active scope as the server resolved it; null when it has no organization or no data cell. */
export const scopeOf = (account: Account | null | undefined): StudioScope | null =>
  account?.organization && account.environmentId ? { orgId: account.organization.id, environmentId: account.environmentId } : null

/** The scope as the query every scoped host route takes: `orgId=…&environmentId=…`. */
export const scopeQuery = (scope: StudioScope, extra: Record<string, string> = {}) =>
  new URLSearchParams({ orgId: scope.orgId, environmentId: scope.environmentId, ...extra }).toString()
