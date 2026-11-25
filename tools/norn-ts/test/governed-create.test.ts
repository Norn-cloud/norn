/**
 * CREATE-WRITER-1 / ACTOR-STAMP-1 / ACTOR-INDEPENDENT-1 (#1101, #1091): a pack
 * author writes a governed create whose writer computes the new record, whose
 * creator the server stamps into a protected property (`fromCaller` +
 * `protected`), and whose decision an independent principal makes
 * (`independentOf`). Neutral domains: an Accounting invoice and a Supply
 * Chain transfer. The generated artifacts are the Rust cell fixture.
 */
import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { checkPack, compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const root = join(TOOL_ROOT, "../..")
const scope = { orgId: "org_demo", environmentId: "dev" }
const fixture = join(TOOL_ROOT, "fixtures/decisions/governed-create.source.ts")

type Artifact = { readonly artifactId: string; readonly payload: Record<string, any> }
const find = (artifacts: ReadonlyArray<unknown>, id: string) =>
  (artifacts as ReadonlyArray<Artifact>).find((artifact) => artifact.artifactId === id)!

test("the governed create fixture is generated from its typed declaration", () => {
  const pack = compilePack(fixture, { scope, root, modules: [fixture] })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  expect(pack.artifacts).toEqual(JSON.parse(readFileSync(join(root, "crates/norn-cell/tests/fixtures/governed-create.json"), "utf8")))

  // ACTOR-STAMP-1: the creator is a protected property, stamped from the
  // verified principal and never taken from caller input.
  const invoice = find(pack.artifacts, "ledger.Invoice.entity").payload.objectType
  expect(invoice.properties.find((p: { id: string }) => p.id === "submitted_by")).toEqual({ id: "submitted_by", protected: true, type: { kind: "text" } })
  expect(invoice.properties.filter((p: { protected?: boolean }) => p.protected === true)).toHaveLength(1)
  const submit = find(pack.artifacts, "ledger.invoice.submit.action").payload
  expect(submit.derive).toEqual([{ field: "submitted_by", expression: "actor.subjectId", inputRequired: false }])
  // CREATE-WRITER-1: a pinned writer composes with a create-only create.
  expect(submit.effectProgram.writeMode).toEqual({ kind: "create_only" })
  expect(submit.synchronousRule).toEqual({ artifactId: "ledger.invoice.writer.rule", version: "1.0.0" })
  // ACTOR-INDEPENDENT-1: the deciders name the protected creator, not input.
  expect(find(pack.artifacts, "ledger.invoice.approve.action").payload.independentOf).toEqual(["submitted_by"])
  expect(find(pack.artifacts, "supply.transfer.release.action").payload.independentOf).toEqual(["requested_by"])
}, 30_000)

test("admission accepts the governed create and refuses a caller-written creator (Ring 0)", () => {
  if (!checker) return
  const pack = compilePack(fixture, { scope, root, modules: [fixture] })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  const admitted = checkPack(checker, pack.artifacts, [])
  expect(admitted.diagnostics.diagnostics.filter((d) => d.severity === "error")).toEqual([])
  expect(admitted.admitted).toBe(true)

  const forged = structuredClone(pack.artifacts) as unknown as Array<Artifact>
  delete find(forged, "ledger.invoice.submit.action").payload.derive
  find(forged, "ledger.invoice.approve.action").payload.independentOf = ["tax_code"]
  const refused = checkPack(checker, forged, [])
  expect(refused.admitted).toBe(false)
  const codes = refused.diagnostics.diagnostics.map((d) => d.code)
  expect(codes).toContain("artifact_set.action.target_not_writable")
  expect(codes).toContain("artifact_set.action.independent_actor")
}, 60_000)

const cli = join(TOOL_ROOT, "src/cli.ts")

function compile(source: string): { status: number | null; out: string; err: string } {
  const dir = mkdtempSync(join(tmpdir(), "norn-governed-create-"))
  try {
    const project = join(dir, "p")
    expect(spawnSync(process.execPath, [cli, "init", project, "--no-install", "--template", "empty"], { encoding: "utf8" }).status).toBe(0)
    writeFileSync(join(project, "src", "ledger.ts"), source)
    const run = spawnSync(process.execPath, [cli, "compile", join(project, "src"), "--org", "local", "--env", "dev"], { cwd: project, encoding: "utf8" })
    return { status: run.status, out: run.stdout, err: run.stderr }
  } finally { rmSync(dir, { recursive: true }) }
}

const typeWith = (protectedMember: string) => `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Invoice = Norn.type({
  name: "ledger.Invoice", title: "Invoice", version: "0.1.0",
  properties: Schema.Struct({ invoice_id: Schema.String, submitted_by: Schema.String }),
  ${protectedMember}
})
`

test("protected names declared properties of the type, once each", () => {
  const ok = compile(typeWith(`protected: ["submitted_by"],`))
  expect(ok.status).toBe(0)
  const artifacts = JSON.parse(ok.out) as ReadonlyArray<Artifact>
  expect(find(artifacts, "ledger.Invoice.entity").payload.objectType.properties).toContainEqual({ id: "submitted_by", protected: true, type: { kind: "text" } })
  // An undeclared name is a type error in typed authoring; a repeat is refused by the compiler.
  for (const [bad, code] of [[`protected: ["approver"],`, "NORN-FLOW-TYPE"], [`protected: ["submitted_by", "submitted_by"],`, "NORN-FLOW-LITERAL"]] as const) {
    const run = compile(typeWith(bad))
    expect(run.status).not.toBe(0)
    expect(run.out + run.err).toContain(code)
  }
}, 120_000)
