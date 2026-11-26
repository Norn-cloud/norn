import { expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { ProjectError, parseProject } from "../src/project.ts"
import { readSeed, seed, seedSession } from "../src/seed.ts"

// `norn seed` reads its session from `.norn/dev.json` and sends each call as its actor. A token
// file is read only from inside the project, symlinks resolved; a malformed descriptor or seed
// document is a ProjectError, never a TypeError; a refusal whose body is not JSON is still a
// refusal; and a call parked for a decision is reported, not refused.

const DESCRIPTOR = {
  format: "norn-dev/1", url: "http://127.0.0.1:4174", tokenFile: ".norn/dev.token",
  scope: { orgId: "local", environmentId: "dev" }, actor: "local:developer",
  reviewer: { actor: "local:developer+reviewer", tokenFile: ".norn/dev.reviewer.token" },
  principals: [{ actor: "clerk:hana", roles: ["warehouse_clerk"], tokenFile: ".norn/dev.principal.1.token" }],
}

function project(descriptor: unknown = DESCRIPTOR): { root: string; outside: string; done: () => void } {
  const base = mkdtempSync(join(tmpdir(), "norn-seed-"))
  const root = join(base, "project")
  const outside = join(base, "outside")
  mkdirSync(join(root, ".norn"), { recursive: true })
  mkdirSync(outside)
  writeFileSync(join(outside, "secret"), "outside-secret\n")
  for (const file of ["dev.token", "dev.reviewer.token", "dev.principal.1.token"]) writeFileSync(join(root, ".norn", file), `${file}-bearer\n`)
  writeFileSync(join(root, ".norn/dev.json"), typeof descriptor === "string" ? descriptor : JSON.stringify(descriptor))
  return { root, outside, done: () => rmSync(base, { recursive: true, force: true }) }
}

const refusal = (run: () => unknown): ProjectError => {
  try { run() } catch (error) { if (error instanceof ProjectError) return error; throw error }
  throw new Error("expected a ProjectError")
}

test("seed_session_reads_every_local_principal", () => {
  const lab = project()
  try {
    const session = seedSession(lab.root)
    expect(session.developer).toBe("local:developer")
    expect(Object.fromEntries(session.tokens)).toEqual({
      "local:developer": "dev.token-bearer",
      "local:developer+reviewer": "dev.reviewer.token-bearer",
      "clerk:hana": "dev.principal.1.token-bearer",
    })
  } finally { lab.done() }
})

test("seed_session_refuses_a_token_file_symlinked_out_of_the_project", () => {
  const lab = project()
  try {
    // Lexically inside `.norn`, really outside the project.
    rmSync(join(lab.root, ".norn/dev.principal.1.token"))
    symlinkSync(join(lab.outside, "secret"), join(lab.root, ".norn/dev.principal.1.token"))
    expect(refusal(() => seedSession(lab.root)).message).toContain("outside the project")
    // A symlinked directory leading out is refused the same way.
    rmSync(join(lab.root, ".norn/dev.principal.1.token"))
    symlinkSync(lab.outside, join(lab.root, "linked"))
    writeFileSync(join(lab.root, ".norn/dev.json"), JSON.stringify({ ...DESCRIPTOR, principals: [{ ...DESCRIPTOR.principals[0], tokenFile: "linked/secret" }] }))
    expect(refusal(() => seedSession(lab.root)).message).toContain("outside the project")
    // And a lexical escape still is.
    writeFileSync(join(lab.root, ".norn/dev.json"), JSON.stringify({ ...DESCRIPTOR, tokenFile: "../outside/secret" }))
    expect(refusal(() => seedSession(lab.root)).message).toContain("outside the project")
  } finally { lab.done() }
})

test("seed_session_refuses_a_malformed_descriptor_as_a_project_error", () => {
  const malformed: unknown[] = [
    "not json",
    [],
    { ...DESCRIPTOR, scope: undefined },
    { ...DESCRIPTOR, scope: { orgId: "local" } },
    { ...DESCRIPTOR, scope: "local/dev" },
    { ...DESCRIPTOR, actor: undefined },
    { ...DESCRIPTOR, actor: 7 },
    { ...DESCRIPTOR, reviewer: "local:developer+reviewer" },
    { ...DESCRIPTOR, principals: {} },
    { ...DESCRIPTOR, principals: [null] },
    { ...DESCRIPTOR, principals: [{ tokenFile: ".norn/dev.principal.1.token" }] },
    { ...DESCRIPTOR, principals: [{ actor: "clerk:hana" }] },
  ]
  for (const descriptor of malformed) {
    const lab = project(descriptor)
    try {
      expect(refusal(() => seedSession(lab.root)).code).toBe("cli.project.io")
    } finally { lab.done() }
  }
})

test("seed_reports_a_refusal_whose_body_is_not_json", async () => {
  const lab = project()
  try {
    const session = seedSession(lab.root)
    const calls = [{ actor: "clerk:hana", action: "supply.movement.record", idempotencyKey: "k1", input: {} }]
    const html: typeof fetch = Object.assign(async () => new Response("<html>bad gateway</html>", { status: 502 }), { preconnect: () => {} })
    expect(await seed(session, calls, html)).toEqual({ ok: false, step: "supply.movement.record as clerk:hana (k1)", status: 502, refusal: "<html>bad gateway</html>" })
    const empty: typeof fetch = Object.assign(async () => new Response("", { status: 503 }), { preconnect: () => {} })
    expect(await seed(session, calls, empty)).toMatchObject({ ok: false, status: 503, refusal: null })
    const json: typeof fetch = Object.assign(async () => Response.json({ code: "authz.denied" }, { status: 403 }), { preconnect: () => {} })
    expect(await seed(session, calls, json)).toMatchObject({ ok: false, status: 403, refusal: { code: "authz.denied" } })
  } finally { lab.done() }
})

test("manifest_reserves_the_built_in_principal_names", () => {
  const manifest = {
    format: "norn-project/1", name: "lab", sources: ["src"], scope: { orgId: "local", environmentId: "dev" },
    dev: { port: 4174, actor: "local:developer", principals: [{ actor: "clerk:hana", roles: ["warehouse_clerk"] }] }, cell: ".norn/cell.sqlite",
  }
  expect(parseProject(manifest).dev.principals).toEqual([{ actor: "clerk:hana", roles: ["warehouse_clerk"] }])
  for (const actor of ["developer", "reviewer", "local:developer", "local:developer+reviewer"]) {
    expect(() => parseProject({ ...manifest, dev: { ...manifest.dev, principals: [{ actor, roles: ["warehouse_clerk"] }] } })).toThrow(ProjectError)
  }
})

test("seed_session_reads_through_a_symlinked_project_root", () => {
  const lab = project()
  try {
    const linked = join(lab.outside, "linked-project")
    symlinkSync(lab.root, linked)
    expect(seedSession(linked).tokens.get("clerk:hana")).toBe("dev.principal.1.token-bearer")
  } finally { lab.done() }
})

test("seed_session_refuses_a_missing_or_directory_token_file_as_a_project_error", () => {
  const lab = project()
  try {
    rmSync(join(lab.root, ".norn/dev.principal.1.token"))
    const missing = refusal(() => seedSession(lab.root))
    expect({ code: missing.code, missing: missing.message.includes("is missing") }).toEqual({ code: "cli.project.io", missing: true })
    mkdirSync(join(lab.root, ".norn/dev.principal.1.token"))
    const directory = refusal(() => seedSession(lab.root))
    expect({ code: directory.code, message: directory.message.includes("is not a file") }).toEqual({ code: "cli.project.io", message: true })
  } finally { lab.done() }
})

test("read_seed_refuses_a_corrupt_or_malformed_document_as_a_project_error", () => {
  const call = { actor: "clerk:hana", action: "supply.movement.record", idempotencyKey: "k1", input: {} }
  const malformed: string[] = [
    "not json",
    "null",
    "[]",
    "7",
    JSON.stringify({ format: "norn-calls/2", calls: [] }),
    JSON.stringify({ format: "norn-calls/1", calls: {} }),
    JSON.stringify({ format: "norn-calls/1", calls: [null] }),
    JSON.stringify({ format: "norn-calls/1", calls: [{ ...call, actor: 7 }] }),
    JSON.stringify({ format: "norn-calls/1", calls: [{ ...call, idempotencyKey: "" }] }),
    JSON.stringify({ format: "norn-calls/1", calls: [{ ...call, input: [] }] }),
  ]
  for (const document of malformed) {
    const lab = project()
    try {
      mkdirSync(join(lab.root, "seed"))
      writeFileSync(join(lab.root, "seed/calls.json"), document)
      expect({ document, code: refusal(() => readSeed(lab.root)).code }).toEqual({ document, code: "cli.project.io" })
    } finally { lab.done() }
  }
  const lab = project()
  try {
    mkdirSync(join(lab.root, "seed"))
    writeFileSync(join(lab.root, "seed/calls.json"), JSON.stringify({ format: "norn-calls/1", calls: [call] }))
    expect(readSeed(lab.root)).toEqual([call])
  } finally { lab.done() }
})

test("seed_reports_a_call_parked_for_a_decision_and_goes_on", async () => {
  const lab = project()
  try {
    const session = seedSession(lab.root)
    const calls = [
      { actor: "clerk:hana", action: "supply.po.create", idempotencyKey: "gated", input: {} },
      { actor: "clerk:hana", action: "supply.movement.record", idempotencyKey: "after", input: {} },
    ]
    const sent: string[] = []
    const gated: typeof fetch = Object.assign(async (url: string | URL | Request) => {
      sent.push(String(url))
      return String(url).includes("supply.po.create")
        ? Response.json({ outcome: "pending", decision: { id: "dec-1", state: "pending" } }, { status: 202 })
        : Response.json({ outcome: "committed" }, { status: 200 })
    }, { preconnect: () => {} })
    expect(await seed(session, calls, gated)).toEqual({
      ok: true, calls: 2, parked: [{ step: "supply.po.create as clerk:hana (gated)", decision: { id: "dec-1", state: "pending" } }],
    })
    expect(sent).toHaveLength(2)
  } finally { lab.done() }
})
