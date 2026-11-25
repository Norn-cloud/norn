// Themes as data: Studio reads the workspace's `theme` definition, gates it again, and applies or
// resets it through the definitions apply path; the cell decides, Studio shows its words.
import { expect, test } from "bun:test"
import { applyWorkspaceTheme, readServedTheme, resetWorkspaceTheme, servedThemeKey } from "../src/lib/workspace-theme.ts"
import type { StudioTheme } from "../src/lib/studio-theme.ts"

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

test("the served theme is read from the workspace's own definition, scoped to the workspace shown", async () => {
  const { calls, fetcher } = host(200, { id: "workspace.theme", kind: "theme", version: 3, payload: theme })
  expect(await readServedTheme(scope, fetcher)).toEqual({ kind: "theme", theme, version: 3 })
  expect(calls[0]!.url).toBe("/api/host/v1/definitions/workspace.theme?orgId=org_a&environmentId=env_a")
  // Two workspaces are two keys: switching workspace reads (and paints) the other one's theme.
  expect(servedThemeKey(scope)).not.toEqual(servedThemeKey({ orgId: "org_b", environmentId: "env_b" }))
})

test("no theme, a retired one, and a cell that does not answer all paint the default", async () => {
  expect(await readServedTheme(scope, host(404, { code: "definition.not_found", message: "no such definition" }).fetcher)).toEqual({ kind: "none" })
  expect(await readServedTheme(scope, host(200, { kind: "theme", version: 4, lifecycle: "retired", payload: theme }).fetcher)).toEqual({ kind: "none" })
  expect(await readServedTheme(scope, host(404, { code: "route.not_found" }).fetcher)).toEqual({ kind: "unavailable" })
  expect(await readServedTheme(scope, host(503, { message: "cold" }).fetcher)).toEqual({ kind: "unavailable" })
  expect(await readServedTheme(scope, async () => { throw new TypeError("network") })).toEqual({ kind: "unavailable" })
})

test("a served theme the shared gate refuses never paints: it comes back invalid, with the gate's issues", async () => {
  const faint = { version: 1, name: "Faint", colors: { "muted-foreground": "#c8c8c8" } }
  const read = await readServedTheme(scope, host(200, { kind: "theme", version: 2, payload: faint }).fetcher)
  expect(read.kind).toBe("invalid")
  if (read.kind === "invalid") {
    expect(read.version).toBe(2)
    expect(new Set(read.issues.map((issue) => issue.code))).toEqual(new Set(["theme.validation.contrast"]))
  }
  // A served colour outside the canonical forms is refused as the checker refuses it.
  const named = await readServedTheme(scope, host(200, { kind: "theme", version: 2, payload: { version: 1, name: "Named", colors: { foreground: "black" } } }).fetcher)
  expect(named.kind === "invalid" && named.issues.map((issue) => issue.code)).toEqual(["theme.validation.invalid_color"])
  const other = await readServedTheme(scope, host(200, { kind: "dashboard", version: 1, payload: theme }).fetcher)
  expect(other.kind).toBe("invalid")
})

test("applying installs the theme definition with the version it replaces; resetting retires it", async () => {
  const install = host(200, { installed: true, id: "workspace.theme", version: 4, receipt: { receiptHash: "a", commitSeq: 1, recordedAtMs: 1 } })
  expect(await applyWorkspaceTheme(scope, theme, 3, install.fetcher)).toEqual({ applied: true, version: 4 })
  const sent = install.calls[0]!
  expect(sent.url).toBe("/api/host/v1/definitions:install?orgId=org_a&environmentId=env_a")
  expect(JSON.parse(String(sent.init?.body))).toEqual({ kind: "theme", id: "workspace.theme", payload: theme, expectedVersion: 3 })
  expect(new Headers(sent.init?.headers).get("x-norn-expected-org")).toBe("org_a")
  const first = host(200, { installed: true, id: "workspace.theme", version: 1, receipt: {} })
  await applyWorkspaceTheme(scope, theme, undefined, first.fetcher)
  expect(JSON.parse(String(first.calls[0]!.init?.body))).not.toHaveProperty("expectedVersion")
  const retire = host(200, { installed: true, id: "workspace.theme", version: 5, receipt: {} })
  expect(await resetWorkspaceTheme(scope, 4, retire.fetcher)).toEqual({ applied: true, version: 5 })
  expect(retire.calls[0]!.url).toBe("/api/host/v1/definitions/workspace.theme:retire?orgId=org_a&environmentId=env_a")
})

test("a refusal comes back in the cell's own words: its findings, a conflict, or its message", async () => {
  const refused = host(422, { code: "definition.refused", message: "refused", diagnostics: [{ code: "theme.validation.contrast", message: "light muted-foreground (on canvas) contrast is 1.7:1", span: { pointer: "/payload/colors/muted-foreground" } }] })
  expect(await applyWorkspaceTheme(scope, theme, 1, refused.fetcher)).toEqual({ applied: false, findings: [{ code: "theme.validation.contrast", message: "light muted-foreground (on canvas) contrast is 1.7:1", law: null, pointer: "/payload/colors/muted-foreground" }] })
  expect(await applyWorkspaceTheme(scope, theme, 1, host(409, { code: "definition.version_conflict" }).fetcher)).toEqual({ applied: false, conflict: true })
  expect(await applyWorkspaceTheme(scope, theme, 1, host(403, { code: "access.denied", message: "Your role may not change this workspace's definitions." }).fetcher)).toEqual({ applied: false, message: "Your role may not change this workspace's definitions." })
})
