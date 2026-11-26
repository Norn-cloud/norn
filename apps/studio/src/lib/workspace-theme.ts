import { queryOptions, useQuery } from "@tanstack/react-query"
import { THEME_KIND, WORKSPACE_THEME_ID } from "../../../../tools/norn-ts/src/builder/theme.ts"
import {
  CellUnavailable,
  defaultFetch,
  findingsOf,
  parseUnavailableDetail,
  scopeFromAccount,
  scopedHostFetch,
  studioScopeKey,
  type Finding,
  type StudioScope,
} from "./definitions.ts"
import { useAccount } from "./session.ts"
import { parseServedTheme, type StudioTheme, type StudioThemeValidationIssue } from "./studio-theme.ts"

/**
 * Themes as data: the workspace's look is its `theme` definition (`workspace.theme`), admitted by
 * the cell's checker and served like any definition, so an agent or a person who may apply
 * definitions changes it with no redeploy. Studio reads it for the workspace it shows, gates it
 * again with the shared validators, and paints it; a reader's light/dark choice stays their own.
 */
export type ServedTheme =
  /** Not read yet for this workspace. */
  | { readonly kind: "loading" }
  /** The workspace has no theme (none installed, or retired): the built-in default paints. */
  | { readonly kind: "none" }
  | { readonly kind: "theme"; readonly theme: StudioTheme; readonly version: number }
  /** The server served a theme Studio's gate refuses: it never paints, and the page says why. */
  | { readonly kind: "invalid"; readonly version: number; readonly issues: readonly StudioThemeValidationIssue[] }
  /** No cell, no route, or the cell did not answer: the built-in default paints. */
  | { readonly kind: "unavailable" }

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/**
 * How often an open page re-reads its workspace's theme, so a theme an agent applies repaints every
 * open page with no reload. A conditional read of one small definition; it pauses while the tab is
 * hidden and runs again when the tab comes back.
 */
export const THEME_FEED_INTERVAL_MS = 4_000

const pinned = (scope: StudioScope) => async () => scope

/** Read the theme the server serves for one workspace. */
export async function readServedTheme(scope: StudioScope, fetcher: Fetch = defaultFetch, signal?: AbortSignal): Promise<ServedTheme> {
  let response: Response
  let body: unknown
  try {
    response = await scopedHostFetch(fetcher, "/api/host", pinned(scope))(`/v1/definitions/${encodeURIComponent(WORKSPACE_THEME_ID)}`, {
      headers: { accept: "application/json" },
      ...(signal === undefined ? {} : { signal }),
    })
    body = await response.json().catch(() => null)
  } catch {
    // No cell for this workspace, or one that did not answer: the default paints until it does.
    signal?.throwIfAborted()
    return { kind: "unavailable" }
  }
  const b = body as { kind?: unknown; version?: unknown; payload?: unknown; code?: unknown; lifecycle?: unknown } | null
  if (response.status === 404 && b?.code === "definition.not_found") return { kind: "none" }
  // A 200 that is not a definition read (no theme kind and no payload object) is no read at all.
  const isDefinition = b?.kind === THEME_KIND || (typeof b?.payload === "object" && b.payload !== null && !Array.isArray(b.payload))
  if (response.status !== 200 || typeof b?.version !== "number" || !isDefinition) return { kind: "unavailable" }
  if (b.lifecycle === "retired") return { kind: "none" }
  if (b.kind !== undefined && b.kind !== THEME_KIND) {
    return { kind: "invalid", version: b.version, issues: [{ code: "theme.validation.schema", path: "kind", message: `The workspace theme is a ${String(b.kind)} definition, not a theme.` }] }
  }
  const parsed = parseServedTheme(b.payload)
  return parsed.ok ? { kind: "theme", theme: parsed.theme, version: b.version } : { kind: "invalid", version: b.version, issues: parsed.issues }
}

export const servedThemeKey = (scope: StudioScope | null) => ["workspace-theme", studioScopeKey(scope)] as const

export function servedThemeQuery(scope: StudioScope | null, fetcher: Fetch = defaultFetch) {
  return queryOptions({
    queryKey: servedThemeKey(scope),
    queryFn: ({ signal }) => (scope === null ? Promise.resolve<ServedTheme>({ kind: "unavailable" }) : readServedTheme(scope, fetcher, signal)),
    refetchInterval: scope === null ? false : THEME_FEED_INTERVAL_MS,
    refetchOnWindowFocus: true,
    retry: false,
  })
}

/** The theme served for the workspace the session shows; it follows a workspace switch. */
export function useServedTheme(): { readonly scope: StudioScope | null; readonly served: ServedTheme } {
  const account = useAccount()
  const scope = account.data === undefined ? null : scopeFromAccount(account.data)
  const query = useQuery({ ...servedThemeQuery(scope), enabled: account.data !== undefined })
  return { scope, served: query.data ?? (account.data === undefined || query.isPending ? { kind: "loading" } : { kind: "unavailable" }) }
}

/** What applying or resetting the workspace theme came to, in the cell's own words. */
export type ThemeApplyResult =
  | { readonly applied: true; readonly version: number }
  | { readonly applied: false; readonly findings: readonly Finding[] }
  | { readonly applied: false; readonly conflict: true }
  | { readonly applied: false; readonly message: string }

async function post(scope: StudioScope, path: string, body: unknown, fetcher: Fetch): Promise<ThemeApplyResult> {
  let response: Response
  let answer: unknown
  try {
    response = await scopedHostFetch(fetcher, "/api/host", pinned(scope))(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) })
    answer = await response.json().catch(() => null)
  } catch (error) {
    return { applied: false, message: error instanceof Error && error.message !== "" ? error.message : "The cell did not answer; nothing was saved." }
  }
  const b = answer as { installed?: unknown; version?: unknown; code?: unknown; message?: unknown } | null
  if (response.status === 200 && b?.installed === true && typeof b.version === "number") return { applied: true, version: b.version }
  if (response.status === 409 && b?.code === "definition.version_conflict") return { applied: false, conflict: true }
  const findings = findingsOf(answer)
  if (findings.length > 0) return { applied: false, findings }
  const message = typeof b?.message === "string" && b.message !== "" ? b.message : new CellUnavailable(response.status, undefined, parseUnavailableDetail(answer)).message
  return { applied: false, message }
}

/**
 * Make `theme` this workspace's theme through the definitions apply path: the cell's checker admits
 * it or refuses it, and the cell decides whether this reader may apply definitions at all.
 */
export function applyWorkspaceTheme(scope: StudioScope, theme: StudioTheme, expectedVersion: number | undefined, fetcher: Fetch = defaultFetch): Promise<ThemeApplyResult> {
  return post(scope, "/v1/definitions:install", { kind: THEME_KIND, id: WORKSPACE_THEME_ID, payload: theme, ...(expectedVersion === undefined ? {} : { expectedVersion }) }, fetcher)
}

/** Retire the workspace's theme, so every page goes back to the built-in default; its history stays in the cell. */
export function resetWorkspaceTheme(scope: StudioScope, expectedVersion: number, fetcher: Fetch = defaultFetch): Promise<ThemeApplyResult> {
  return post(scope, `/v1/definitions/${encodeURIComponent(WORKSPACE_THEME_ID)}:retire`, { expectedVersion }, fetcher)
}
