/**
 * An action names the access policy that authorizes calls (ActionIR `access`):
 * by `{ artifactId, version? }` (a policy whose rules name the action cannot be
 * referenced from it without a cycle) or by a Norn.access reference.
 */
import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { TOOL_ROOT } from "../src/profile.ts"

const cli = join(TOOL_ROOT, "src/cli.ts")

function compile(source: string): { status: number | null; out: string } {
  const root = mkdtempSync(join(tmpdir(), "norn-access-"))
  try {
    const dir = join(root, "p")
    expect(spawnSync(process.execPath, [cli, "init", dir, "--no-install", "--template", "empty"], { encoding: "utf8" }).status).toBe(0)
    writeFileSync(join(dir, "src", "notes.ts"), source)
    const run = spawnSync(process.execPath, [cli, "compile", join(dir, "src"), "--org", "local", "--env", "dev"], { cwd: dir, encoding: "utf8" })
    return { status: run.status, out: run.stdout }
  } finally { rmSync(root, { recursive: true }) }
}

const head = `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
const Fields = Schema.Struct({ note: Schema.String })
export const Note = Norn.type({ name: "app.Note", title: "Note", version: "0.1.0", properties: Fields, identity: [{ name: "primary", properties: ["note"] }] })
`
const add = (access: string) => `export const AddNote = Norn.action({
  id: "app.note.add", title: "Add note", version: "0.1.0", input: Fields, output: Fields,
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, ${access}
  writes: [{ type: Note, change: "created" }],
  effect: { kind: "upsert_input_object", type: Note, objectIdField: "note", writeMode: { kind: "create_only" } },
})
`
const policy = (actions: string) => `export const NoteAccess = Norn.access({ id: "app.note.access", title: "Notes", version: "0.2.0",
  rules: [{ ruleId: "add", actions: [${actions}], resourceType: Note, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member"] } }] }] })
`

function accessOf(out: string): unknown {
  const artifacts = JSON.parse(out) as Array<{ artifactId: string; payload: { access?: unknown } }>
  return artifacts.find((artifact) => artifact.artifactId === "app.note.add.action")!.payload.access
}

test("an action names its policy by artifact id, even when the policy's rules name the action", () => {
  const run = compile(head + add(`access: { artifactId: "app.note.access.policy", version: "0.2.0" },`) + policy("AddNote"))
  expect(run.status).toBe(0)
  expect(accessOf(run.out)).toEqual({ artifactId: "app.note.access.policy", version: "0.2.0" })
}, 60_000)

test("or by a Norn.access reference, which pins the policy's version", () => {
  const run = compile(head + policy(`"app.note.add"`) + add("access: NoteAccess,"))
  expect(run.status).toBe(0)
  expect(accessOf(run.out)).toEqual({ artifactId: "app.note.access.policy", version: "0.2.0" })
}, 60_000)

test("without access the payload has none, and a non-policy artifact id is refused", () => {
  expect(accessOf(compile(head + add("")).out)).toBeUndefined()
  const bad = compile(head + add(`access: { artifactId: "app.note.add.action" },`))
  expect(bad.status).toBe(1)
  expect(bad.out).toContain("access names a policy artifact")
}, 60_000)
