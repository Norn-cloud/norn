// Themes as data: Studio reads the workspace's `theme` artifact from `GET /v1/definitions`,
// gates it again, and applies or resets it through `POST /v1/cell/definitions:apply`.
// Includes regression tests for Findings 1, 3, 4, 5, and 6.
import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { applyAppearance, clearPublishedServedTheme } from "../src/lib/appearance.tsx"
import { captureThemeEditorSnapshot } from "../src/components/custom-theme-dialog.tsx"
import { readDocument } from "../src/lib/exact.ts"
import { parseServedTheme, parseStudioTheme, type StudioTheme } from "../src/lib/studio-theme.ts"
import { WorkspaceProvider, workspaceOf, type Workspace } from "../src/lib/workspace.tsx"
import {
  applyWorkspaceTheme,
  nextThemeArtifactVersion,
  readServedTheme,
  resetWorkspaceTheme,
  servedThemeKey,
  themeArtifactEnvelope,
  useServedTheme,
  type ServedTheme,
} from "../src/lib/workspace-theme.ts"

const scope = { orgId: "org_a", environmentId: "env_a" }
const theme: StudioTheme = { version: 1, name: "Paper", baseSkin: "polar", variants: { light: { canvas: "#f4f2ed" } } }

function host(status: number, body: unknown) {
  const calls: { url: string; init: RequestInit | undefined }[] = []
  const fetcher = async (url: string, init?: RequestInit) => {
    calls.push({ url, init })
    return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } })
  }
  return { calls, fetcher }
}

function catalog(version: number, artifacts: readonly unknown[]) {
  return { admitted: true, version, artifacts }
}

test("Finding 1: served theme is read from GET /v1/definitions, scoped to the workspace shown", async () => {
  const envelope = themeArtifactEnvelope(scope, theme, { id: "workspace.theme", version: "0.2.0", lifecycle: "active" })
  const { calls, fetcher } = host(200, catalog(3, [envelope]))
  expect(await readServedTheme(scope, fetcher)).toEqual({
    kind: "theme",
    id: "workspace.theme",
    artifactVersion: "0.2.0",
    theme,
    version: 3,
  })
  expect(calls[0]!.url).toBe("/api/host/v1/definitions?orgId=org_a&environmentId=env_a")
  // Two workspaces are two keys: switching workspace reads (and paints) the other one's theme.
  expect(servedThemeKey(scope)).not.toEqual(servedThemeKey({ orgId: "org_b", environmentId: "env_b" }))
})

test("no theme, a deprecated theme, and a cell that does not answer all paint the default", async () => {
  expect(await readServedTheme(scope, host(200, catalog(2, [])).fetcher)).toEqual({ kind: "none", version: 2 })
  const deprecated = themeArtifactEnvelope(scope, theme, { id: "workspace.theme", version: "0.3.0", lifecycle: "deprecated" })
  expect(await readServedTheme(scope, host(200, catalog(4, [deprecated])).fetcher)).toEqual({
    kind: "none",
    version: 4,
    artifactId: "workspace.theme",
    artifactVersion: "0.3.0",
  })
  expect(await readServedTheme(scope, host(404, { code: "route.not_found" }).fetcher)).toEqual({ kind: "unavailable" })
  expect(await readServedTheme(scope, host(503, { message: "cold" }).fetcher)).toEqual({ kind: "unavailable" })
  expect(await readServedTheme(scope, host(200, { ok: true }).fetcher)).toEqual({ kind: "unavailable" })
  expect(await readServedTheme(scope, async () => { throw new TypeError("network") })).toEqual({ kind: "unavailable" })
})

test("Findings 3 & 4: served themes with low contrast, invisible warning (#0000), or named colors (red/black) never paint", async () => {
  const faint = { version: 1, name: "Faint", colors: { "muted-foreground": "#c8c8c8" } }
  const read = await readServedTheme(scope, host(200, catalog(2, [themeArtifactEnvelope(scope, faint as StudioTheme)])).fetcher)
  expect(read.kind).toBe("invalid")
  if (read.kind === "invalid") {
    expect(read.version).toBe(2)
    expect(new Set(read.issues.map((issue) => issue.code))).toEqual(new Set(["theme.validation.contrast"]))
  }

  // Finding 3: named wire colors are rejected consistently across Ring-1, defineTheme, and parseServedTheme.
  for (const [token, raw] of [["chart-1", "red"], ["foreground", "black"]] as const) {
    const namedPayload = { version: 1, name: "Named", baseSkin: "luma", colors: { [token]: raw } }
    const served = await readServedTheme(scope, host(200, catalog(2, [themeArtifactEnvelope(scope, namedPayload as StudioTheme)])).fetcher)
    expect(served.kind).toBe("invalid")
    if (served.kind === "invalid") {
      expect(served.issues.map((issue) => issue.code)).toEqual(["theme.validation.invalid_color"])
    }
    expect(parseServedTheme(namedPayload).ok).toBe(false)
  }

  // Finding 4: invisible warning text (`#0000`) is rejected in both light and dark schemes.
  const invisibleWarning = { version: 1, name: "Hidden Warning", baseSkin: "luma", colors: { warning: "#0000" } }
  const warnRead = await readServedTheme(scope, host(200, catalog(2, [themeArtifactEnvelope(scope, invisibleWarning as StudioTheme)])).fetcher)
  expect(warnRead.kind).toBe("invalid")
  if (warnRead.kind === "invalid") {
    expect(new Set(warnRead.issues.map((issue) => issue.code))).toEqual(new Set(["theme.validation.contrast"]))
    expect(warnRead.issues.some((issue) => issue.path.includes("light.warning (on background)"))).toBe(true)
    expect(warnRead.issues.some((issue) => issue.path.includes("dark.warning (on card)"))).toBe(true)
  }
  expect(parseStudioTheme(invisibleWarning).ok).toBe(false)
})

test("Finding 1: applying and resetting both use POST /v1/cell/definitions:apply with compare-and-set expectedVersion", async () => {
  const applyHost = host(200, { outcome: "installed", definitionsVersion: 4, previousVersion: 3, receiptSha256: "a".repeat(64), commitSeq: 1 })
  expect(await applyWorkspaceTheme(scope, theme, 3, { id: "workspace.theme", previousArtifactVersion: "0.1.0", fetcher: applyHost.fetcher })).toEqual({ applied: true, version: 4 })
  const sent = applyHost.calls[0]!
  expect(sent.url).toBe("/api/host/v1/cell/definitions:apply?orgId=org_a&environmentId=env_a")
  expect(new Headers(sent.init?.headers).get("x-norn-expected-org")).toBe("org_a")
  expect(new Headers(sent.init?.headers).get("x-norn-expected-env")).toBe("env_a")
  const sentBody = JSON.parse(String(sent.init?.body)) as { expectedVersion: number; artifacts: readonly Record<string, unknown>[] }
  expect(sentBody.expectedVersion).toBe(3)
  expect(sentBody.artifacts).toHaveLength(1)
  expect(sentBody.artifacts[0]).toMatchObject({
    irVersion: "0.1",
    artifactId: "workspace.theme",
    kind: "theme",
    version: "0.1.1",
    lifecycle: "active",
    scope,
    payload: theme,
  })

  const resetHost = host(200, { outcome: "installed", definitionsVersion: 5, previousVersion: 4, receiptSha256: "b".repeat(64), commitSeq: 2 })
  expect(await resetWorkspaceTheme(scope, 4, { id: "workspace.theme", previousArtifactVersion: "0.1.1", fetcher: resetHost.fetcher })).toEqual({ applied: true, version: 5 })
  expect(resetHost.calls[0]!.url).toBe("/api/host/v1/cell/definitions:apply?orgId=org_a&environmentId=env_a")
  const resetBody = JSON.parse(String(resetHost.calls[0]!.init?.body)) as { expectedVersion: number; artifacts: readonly Record<string, unknown>[] }
  expect(resetBody.expectedVersion).toBe(4)
  expect(resetBody.artifacts[0]).toMatchObject({
    artifactId: "workspace.theme",
    kind: "theme",
    version: "0.1.2",
    lifecycle: "deprecated",
  })
  expect(nextThemeArtifactVersion(undefined)).toBe("0.1.0")
  expect(nextThemeArtifactVersion("0.1.9")).toBe("0.1.10")
})

test("refusals come back in the cell's own words: defs.refused findings, defs.stale conflict, or access denied", async () => {
  const refused = host(422, { code: "defs.refused", message: "refused", diagnostics: [{ code: "theme.validation.contrast", message: "light muted-foreground (on canvas) contrast is 1.7:1", span: { pointer: "/artifacts/0/payload/colors/muted-foreground" } }] })
  expect(await applyWorkspaceTheme(scope, theme, 1, { fetcher: refused.fetcher })).toEqual({
    applied: false,
    findings: [{ code: "theme.validation.contrast", message: "light muted-foreground (on canvas) contrast is 1.7:1", law: null, pointer: "/artifacts/0/payload/colors/muted-foreground" }],
  })
  expect(await applyWorkspaceTheme(scope, theme, 1, { fetcher: host(409, { code: "defs.stale" }).fetcher })).toEqual({ applied: false, conflict: true })
  expect(await applyWorkspaceTheme(scope, theme, 1, { fetcher: host(403, { code: "access.denied", message: "Your role may not change this workspace's definitions." }).fetcher })).toEqual({ applied: false, message: "Your role may not change this workspace's definitions." })
})

test("Finding 5: editor snapshot pins expectedVersion at open so concurrent revisions trigger a 409 conflict", async () => {
  const rev1: ServedTheme = { kind: "theme", id: "workspace.theme", artifactVersion: "0.1.0", theme, version: 1 }
  const snapshot = captureThemeEditorSnapshot(scope, rev1)
  expect(snapshot).toEqual({
    scope,
    expectedVersion: 1,
    artifactId: "workspace.theme",
    artifactVersion: "0.1.0",
    hasActiveTheme: true,
  })
  // Even if the live feed later advances to revision 2, the captured snapshot still submits expectedVersion: 1.
  let serverVersion = 2
  const calls: number[] = []
  const concurrentHost = async (_url: string, init?: RequestInit) => {
    const body = JSON.parse(String(init?.body)) as { expectedVersion: number }
    calls.push(body.expectedVersion)
    if (body.expectedVersion !== serverVersion) {
      return new Response(JSON.stringify({ code: "defs.stale", message: "stale" }), { status: 409, headers: { "content-type": "application/json" } })
    }
    serverVersion += 1
    return new Response(JSON.stringify({ outcome: "installed", definitionsVersion: serverVersion }), { status: 200, headers: { "content-type": "application/json" } })
  }
  expect(await applyWorkspaceTheme(snapshot!.scope, theme, snapshot!.expectedVersion, { id: snapshot!.artifactId, previousArtifactVersion: snapshot!.artifactVersion, fetcher: concurrentHost })).toEqual({ applied: false, conflict: true })
  expect(await resetWorkspaceTheme(snapshot!.scope, snapshot!.expectedVersion, { id: snapshot!.artifactId, previousArtifactVersion: snapshot!.artifactVersion, fetcher: concurrentHost })).toEqual({ applied: false, conflict: true })
  expect(calls).toEqual([1, 1])
  expect(serverVersion).toBe(2)
})

test("Finding 6: clearing published served theme synchronously removes workspace A's CSS overrides with zero bleed", () => {
  const styles = new Map<string, string>()
  const dataset: Record<string, string> = {}
  const classes = new Set<string>()
  const root = {
    dataset,
    classList: {
      contains: (name: string) => classes.has(name),
      toggle: (name: string, force?: boolean) => {
        const next = force ?? !classes.has(name)
        if (next) classes.add(name)
        else classes.delete(name)
        return next
      },
    },
    style: {
      setProperty: (k: string, v: string) => { styles.set(k, v) },
      removeProperty: (k: string) => { styles.delete(k) },
    },
  } as unknown as HTMLElement

  applyAppearance("luma", "sidebar", root, theme)
  expect(dataset.customTheme).toBe("Paper")
  expect(styles.get("--canvas")).toBe("#f4f2ed")

  // Transitioning away from workspace A or signing out clears all inline theme properties synchronously.
  clearPublishedServedTheme(undefined, root)
  expect(dataset.customTheme).toBeUndefined()
  expect(styles.size).toBe(0)
  expect(dataset.skin).toBe("luma")
})

test("Finding 6: useServedTheme follows WorkspaceProvider's decided scope, never a stale /api/me account scope", () => {
  const scopeA = { orgId: "org_a", environmentId: "env_a" }
  const scopeB = { orgId: "org_b", environmentId: "env_b" }
  const queryClient = new QueryClient()
  // Stale account cache still points to workspace A while WorkspaceProvider has moved to deciding or workspace B.
  queryClient.setQueryData(["account"], { user: { id: "u1", name: "Ada", email: "ada@example.com" }, organization: { id: "org_a", name: "Alpha", role: "owner" }, environmentId: "env_a" })
  queryClient.setQueryData(servedThemeKey(scopeA), { kind: "theme", id: "workspace.theme", artifactVersion: "0.1.0", theme, version: 2 } satisfies ServedTheme)

  const readUnder = (workspace: Workspace) => {
    let observed: ReturnType<typeof useServedTheme> | undefined
    function Probe() {
      observed = useServedTheme()
      return null
    }
    renderToStaticMarkup(
      createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(WorkspaceProvider, { workspace, children: createElement(Probe) }),
      ),
    )
    return observed!
  }

  const emptyCatalog = readDocument<readonly []>("[]", "live")
  const liveA = workspaceOf({ status: "success", data: { kind: "live", scope: scopeA, catalog: emptyCatalog, counts: {}, publishedQuestions: new Set(), builtins: [] } })
  const liveB = workspaceOf({ status: "success", data: { kind: "live", scope: scopeB, catalog: emptyCatalog, counts: {}, publishedQuestions: new Set(), builtins: [] } })
  const deciding = workspaceOf({ status: "pending" })
  const recorded = workspaceOf({ status: "success", data: { kind: "recorded" } })

  // Under workspace A, A's theme is served.
  expect(readUnder(liveA)).toEqual({ scope: scopeA, served: { kind: "theme", id: "workspace.theme", artifactVersion: "0.1.0", theme, version: 2 } })
  // Transitioning to deciding immediately drops A's scope and theme, despite stale account cache.
  expect(readUnder(deciding)).toEqual({ scope: null, served: { kind: "loading" } })
  // Transitioning to workspace B (before B's query resolves) follows B's scope and never leaks A's theme.
  expect(readUnder(liveB)).toEqual({ scope: scopeB, served: { kind: "loading" } })
  // Transitioning to recorded mode clears to unavailable.
  expect(readUnder(recorded)).toEqual({ scope: null, served: { kind: "unavailable" } })
})
