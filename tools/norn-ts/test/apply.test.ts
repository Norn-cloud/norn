import { expect, test } from "bun:test"
import { ADMIT_PATH, APPLY_PATH, INSTALL_PATH, PLAN_PATH, change, credentialsAllowed } from "../src/apply.ts"

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
      if (url.pathname.endsWith(PLAN_PATH)) return Response.json({ baseVersion: 2, nextVersion: 3, unchanged: false, added: [{ artifactId: "lab.Batch.entity", kind: "entity", toVersion: "0.1.0" }], changed: [], same: [], newObjectTypes: ["lab.Batch"], checkedObjects: 0, diagnostics: [] })
      return Response.json({ outcome: "installed", definitionsVersion: 3, previousVersion: 2, receiptSha256: "ab", commitSeq: 9 })
    },
  })
  try {
    const target = { url: `http://127.0.0.1:${server.port}/api/host`, org: "org a", env: "prod", headers: { authorization: "Bearer t" } }
    const applied = await change(target, "apply", [{ artifactId: "lab.Batch.entity" }], false)
    expect(applied.ok).toBe(true)
    expect(seen.map((s) => s.path)).toEqual([`/api/host${PLAN_PATH}`, `/api/host${APPLY_PATH}`])
    expect(seen[0]!.query).toBe("?orgId=org+a&environmentId=prod")
    expect(seen[0]!.body).toEqual({ artifacts: [{ artifactId: "lab.Batch.entity" }] })
    expect(seen[1]!.body.expectedVersion).toBe(2)
    expect(seen[1]!.auth).toBe("Bearer t")
    expect(applied.out.join("\n")).toContain("new types: lab.Batch")
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

// A pack's approvals are installed after its set, by signing the cell's
// restatement; one the cell refuses fails the apply.
test("apply installs the pack's approvals after the set, signing their restatement", async () => {
  const seen: { method: string; path: string; body: any }[] = []
  let admissible = true
  const server = Bun.serve({
    port: 0,
    async fetch(request) {
      const url = new URL(request.url)
      seen.push({ method: request.method, path: url.pathname.replace("/api/host", ""), body: request.method === "POST" ? await request.json() : null })
      if (url.pathname.endsWith(PLAN_PATH)) return Response.json({ baseVersion: 2, nextVersion: 3, unchanged: false, added: [], changed: [], same: [], newObjectTypes: [], checkedObjects: 0, diagnostics: [] })
      if (url.pathname.endsWith(APPLY_PATH)) return Response.json({ outcome: "installed", definitionsVersion: 3, previousVersion: 2, receiptSha256: "ab", commitSeq: 9 })
      if (request.method === "GET") return Response.json({ code: "definition.not_found", message: "no such definition" }, { status: 404 })
      if (url.pathname.endsWith(ADMIT_PATH)) return Response.json(admissible
        ? { admitted: true, diagnostics: [], restatement: "Every call of lab.batch.close waits for a person to approve it.", restatementSha256: "cd" }
        : { admitted: false, diagnostics: [{ severity: "error", code: "definition.approval_role_unknown", message: "no such role", span: { pointer: "/decider/roles/0" } }], restatement: null, restatementSha256: null })
      return Response.json({ id: "lab.close_approval", version: 1 })
    },
  })
  try {
    const target = { url: `http://127.0.0.1:${server.port}/api/host`, org: "o", env: "e", headers: {} }
    const approval = { kind: "approval" as const, id: "lab.close_approval", payload: { action: "lab.batch.close", decider: { roles: ["admin"] }, question: "Close?", risk: "low" } }
    const applied = await change(target, "apply", [{ artifactId: "lab.Batch.entity" }], false, undefined, [approval])
    expect(applied.ok).toBe(true)
    expect(seen.map((s) => `${s.method} ${s.path}`)).toEqual([`POST ${PLAN_PATH}`, `POST ${APPLY_PATH}`, "GET /v1/definitions/lab.close_approval", `POST ${ADMIT_PATH}`, `POST ${INSTALL_PATH}`])
    expect(seen[4]!.body).toEqual({ ...approval, expectedVersion: null, signoff: { restatementSha256: "cd" } })
    expect(applied.out.join("\n")).toContain("+ approval lab.close_approval v1, signing:\n    Every call of lab.batch.close waits")

    const planned = await change(target, "plan", [], false, undefined, [approval])
    expect(planned.out.join("\n")).toContain("then approval lab.close_approval, signed on apply")

    admissible = false
    seen.length = 0
    const refused = await change(target, "apply", [], false, undefined, [approval])
    expect(refused.ok).toBe(false)
    expect(seen.map((s) => s.path)).not.toContain(INSTALL_PATH)
    expect(refused.out.join("\n")).toContain("error definition.approval_role_unknown at /decider/roles/0: no such role")
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
