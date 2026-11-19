/**
 * An absent value on an action is `Schema.NullOr` of a plain scalar, lowered to
 * the one nullable leaf ActionIR admits (`["string" | "integer" | "number" | "boolean", "null"]`,
 * no sibling keywords). The same struct can declare a type's nullable
 * properties, so an upsert writes null into them. Anything else nullable on an
 * action is refused with the rule's reason.
 */
import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { TOOL_ROOT } from "../src/profile.ts"

const cli = join(TOOL_ROOT, "src/cli.ts")

/** `fields` declare both the type and the action input; `extra` is on the action input only. */
function compile(fields: string, extra = ""): { status: number | null; out: string } {
  const root = mkdtempSync(join(tmpdir(), "norn-nullable-"))
  try {
    const dir = join(root, "p")
    expect(spawnSync(process.execPath, [cli, "init", dir, "--no-install", "--template", "empty"], { encoding: "utf8" }).status).toBe(0)
    writeFileSync(join(dir, "src", "reading.ts"), `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
const Fields = Schema.Struct({ reading: Schema.String, ${fields} })
const Input = Schema.Struct({ reading: Schema.String, ${fields}${extra ? `, ${extra}` : ""} })
export const Reading = Norn.type({ name: "app.Reading", title: "Reading", version: "0.1.0", properties: Fields, identity: [{ name: "primary", properties: ["reading"] }] })
export const Record = Norn.action({
  id: "app.reading.record", title: "Record a reading", version: "0.1.0", input: Input, output: Schema.Struct({ reading: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Reading, change: "created" }],
  effect: { kind: "upsert_input_object", type: Reading, objectIdField: "reading", writeMode: { kind: "create_only" } },
})
`)
    const run = spawnSync(process.execPath, [cli, "compile", join(dir, "src"), "--org", "local", "--env", "dev"], { cwd: dir, encoding: "utf8" })
    return { status: run.status, out: run.stdout }
  } finally { rmSync(root, { recursive: true }) }
}

type Artifact = { artifactId: string; payload: Record<string, any> }
const artifact = (out: string, id: string) => (JSON.parse(out) as Artifact[]).find((a) => a.artifactId === id)!.payload

test("Schema.NullOr of a plain scalar is a nullable action input and a nullable property", () => {
  const run = compile("note: Schema.NullOr(Schema.String), count: Schema.NullOr(Schema.Int), seen: Schema.NullOr(Schema.Boolean)", "level: Schema.NullOr(Schema.Finite)")
  expect(run.status).toBe(0)
  const input = artifact(run.out, "app.reading.record.action").inputSchema
  expect(input.properties).toEqual({
    reading: { type: "string" },
    note: { type: ["string", "null"] },
    count: { type: ["integer", "null"] },
    seen: { type: ["boolean", "null"] },
    level: { type: ["number", "null"] },
  })
  // Present (possibly null) on every call: absence is a value, not a missing key.
  expect(input.required).toEqual(["reading", "note", "count", "seen", "level"])
  const properties = artifact(run.out, "app.Reading.entity").objectType.properties as Array<{ id: string; constraints?: { nullable?: boolean } }>
  expect(Object.fromEntries(properties.map((p) => [p.id, p.constraints?.nullable ?? false]))).toEqual({ reading: false, note: true, count: true, seen: true })
}, 60_000)

test("a nullable literal set and a nullable decimal carry their enum and nornType beside null", () => {
  const run = compile("kind: Schema.NullOr(Schema.Literals([\"a\", \"b\"])), amount: Schema.NullOr(Norn.decimal(2))")
  expect(run.status).toBe(0)
  const input = artifact(run.out, "app.reading.record.action").inputSchema
  expect(input.properties.kind).toEqual({ type: ["string", "null"], enum: ["a", "b"] })
  expect(input.properties.amount.type).toEqual(["number", "null"])
  expect(input.properties.amount.nornType).toMatchObject({ kind: "decimal", scale: 2 })
  const properties = artifact(run.out, "app.Reading.entity").objectType.properties as Array<{ id: string; constraints?: { nullable?: boolean } }>
  expect(properties.filter((p) => p.constraints?.nullable).map((p) => p.id).sort()).toEqual(["amount", "kind"])
}, 60_000)

test("a nullable struct or array on an action is refused with the reason", () => {
  const run = compile("note: Schema.String", "tags: Schema.NullOr(Schema.Array(Schema.String))")
  expect(run.status).toBe(1)
  expect(run.out).toContain("an action's absent value is a nullable scalar")
}, 60_000)
