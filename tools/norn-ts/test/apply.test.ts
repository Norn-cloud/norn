import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { authoredArtifacts, bindSourcePins, checkPack, compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"
import { APPLY_PATH, PLAN_PATH, change, credentialsAllowed, studioSession } from "../src/apply.ts"

// HOST-INSTALL-1 client: `norn apply` plans first, applies against the planned
// revision, and stops at the cell's refusal. The cell itself is pinned by
// crates/norn-cell-do/tests/hosted_install.rs; this pins only what the client sends.
test("apply plans first, applies against the planned revision, and stops at a refusal", async () => {
  const seen: { path: string; query: string; body: any; auth: string | null }[] = []
  let refuse = false
  const server = Bun.serve({
    port: 0,
    async fetch(request) {
      const url = new URL(request.url)
      seen.push({ path: url.pathname, query: url.search, body: await request.json(), auth: request.headers.get("authorization") })
      if (refuse) return Response.json({ code: "defs.refused", message: "not admissible", explanation: "nothing changed", diagnostics: [{ severity: "error", code: "law.x", message: "bad", span: { pointer: "/artifacts/1" } }] }, { status: 422 })
      if (url.pathname.endsWith(PLAN_PATH)) return Response.json({ baseVersion: 2, nextVersion: 3, unchanged: false, added: [{ artifactId: "jewelry.Melt.entity", kind: "entity", toVersion: "0.1.0" }], changed: [], same: [], newObjectTypes: ["jewelry.Melt"], checkedObjects: 0, diagnostics: [] })
      return Response.json({ outcome: "installed", definitionsVersion: 3, previousVersion: 2, receiptSha256: "ab", commitSeq: 9 })
    },
  })
  try {
    const target = { url: `http://127.0.0.1:${server.port}/api/host`, org: "org a", env: "prod", headers: { authorization: "Bearer t" } }
    const applied = await change(target, "apply", [{ artifactId: "jewelry.Melt.entity" }], false)
    expect(applied.ok).toBe(true)
    expect(seen.map((s) => s.path)).toEqual([`/api/host${PLAN_PATH}`, `/api/host${APPLY_PATH}`])
    expect(seen[0]!.query).toBe("?orgId=org+a&environmentId=prod")
    expect(seen[0]!.body).toEqual({ artifacts: [{ artifactId: "jewelry.Melt.entity" }] })
    expect(seen[1]!.body.expectedVersion).toBe(2)
    expect(seen[1]!.auth).toBe("Bearer t")
    expect(applied.out.join("\n")).toContain("new types: jewelry.Melt")
    expect(applied.out.join("\n")).toContain("installed revision 3 over 2")

    refuse = true
    seen.length = 0
    const refused = await change(target, "apply", [{ artifactId: "x" }], false)
    expect(refused.ok).toBe(false)
    expect(seen.map((s) => s.path)).toEqual([`/api/host${PLAN_PATH}`])
    expect(refused.out.join("\n")).toContain("error law.x at /artifacts/1: bad")
  } finally {
    server.stop(true)
  }
})

test("credentials go only over HTTPS or to loopback", () => {
  expect(credentialsAllowed("https://studio.example/api/host")).toBe(true)
  expect(credentialsAllowed("http://127.0.0.1:8787")).toBe(true)
  expect(credentialsAllowed("http://studio.example/api/host")).toBe(false)
  expect(credentialsAllowed("http://localhost.example")).toBe(false)
})

// A member of two workspaces applies to the one that is not their default:
// the session selects `--org` through Studio's own workspace route before
// the plan, and the (stand-in) server decides from the session's workspace.
test("an account in several workspaces applies to the one --org names", async () => {
  const active = new Map<string, string>()
  const member: Record<string, string[]> = { "ada@example.com": ["org_default", "org_lending"] }
  const server = Bun.serve({
    port: 0,
    async fetch(request) {
      const url = new URL(request.url)
      const sid = /sid=([^;]+)/.exec(request.headers.get("cookie") ?? "")?.[1]
      if (url.pathname === "/api/auth/sign-in/email") {
        const { email } = await request.json() as { email: string }
        active.set(email, member[email]![0]!)
        return new Response("{}", { headers: { "set-cookie": `sid=${email}; Path=/; HttpOnly` } })
      }
      if (url.pathname === "/api/auth/organization/set-active") {
        const { organizationId } = await request.json() as { organizationId: string }
        if (!sid || !member[sid]?.includes(organizationId)) return Response.json({ code: "FORBIDDEN" }, { status: 403 })
        active.set(sid, organizationId)
        return new Response("{}", { headers: { "set-cookie": `cache=${organizationId}; Path=/` } })
      }
      // The host decides from the session's workspace, never from the query alone.
      const org = url.searchParams.get("orgId")
      if (!sid || active.get(sid) !== org) return Response.json({ code: "authz.denied" }, { status: 403 })
      if (url.pathname.endsWith(PLAN_PATH)) return Response.json({ baseVersion: 1, nextVersion: 2, unchanged: false, added: [], changed: [], same: [], newObjectTypes: [], checkedObjects: 0, diagnostics: [] })
      return Response.json({ outcome: "installed", definitionsVersion: 2, previousVersion: 1, receiptSha256: "ab", commitSeq: 3 })
    },
  })
  try {
    const origin = `http://127.0.0.1:${server.port}`
    const cookie = await studioSession(origin, "ada@example.com", "pw", "org_lending")
    expect(cookie).toContain("sid=ada@example.com")
    expect(cookie).toContain("cache=org_lending")
    const target = { url: `${origin}/api/host`, org: "org_lending", env: "prod", headers: { cookie, origin } }
    const applied = await change(target, "apply", [{ artifactId: "lending.Loan.entity" }], false)
    expect(applied.ok).toBe(true)
    expect(applied.out.join("\n")).toContain("installed revision 2 over 1")
    // A workspace the account is not a member of is refused by the server.
    await expect(studioSession(origin, "ada@example.com", "pw", "org_other")).rejects.toThrow("refused workspace org_other")
  } finally {
    server.stop(true)
  }
})

// `norn compile|plan|apply --org --env` stamps the tenant's envelope on a
// pack's source schema; the mapping's authored pin names the schema as the
// registry carries it. Bound to the compiled schema by the checker's hash,
// the tenant-compiled pack passes the checker the hosted plan runs, where the
// unbound one was refused with mapping.model.source_hash_mismatch.
test("a tenant compile binds each mapping to the source schema it is compiled with", () => {
  if (!checker) {
    if (process.env.CI) throw new Error("source pins need the real Wasm checker in CI")
    return
  }
  const root = resolve(TOOL_ROOT, "../..")
  const dir = join(root, "registry/packs/documents")
  const options = { scope: { orgId: "org_lending", environmentId: "prod" }, root }
  const pack = compilePack(dir, options)
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  const authored = authoredArtifacts(dir, options)
  if (!authored) throw new Error("the documents pack compiles as its registry package")
  // The hosted cell's own trusted base (crates/norn-cell-do/src/base.rs), and
  // the core and canon artifacts the documents pack names, as the hosted
  // documents tests provision them (crates/norn-cell-do/tests/held_back.rs).
  const read = (path: string) => JSON.parse(readFileSync(join(root, path), "utf8"))
  const base = [...readFileSync(join(root, "crates/norn-cell-do/src/base.rs"), "utf8").matchAll(/"\.\.\/\.\.\/\.\.\/(registry\/core\/artifacts\/[^"]+\.json)"/g)]
    .map((match) => read(match[1]!))
  expect(base.length).toBeGreaterThan(10)
  const named = [
    "registry/core/artifacts/norn.SourceEvidence.interface.json",
    "registry/core/artifacts/norn.SourceDocument.interface.json",
    "registry/core/artifacts/norn.ObservedLinkClaim.entity.json",
    "registry/packs/canon/artifacts/canon.ItemizedDocument.interface.json",
    "registry/packs/canon/artifacts/canon.LineItem.interface.json",
    "registry/packs/canon/artifacts/canon.ItemizedDocument.total_matches_lines.law.json",
  ].map(read)
  const codes = (artifacts: ReadonlyArray<unknown>) =>
    checkPack(checker!, [...named, ...artifacts] as never, base).diagnostics.diagnostics
      .filter((d) => d.severity === "error").map((d) => d.code)
  expect(codes(pack.artifacts)).toContain("mapping.model.source_hash_mismatch")
  const bound = bindSourcePins(checker, pack.artifacts, authored)
  expect(codes(bound)).toEqual([])
  // A pin that names neither form of its schema is left for the checker to refuse.
  const forged = pack.artifacts.map((artifact: any) => artifact.kind === "mapping"
    ? { ...artifact, payload: { ...artifact.payload, source: { ...artifact.payload.source, sha256: "0".repeat(64) } } }
    : artifact)
  expect(codes(bindSourcePins(checker, forged, authored))).toContain("mapping.model.source_hash_mismatch")
}, 120_000)
