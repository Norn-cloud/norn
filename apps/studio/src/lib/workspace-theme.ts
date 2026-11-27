import { queryOptions, useQuery } from "@tanstack/react-query"
import { THEME_KIND, WORKSPACE_THEME_ID } from "../../../../tools/norn-ts/src/builder/theme.ts"
import {
  CellUnavailable,
  defaultFetch,
  findingsOf,
  parseUnavailableDetail,
  scopedHostFetch,
  studioScopeKey,
  type Finding,
  type StudioScope,
} from "./definitions.ts"
import { parseServedTheme, type StudioTheme, type StudioThemeValidationIssue } from "./studio-theme.ts"
import { useWorkspace } from "./workspace.tsx"

/**
 * Themes as data: the workspace's look is its `theme` artifact in the cell's kernel definition
 * set (`GET /v1/definitions`, mutated through `POST /v1/cell/definitions:apply`), admitted by
 * Ring-1 `norn-ui-checker` and pushed on the live catalog feed. Studio reads it for the decided
 * workspace (`useWorkspace()`), gates it again with the shared validators, and paints it; a
 * reader's light/dark choice stays their own.
 */
export type ServedTheme =
  /** Not read yet for this workspace (or workspace mode is still deciding). */
  | { readonly kind: "loading" }
  /** The workspace has no active theme (none installed, or deprecated): the built-in default paints. */
  | {
    readonly kind: "none"
    readonly version: number
    readonly artifactId?: string
    readonly artifactVersion?: string
  }
  | {
    readonly kind: "theme"
    readonly id: string
    readonly artifactVersion: string
    readonly theme: StudioTheme
    /** The cell's active definitions version (expectedVersion for the next `:apply`). */
    readonly version: number
  }
  /** The server served a theme Studio's gate refuses: it never paints, and the page says why. */
  | {
    readonly kind: "invalid"
    readonly id: string
    readonly artifactVersion: string
    readonly version: number
    readonly issues: readonly StudioThemeValidationIssue[]
  }
  /** No cell, no route, or the cell did not answer: the built-in default paints. */
  | { readonly kind: "unavailable" }

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/**
 * How often an open page re-reads its workspace's definitions catalog, so a theme an agent applies
 * repaints every open page with no reload. Pauses while the tab is hidden and runs again on focus.
 */
export const THEME_FEED_INTERVAL_MS = 4_000

const INACTIVE_LIFECYCLES = new Set(["deprecated", "superseded", "rejected"])

const pinned = (scope: StudioScope) => async () => scope

const isObject = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value)

/** Bump the patch segment of an artifact semver (`"0.1.0"` → `"0.1.1"`), defaulting to `"0.1.0"`. */
export function nextThemeArtifactVersion(previous?: string): string {
  const match = previous === undefined ? null : /^(\d+)\.(\d+)\.(\d+)$/.exec(previous.trim())
  if (match === null) return "0.1.0"
  return `${match[1]}.${match[2]}.${Number(match[3]) + 1}`
}

/** Build the canonical `theme` artifact envelope accepted by `POST /v1/cell/definitions:plan` and `:apply`. */
export function themeArtifactEnvelope(
  scope: StudioScope,
  theme: StudioTheme,
  options?: {
    readonly id?: string
    readonly version?: string
    readonly lifecycle?: "active" | "deprecated"
  },
): Record<string, unknown> {
  const artifactId = options?.id ?? WORKSPACE_THEME_ID
  const dot = artifactId.indexOf(".")
  const namespace = dot > 0 ? artifactId.slice(0, dot) : "workspace"
  return {
    irVersion: "0.1",
    artifactId,
    kind: THEME_KIND,
    namespace,
    name: theme.name,
    version: options?.version ?? "0.1.0",
    lifecycle: options?.lifecycle ?? "active",
    scope: { orgId: scope.orgId, environmentId: scope.environmentId },
    dependencies: [],
    provenance: { source: "authoring_sdk" },
    payload: theme,
  }
}

interface CatalogThemeEntry {
  readonly id: string
  readonly artifactVersion: string
  readonly lifecycle: string
  readonly payload: unknown
}

function extractThemeEntries(artifacts: readonly unknown[]): readonly CatalogThemeEntry[] {
  const entries: CatalogThemeEntry[] = []
  for (const raw of artifacts) {
    if (!isObject(raw) || raw.kind !== THEME_KIND) continue
    entries.push({
      id: typeof raw.artifactId === "string" && raw.artifactId !== "" ? raw.artifactId : WORKSPACE_THEME_ID,
      artifactVersion: typeof raw.version === "string" && raw.version !== "" ? raw.version : "0.1.0",
      lifecycle: typeof raw.lifecycle === "string" ? raw.lifecycle : "active",
      payload: raw.payload,
    })
  }
  return entries
}

/** Read the theme the server serves for one workspace from `GET /v1/definitions`. */
export async function readServedTheme(scope: StudioScope, fetcher: Fetch = defaultFetch, signal?: AbortSignal): Promise<ServedTheme> {
  let response: Response
  let body: unknown
  try {
    response = await scopedHostFetch(fetcher, "/api/host", pinned(scope))("/v1/definitions", {
      headers: { accept: "application/json" },
      ...(signal === undefined ? {} : { signal }),
    })
    body = await response.json().catch(() => null)
  } catch {
    // No cell for this workspace, or one that did not answer: the default paints until it does.
    signal?.throwIfAborted()
    return { kind: "unavailable" }
  }
  if (response.status !== 200 || !isObject(body) || typeof body.version !== "number" || !Array.isArray(body.artifacts)) {
    return { kind: "unavailable" }
  }
  const definitionsVersion = body.version
  const allThemes = extractThemeEntries(body.artifacts)
  const active = allThemes.filter((entry) => !INACTIVE_LIFECYCLES.has(entry.lifecycle))
  if (active.length === 0) {
    const retired = allThemes.find((entry) => entry.id === WORKSPACE_THEME_ID) ?? allThemes[0]
    return {
      kind: "none",
      version: definitionsVersion,
      ...(retired === undefined ? {} : { artifactId: retired.id, artifactVersion: retired.artifactVersion }),
    }
  }
  if (active.length > 1) {
    const first = active[0]!
    return {
      kind: "invalid",
      id: first.id,
      artifactVersion: first.artifactVersion,
      version: definitionsVersion,
      issues: [{ code: "theme.validation.schema", path: "artifacts", message: "This workspace serves more than one active theme." }],
    }
  }
  const only = active[0]!
  const parsed = parseServedTheme(only.payload)
  return parsed.ok
    ? { kind: "theme", id: only.id, artifactVersion: only.artifactVersion, theme: parsed.theme, version: definitionsVersion }
    : { kind: "invalid", id: only.id, artifactVersion: only.artifactVersion, version: definitionsVersion, issues: parsed.issues }
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

/**
 * The theme served for the decided workspace (`useWorkspace()`). Bound directly to the workspace
 * provider's decided scope so switching workspaces never reads or retains the prior workspace's
 * theme while an account refresh is pending.
 */
export function useServedTheme(): { readonly scope: StudioScope | null; readonly served: ServedTheme } {
  const workspace = useWorkspace()
  const scope = workspace.kind === "live" ? workspace.scope : null
  const query = useQuery({ ...servedThemeQuery(scope), enabled: workspace.kind === "live" })
  if (workspace.kind === "deciding") return { scope: null, served: { kind: "loading" } }
  if (workspace.kind !== "live") return { scope: null, served: { kind: "unavailable" } }
  return { scope, served: query.data ?? (query.isPending ? { kind: "loading" } : { kind: "unavailable" }) }
}

/** What applying or resetting the workspace theme came to, in the cell's own words. */
export type ThemeApplyResult =
  | { readonly applied: true; readonly version: number }
  | { readonly applied: false; readonly findings: readonly Finding[] }
  | { readonly applied: false; readonly conflict: true }
  | { readonly applied: false; readonly message: string }

export interface ThemeWriteOptions {
  readonly id?: string
  readonly previousArtifactVersion?: string
  readonly fetcher?: Fetch
}

async function applyEnvelope(
  scope: StudioScope,
  expectedVersion: number,
  envelope: Record<string, unknown>,
  fetcher: Fetch,
): Promise<ThemeApplyResult> {
  let response: Response
  let answer: unknown
  try {
    response = await scopedHostFetch(fetcher, "/api/host", pinned(scope))("/v1/cell/definitions:apply", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ expectedVersion, artifacts: [envelope] }),
    })
    answer = await response.json().catch(() => null)
  } catch (error) {
    return { applied: false, message: error instanceof Error && error.message !== "" ? error.message : "The cell did not answer; nothing was saved." }
  }
  const b = isObject(answer) ? answer : null
  if (response.status === 200 && typeof b?.definitionsVersion === "number") {
    return { applied: true, version: b.definitionsVersion }
  }
  if (response.status === 409 && (b?.code === "defs.stale" || b?.code === "definition.version_conflict")) {
    return { applied: false, conflict: true }
  }
  const findings = findingsOf(answer)
  if (findings.length > 0) return { applied: false, findings }
  const message = typeof b?.message === "string" && b.message !== "" ? b.message : new CellUnavailable(response.status, undefined, parseUnavailableDetail(answer)).message
  return { applied: false, message }
}

/**
 * Make `theme` this workspace's theme through `POST /v1/cell/definitions:apply`: the cell's Ring-1
 * checker admits or refuses it, compare-and-set on `expectedVersion`, and the cell decides whether
 * this reader may write definitions at all.
 */
export function applyWorkspaceTheme(
  scope: StudioScope,
  theme: StudioTheme,
  expectedVersion: number,
  options: ThemeWriteOptions = {},
): Promise<ThemeApplyResult> {
  const envelope = themeArtifactEnvelope(scope, theme, {
    id: options.id ?? WORKSPACE_THEME_ID,
    version: nextThemeArtifactVersion(options.previousArtifactVersion),
    lifecycle: "active",
  })
  return applyEnvelope(scope, expectedVersion, envelope, options.fetcher ?? defaultFetch)
}

/**
 * Reset the workspace to the built-in default by deprecating its `theme` artifact through
 * `POST /v1/cell/definitions:apply` (compare-and-set on `expectedVersion`).
 */
export function resetWorkspaceTheme(
  scope: StudioScope,
  expectedVersion: number,
  options: ThemeWriteOptions = {},
): Promise<ThemeApplyResult> {
  const envelope = themeArtifactEnvelope(
    scope,
    { version: 1, name: "Default", baseSkin: "luma" },
    {
      id: options.id ?? WORKSPACE_THEME_ID,
      version: nextThemeArtifactVersion(options.previousArtifactVersion),
      lifecycle: "deprecated",
    },
  )
  return applyEnvelope(scope, expectedVersion, envelope, options.fetcher ?? defaultFetch)
}
