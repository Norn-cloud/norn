import { expect, test } from "bun:test"
import { resolve } from "node:path"
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

// #1469: a pack's approvals travel with its set. The plan returns the words
// to sign for each; the one apply carries each sign-off, so the cell installs
// the set and its approvals together. No request installs an approval on its
// own, and a refusal at plan applies nothing.
test("apply sends the pack's approvals with the set, signing the plan's restatement", async () => {
  const seen: { method: string; path: string; body: any }[] = []
  let admissible = true
  const words = "Every call of lab.batch.close waits for a person to approve it."
  const server = Bun.serve({
    port: 0,
    async fetch(request) {
      const url = new URL(request.url)
      seen.push({ method: request.method, path: url.pathname.replace("/api/host", ""), body: request.method === "POST" ? await request.json() : null })
      if (url.pathname.endsWith(PLAN_PATH)) return admissible
        ? Response.json({ baseVersion: 2, nextVersion: 3, unchanged: false, added: [], changed: [], same: [], newObjectTypes: [], checkedObjects: 0, diagnostics: [], approvals: [{ id: "lab.close_approval", unchanged: false, restatement: words, restatementSha256: "cd" }] })
        : Response.json({ code: "defs.refused", message: "not admissible", diagnostics: [{ severity: "error", code: "definition.approval_role_unknown", message: "no such role", span: { pointer: "/approvals/0/payload/decider/roles/0" } }] }, { status: 422 })
      return Response.json({ outcome: "installed", definitionsVersion: 3, previousVersion: 2, receiptSha256: "ab", commitSeq: 9, plan: { approvals: [{ id: "lab.close_approval", unchanged: false, restatement: words, restatementSha256: "cd" }] } })
    },
  })
  try {
    const target = { url: `http://127.0.0.1:${server.port}/api/host`, org: "o", env: "e", headers: {} }
    const approval = { kind: "approval" as const, id: "lab.close_approval", payload: { action: "lab.batch.close", decider: { roles: ["admin"] }, question: "Close?", risk: "low" } }
    const applied = await change(target, "apply", [{ artifactId: "lab.Batch.entity" }], false, undefined, [approval])
    expect(applied.ok).toBe(true)
    expect(seen.map((s) => `${s.method} ${s.path}`)).toEqual([`POST ${PLAN_PATH}`, `POST ${APPLY_PATH}`])
    expect(seen[0]!.body.approvals).toEqual([approval])
    expect(seen[1]!.body).toEqual({ artifacts: [{ artifactId: "lab.Batch.entity" }], expectedVersion: 2,
      approvals: [{ ...approval, expectedVersion: null, signoff: { restatementSha256: "cd" } }] })
    expect(applied.out.join("\n")).toContain(`+ approval lab.close_approval, signed:\n      ${words}`)

    // A given --expected-version still plans, for the words to sign.
    seen.length = 0
    expect((await change(target, "apply", [], false, 2, [approval])).ok).toBe(true)
    expect(seen.map((s) => s.path)).toEqual([PLAN_PATH, APPLY_PATH])

    const planned = await change(target, "plan", [], false, undefined, [approval])
    expect(planned.out.join("\n")).toContain(`+ approval lab.close_approval, signs:\n      ${words}`)

    admissible = false
    seen.length = 0
    const refused = await change(target, "apply", [], false, undefined, [approval])
    expect(refused.ok).toBe(false)
    expect(seen.map((s) => s.path)).toEqual([PLAN_PATH])
    expect(refused.out.join("\n")).toContain("error definition.approval_role_unknown at /approvals/0/payload/decider/roles/0: no such role")
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

// #1469 S8: `norn apply <project-root>` reads the project through the shared
// loader `norn dev` uses (its manifest's `src` modules and JSON definitions),
// scoped to the target cell rather than the manifest's dev scope.
test("apply reads a project root through the shared project loader", async () => {
  const { loadPack } = await import("../src/apply.ts")
  const root = resolve(import.meta.dir, "../../../fixtures/examples/purchase-requests")
  const pack = loadPack(root, { orgId: "org_hosted", environmentId: "prod" })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  const ids = pack.artifacts.map((artifact) => (artifact as { artifactId: string }).artifactId)
  expect(ids.some((id) => id.startsWith("purchasing."))).toBe(true)
  expect(pack.retained.map((definition) => definition.id)).toEqual(["purchasing.large_requests"])
  const scopes = new Set(pack.artifacts.map((artifact) => JSON.stringify((artifact as { scope?: unknown }).scope)))
  expect(scopes.has(JSON.stringify({ orgId: "org_purchasing", environmentId: "dev" }))).toBe(false)
})
