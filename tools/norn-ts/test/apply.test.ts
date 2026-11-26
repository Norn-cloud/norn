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

// #1469: an approval the cell refuses at apply (admitted at plan, refused when the apply re-admits
// it) fails the command with the cell's refusal, and nothing else is sent: the client never installs
// the set or an approval in a request of its own, so the one refused apply is all there is. The cell's
// side (nothing published, no call commits) is pinned by
// crates/norn-cell-do/tests/hosted_install.rs `a_failed_apply_installs_no_ungated_action_and_no_call_commits`.
test("an approval refused at apply fails the command and nothing else is sent", async () => {
  const seen: string[] = []
  const server = Bun.serve({
    port: 0,
    async fetch(request) {
      const url = new URL(request.url)
      seen.push(`${request.method} ${url.pathname.replace("/api/host", "")}`)
      await request.text()
      if (url.pathname.endsWith(PLAN_PATH)) return Response.json({ baseVersion: 2, nextVersion: 3, unchanged: false, added: [{ artifactId: "lab.batch.close", kind: "action", toVersion: "0.1.0" }], changed: [], same: [], newObjectTypes: [], checkedObjects: 0, diagnostics: [], approvals: [{ id: "lab.close_approval", unchanged: false, restatement: "words", restatementSha256: "cd" }] })
      return Response.json({ code: "definition.signoff_mismatch", message: "the approval's words changed", diagnostics: [{ severity: "error", code: "definition.signoff_mismatch", message: "signed other words", span: { pointer: "/approvals/0/signoff" } }] }, { status: 422 })
    },
  })
  try {
    const target = { url: `http://127.0.0.1:${server.port}/api/host`, org: "o", env: "e", headers: {} }
    const approval = { kind: "approval" as const, id: "lab.close_approval", payload: { action: "lab.batch.close", decider: { roles: ["admin"] }, question: "Close?", risk: "low" } }
    const refused = await change(target, "apply", [{ artifactId: "lab.batch.close" }], false, undefined, [approval])
    expect(refused.ok).toBe(false)
    expect(seen).toEqual([`POST ${PLAN_PATH}`, `POST ${APPLY_PATH}`])
    expect(refused.out.join("\n")).toContain("refused (422) definition.signoff_mismatch")
    expect(refused.out.join("\n")).not.toContain("installed revision")
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
  const { pack } = loadPack(root, { orgId: "org_hosted", environmentId: "prod" })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  const ids = pack.artifacts.map((artifact) => (artifact as { artifactId: string }).artifactId)
  expect(ids.some((id) => id.startsWith("purchasing."))).toBe(true)
  expect(pack.retained.map((definition) => definition.id)).toEqual(["purchasing.large_requests"])
  const scopes = new Set(pack.artifacts.map((artifact) => JSON.stringify((artifact as { scope?: unknown }).scope)))
  expect(scopes.has(JSON.stringify({ orgId: "org_purchasing", environmentId: "dev" }))).toBe(false)
})

// DEV-READ-DEFINITIONS-1: a project's source questions and dashboards are installed by `norn dev`
// alone; a hosted cell takes them only through the definitions API. `norn plan`, `apply` and
// `check` leave them out, and say so: a note line for each, and `omittedDefinitions` in the JSON.
test("plan, apply and check name each source question and dashboard they leave out", async () => {
  const { loadPack } = await import("../src/apply.ts")
  const { HOSTED_READS_REASON } = await import("../src/worker.ts")
  const { TOOL_ROOT } = await import("../src/profile.ts")
  const { mkdtempSync, readFileSync, rmSync } = await import("node:fs")
  const { tmpdir } = await import("node:os")
  const { join } = await import("node:path")
  const CLI = join(TOOL_ROOT, "src/cli.ts")
  const base = mkdtempSync(join(tmpdir(), "norn-omitted-"))
  const dir = join(base, "lab")
  try {
    const init = Bun.spawnSync([process.execPath, CLI, "init", dir, "--template", "supply-lab", "--no-install", "--json"])
    expect(init.exitCode).toBe(0)
    const pack = join(TOOL_ROOT, "../../registry/packs/supply-lab/questions")
    const bundled = [
      ...JSON.parse(readFileSync(join(pack, "supply.questions.json"), "utf8")).map((q: { name: string }) => ({ kind: "question", id: q.name })),
      { kind: "dashboard", id: "supply.dashboard.supply_lab" },
    ]
    const key = (d: { kind: string; id: string }) => `${d.kind} ${d.id}`
    const { pack: loaded, omitted } = loadPack(dir, { orgId: "org_hosted", environmentId: "prod" })
    if (!loaded.ok) throw new Error(JSON.stringify(loaded.failures))
    expect(omitted.map(key).sort()).toEqual(bundled.map(key).sort())
    expect(new Set(omitted.map((d) => d.reason))).toEqual(new Set([HOSTED_READS_REASON]))
    expect(loaded.retained.filter((d) => d.kind !== "approval")).toEqual([])

    const bodies: any[] = []
    const server = Bun.serve({
      port: 0,
      async fetch(request) {
        bodies.push(await request.json())
        return Response.json({ baseVersion: 2, nextVersion: 3, unchanged: false, added: [], changed: [], same: [], newObjectTypes: [], checkedObjects: 0, diagnostics: [] })
      },
    })
    try {
      const target = { url: `http://127.0.0.1:${server.port}/api/host`, org: "o", env: "e", headers: {} }
      const json = await change(target, "plan", [{ artifactId: "lab.Batch.entity" }], true, undefined, [], omitted)
      expect(JSON.parse(json.out[0]!).omittedDefinitions).toEqual(omitted)
      const applied = await change(target, "apply", [{ artifactId: "lab.Batch.entity" }], true, undefined, [], omitted)
      expect(applied.out.length).toBeGreaterThan(0)
      for (const line of applied.out) expect(JSON.parse(line).omittedDefinitions).toEqual(omitted)
      const text = await change(target, "plan", [{ artifactId: "lab.Batch.entity" }], false, undefined, [], omitted)
      for (const definition of bundled) expect(text.out).toContain(`note: ${definition.kind} ${definition.id} is not sent: ${HOSTED_READS_REASON}`)
      // Nothing of theirs was sent.
      expect(JSON.stringify(bodies)).not.toContain("supply.dashboard.supply_lab")
    } finally {
      server.stop(true)
    }

    if (!process.env.NORN_CHECKER_WASM_PKG) {
      if (process.env.CI) throw new Error("the check row requires NORN_CHECKER_WASM_PKG")
      return
    }
    const check = Bun.spawnSync([process.execPath, CLI, "check", dir])
    const report = JSON.parse(check.stdout.toString())
    expect(report.omittedDefinitions.map(key).sort()).toEqual(bundled.map(key).sort())
    expect(check.stderr.toString()).toContain(`note: dashboard supply.dashboard.supply_lab is not sent: ${HOSTED_READS_REASON}`)
  } finally {
    rmSync(base, { recursive: true, force: true })
  }
})
