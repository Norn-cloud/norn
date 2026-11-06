/**
 * The Studio scope a request acts in: which organization and environment the cell reads and writes. The cell
 * fails closed (403) on a definitions call without one. Until Studio lets a person choose (STUDIO-3's shared
 * helper replaces this seam), the active scope is the demo's: `org_demo` / `analytics`, where agents live.
 */
export interface StudioScope { readonly orgId: string; readonly environmentId: string }

export const DEMO_SCOPE: StudioScope = { orgId: "org_demo", environmentId: "analytics" }

/** The signed-in person's active Studio scope. */
export const activeScope = (): StudioScope => DEMO_SCOPE

/** The scope as the query every scoped host route takes: `orgId=…&environmentId=…`. */
export const scopeQuery = (scope: StudioScope, extra: Record<string, string> = {}) =>
  new URLSearchParams({ orgId: scope.orgId, environmentId: scope.environmentId, ...extra }).toString()
