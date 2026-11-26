import { expect, test } from "bun:test"
import { resolve } from "node:path"
import {
  APPLY_PATH,
  PLAN_PATH,
  change,
  credentialsAllowed,
  extractCookieHeader,
  normalizeTargetUrl,
  post,
  readBoundedText,
} from "../src/apply.ts"

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
  expect(credentialsAllowed("https://studio.example/api/host?extra=1")).toBe(false)
  expect(credentialsAllowed("https://studio.example/api/host#frag")).toBe(false)
  expect(credentialsAllowed("https://studio.example/api/host?")).toBe(false)
  expect(credentialsAllowed("https://studio.example/api/host#")).toBe(false)
  expect(credentialsAllowed("https://user:pass@studio.example/api/host")).toBe(false)
})

test("normalizeTargetUrl rejects query strings, fragments, userinfo, and non-HTTP schemes", () => {
  expect(normalizeTargetUrl("https://studio.example/api/host///")).toBe("https://studio.example/api/host")
  expect(normalizeTargetUrl("http://127.0.0.1:8787/")).toBe("http://127.0.0.1:8787")
  for (const bad of [
    "https://studio.example/api/host?foo=bar",
    "https://studio.example/api/host?",
    "https://studio.example/api/host#section",
    "https://studio.example/api/host#",
    "https://alice:secret@studio.example/api/host",
    "ftp://studio.example/api/host",
    "not-a-url",
  ]) {
    expect(() => normalizeTargetUrl(bad)).toThrow()
  }

  let redactedError = ""
  try {
    normalizeTargetUrl("https://alice:pwsecret@studio.example/api/host?token=querysecret#fragsecret")
  } catch (error) {
    redactedError = error instanceof Error ? error.message : String(error)
  }
  expect(redactedError).toContain("?<redacted>#<redacted>")
  expect(redactedError).not.toContain("pwsecret")
  expect(redactedError).not.toContain("querysecret")
  expect(redactedError).not.toContain("fragsecret")
})

test("extractCookieHeader deduplicates cookies last-write-wins and honors Max-Age/Expires deletions", () => {
  const now = Date.parse("2026-11-26T21:00:00Z")
  expect(
    extractCookieHeader(
      [
        "session=old; Path=/; HttpOnly",
        "csrf=tok1; Path=/",
        "session=new; Path=/; Secure",
      ],
      now,
    ),
  ).toBe("session=new; csrf=tok1")

  // Max-Age=0 deletes an earlier cookie.
  expect(
    extractCookieHeader(
      [
        "session=old; Path=/",
        "session=; Max-Age=0; Path=/",
        "theme=dark; Path=/",
      ],
      now,
    ),
  ).toBe("theme=dark")

  // Max-Age takes precedence over Expires per RFC 6265 §5.3.
  expect(
    extractCookieHeader(
      [
        "session=alive; Max-Age=3600; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/",
        "gone=1; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/",
      ],
      now,
    ),
  ).toBe("session=alive")

  // Duplicate Expires attributes: the last valid Expires attribute wins (RFC 6265).
  expect(
    extractCookieHeader(
      [
        "revived=ok; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Expires=Fri, 01 Jan 2027 00:00:00 GMT",
        "expired=no; Expires=Fri, 01 Jan 2027 00:00:00 GMT; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
        "kept=yes; Expires=Fri, 01 Jan 2027 00:00:00 GMT; Expires=not-a-date",
      ],
      now,
    ),
  ).toBe("revived=ok; kept=yes")

  // Malformed or CRLF-containing cookies are skipped.
  expect(
    extractCookieHeader(["noequals", "=noname", "bad=a\r\nb", "ok=1=2; Path=/"], now),
  ).toBe("ok=1=2")
})

test("readBoundedText decodes split multi-byte UTF-8 chunks and does not hang on stalled stream cancellation", async () => {
  // Split 3-byte "€" (e2 82 ac) and 4-byte "🚀" (f0 9f 9a 80) across chunk boundaries.
  const full = new TextEncoder().encode("{\"currency\":\"€\",\"rocket\":\"🚀\"}")
  const splitA = full.indexOf(0x82)
  const splitB = full.indexOf(0x9a)
  const utf8Stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(full.subarray(0, splitA))
      controller.enqueue(full.subarray(splitA, splitB))
      controller.enqueue(full.subarray(splitB))
      controller.close()
    },
  })
  const decoded = await readBoundedText(new Response(utf8Stream), 1024, "utf8", Date.now() + 5_000)
  expect(JSON.parse(decoded)).toEqual({ currency: "€", rocket: "🚀" })

  // Stalled cancellation after chunk 1 was read must still reject promptly at deadline.
  let pulledCount = 0
  let cancelCalled = false
  const stalledStream = new ReadableStream<Uint8Array>({
    pull(controller) {
      pulledCount++
      if (pulledCount === 1) {
        controller.enqueue(new TextEncoder().encode("{\"partial\":"))
        return
      }
      return new Promise(() => {})
    },
    cancel() {
      cancelCalled = true
      return new Promise(() => {})
    },
  })
  const startedAt = Date.now()
  await expect(
    readBoundedText(new Response(stalledStream), 1024, PLAN_PATH, startedAt + 50),
  ).rejects.toThrow(`${PLAN_PATH} timed out reading response`)
  expect(pulledCount).toBeGreaterThanOrEqual(2)
  expect(cancelCalled).toBe(true)
  expect(Date.now() - startedAt).toBeLessThan(1_000)

  // Stalled cancellation on Content-Length limit refusal must also return promptly.
  let clCancelCalled = false
  const clStream = new ReadableStream<Uint8Array>({
    cancel() {
      clCancelCalled = true
      return new Promise(() => {})
    },
  })
  await expect(
    readBoundedText(
      new Response(clStream, { headers: { "content-length": "99999" } }),
      256,
      PLAN_PATH,
      Date.now() + 5_000,
    ),
  ).rejects.toThrow(`${PLAN_PATH} response exceeds size limit (256 bytes)`)
  expect(clCancelCalled).toBe(true)
})

test("post enforces response byte bounds, HTTP timeouts, redirects, and target URL validation", async () => {
  let streamSlowChunkSent = false
  const server = Bun.serve({
    port: 0,
    async fetch(request) {
      const url = new URL(request.url)
      if (url.pathname === "/redirect/v1/cell/definitions:plan") {
        return new Response(null, { status: 302, headers: { location: "https://evil.example/" } })
      }
      if (url.pathname === "/large/v1/cell/definitions:plan") {
        return new Response(JSON.stringify({ pad: "x".repeat(4096) }), {
          status: 200,
          headers: { "content-type": "application/json" },
        })
      }
      if (url.pathname === "/stream-large/v1/cell/definitions:plan") {
        return new Response(
          new ReadableStream({
            start(controller) {
              controller.enqueue(new TextEncoder().encode("{\"a\":\""))
              controller.enqueue(new TextEncoder().encode("y".repeat(2048)))
              controller.enqueue(new TextEncoder().encode("\"}"))
              controller.close()
            },
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        )
      }
      if (url.pathname === "/slow/v1/cell/definitions:plan") {
        await Bun.sleep(500)
        return Response.json({ ok: true })
      }
      if (url.pathname === "/stream-slow/v1/cell/definitions:plan") {
        return new Response(
          new ReadableStream({
            async start(controller) {
              controller.enqueue(new TextEncoder().encode("{\"ok\":"))
              streamSlowChunkSent = true
              await Bun.sleep(500)
              try {
                controller.enqueue(new TextEncoder().encode("true}"))
                controller.close()
              } catch {
                // Stream already cancelled on timeout.
              }
            },
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        )
      }
      return Response.json({ ok: true })
    },
  })
  try {
    await expect(
      post({ url: `http://127.0.0.1:${server.port}/api?inject=1`, org: "o", env: "e", headers: {} }, PLAN_PATH, {}),
    ).rejects.toThrow("query or fragment")

    await expect(
      post({ url: `http://127.0.0.1:${server.port}/redirect`, org: "o", env: "e", headers: {} }, PLAN_PATH, {}),
    ).rejects.toThrow("unexpected redirect (302)")

    await expect(
      post(
        { url: `http://127.0.0.1:${server.port}/large`, org: "o", env: "e", headers: {}, maxResponseBytes: 256 },
        PLAN_PATH,
        {},
      ),
    ).rejects.toThrow("exceeds size limit")

    await expect(
      post(
        { url: `http://127.0.0.1:${server.port}/stream-large`, org: "o", env: "e", headers: {}, maxResponseBytes: 256 },
        PLAN_PATH,
        {},
      ),
    ).rejects.toThrow("exceeds size limit")

    await expect(
      post(
        { url: `http://127.0.0.1:${server.port}/slow`, org: "o", env: "e", headers: {}, timeoutMs: 50 },
        PLAN_PATH,
        {},
      ),
    ).rejects.toThrow(/timed out|aborted/i)

    await expect(
      post(
        { url: `http://127.0.0.1:${server.port}/stream-slow`, org: "o", env: "e", headers: {}, timeoutMs: 80 },
        PLAN_PATH,
        {},
      ),
    ).rejects.toThrow(/timed out|aborted/i)
    expect(streamSlowChunkSent).toBe(true)
  } finally {
    server.stop(true)
  }
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

test("apply CLI rejects non-integer --expected-version and worker rejects escaping JSON symlinks before reading", async () => {
  const { main } = await import("../src/apply.ts")
  const { compileProjectPack } = await import("../src/worker.ts")
  const { readProject } = await import("../src/project.ts")
  const { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } = await import("node:fs")
  const { tmpdir } = await import("node:os")
  const { join } = await import("node:path")

  const root = mkdtempSync(join(tmpdir(), "norn-apply-hardening-"))
  try {
    const projectDir = join(root, "proj")
    mkdirSync(join(projectDir, "src"), { recursive: true })
    writeFileSync(join(projectDir, "norn.json"), JSON.stringify({
      format: "norn-project/1", name: "proj", sources: ["src"],
      scope: { orgId: "o1", environmentId: "e1" }, dev: { port: 4174, actor: "local:developer" }, cell: ".norn/cell.sqlite",
    }))

    for (const bad of [["--expected-version", "abc"], ["--expected-version", "--json"], ["--expected-version", "-1"]]) {
      const code = await main("apply", [projectDir, "--url", "http://127.0.0.1:1", "--org", "o1", "--env", "e1", ...bad])
      expect(code).toBe(2)
    }

    for (const badUrl of [
      "https://studio.example/api/host?extra=1",
      "https://studio.example/api/host#frag",
      "https://user:secret@studio.example/api/host",
    ]) {
      const code = await main("plan", [projectDir, "--url", badUrl, "--org", "o1", "--env", "e1"])
      expect(code).toBe(2)
    }

    // Symlink pointing outside project to non-JSON content must fail confinement before JSON.parse.
    const outsideSecret = join(root, "secret.txt")
    writeFileSync(outsideSecret, "NOT JSON SECRET CONTENT")
    symlinkSync(outsideSecret, join(projectDir, "src/leak.json"))
    const { root: pRoot, manifest } = readProject(projectDir)
    expect(() => compileProjectPack(pRoot, manifest)).toThrow("does not resolve inside the project")

    // Symlink pointing outside project to non-artifact JSON must also fail confinement.
    rmSync(join(projectDir, "src/leak.json"))
    const outsideJson = join(root, "plain.json")
    writeFileSync(outsideJson, JSON.stringify({ hello: "world" }))
    symlinkSync(outsideJson, join(projectDir, "src/plain.json"))
    expect(() => compileProjectPack(pRoot, manifest)).toThrow("does not resolve inside the project")
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
