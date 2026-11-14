import { expect, test } from "bun:test"
import { APPLY_PATH, PLAN_PATH, change, credentialsAllowed } from "../src/apply.ts"

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
