import { expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"

const SOURCE = `
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const PayoutProps = Schema.Struct({
  id: Schema.String,
  payslip_id: Schema.String,
  generation: Schema.Int,
  stamped_at: Norn.timestamp("utc"),
  last_evidence_digest: Schema.String,
  state: Schema.Literals(["requested", "failed"]),
  created_by: Schema.String,
})

export const Payout = Norn.type({
  name: "test.Payout",
  title: "Payout",
  version: "0.1.0",
  properties: PayoutProps,
  protected: ["created_by"],
  origins: {
    created_by: "actor",
    stamped_at: "clock",
    generation: "derived",
    last_evidence_digest: "receipt",
  },
  immutableAfterCreate: ["payslip_id"],
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: { generation: 1, last_evidence_digest: "", state: "requested" },
  lifecycles: [{
    property: "state",
    initial: ["requested"],
    transitions: [
      { id: "fail", from: ["requested"], to: "failed" },
      { id: "retry", from: ["failed"], to: "requested" },
    ],
  }],
})

const CreateInput = Schema.Struct({
  id: Schema.String,
  payslip_id: Schema.String,
  generation: Schema.Int,
  stamped_at: Norn.timestamp("utc"),
  created_by: Schema.String,
})

export const CreatePayout = Norn.action({
  id: "test.payout.create",
  title: "Create payout",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: CreateInput,
  output: Schema.Struct({ id: Schema.String }),
  fromCaller: ["created_by"],
  derive: [
    { field: "stamped_at", expression: Norn.commitTime(), inputRequired: false },
    { field: "generation", expression: Norn.expression(Schema.Struct({ id: Schema.String }), () => 1), inputRequired: false },
  ],
  writes: [{ type: Payout, change: "created" }],
  effect: { kind: "upsert_input_object", type: Payout, objectIdField: "id", writeMode: { kind: "create_only" } },
})

const RetryInput = Schema.Struct({
  id: Schema.String,
  next_generation: Schema.Int,
})

export const RetryPayout = Norn.action({
  id: "test.payout.retry",
  title: "Retry failed payout",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: RetryInput,
  output: Schema.Struct({ id: Schema.String }),
  slice: [{ name: "instruction", type: Payout, key: { property: { from: "input", property: "id" } } }],
  derive: [
    {
      field: "next_generation",
      expression: Norn.expression(
        Schema.Struct({ input: RetryInput, slice: Schema.Struct({ instruction: Schema.Struct({ generation: Schema.Int }) }) }),
        (ctx) => ctx.slice.instruction.generation + 1,
        { root: "context" },
      ),
      inputRequired: false,
    },
  ],
  writes: [{ type: Payout, change: "modified" }],
  effect: {
    kind: "fire_transition",
    type: Payout,
    objectIdField: "id",
    lifecycle: "state",
    transition: "retry",
    setFields: { generation: "next_generation" },
  },
})
`

test("PR-A authoring lowers origins, immutableAfterCreate, Norn.commitTime(), post-slice derive, and fire_transition setFields", () => {
  const dir = mkdtempSync(join(tmpdir(), "norn-pr-a-"))
  try {
    const file = join(dir, "payout.ts")
    writeFileSync(file, SOURCE)
    const compiled = compileDefinitions(file, { scope: { orgId: "org_test", environmentId: "dev" }, sourcePath: "payout.ts" })
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    const entity = compiled.artifacts.find((a) => (a as { artifactId: string }).artifactId === "test.Payout.entity") as {
      payload: { objectType: { properties: Array<Record<string, unknown>> } }
    }
    const byId = Object.fromEntries(entity.payload.objectType.properties.map((p) => [p.id as string, p]))
    expect(byId.created_by?.origin).toBe("actor")
    expect(byId.stamped_at?.origin).toBe("clock")
    expect(byId.generation?.origin).toBe("derived")
    expect(byId.last_evidence_digest?.origin).toBe("receipt")
    expect(byId.payslip_id?.immutableAfterCreate).toBe(true)

    const create = compiled.artifacts.find((a) => (a as { artifactId: string }).artifactId === "test.payout.create.action") as {
      payload: { derive: Array<{ field: string; expression: string; inputRequired?: boolean }> }
    }
    expect(create.payload.derive).toEqual([
      { field: "created_by", expression: "actor.subjectId", inputRequired: false },
      { field: "stamped_at", expression: "clock.nowIso", inputRequired: false },
      { field: "generation", expression: "1", inputRequired: false },
    ])

    const retry = compiled.artifacts.find((a) => (a as { artifactId: string }).artifactId === "test.payout.retry.action") as {
      payload: {
        derive: Array<{ field: string; expression: string; inputRequired?: boolean }>
        effectProgram: { kind: string; setFields?: Record<string, string> }
      }
    }
    expect(retry.payload.derive).toEqual([
      { field: "next_generation", expression: "slice.instruction.generation + 1", inputRequired: false },
    ])
    expect(retry.payload.effectProgram.setFields).toEqual({ generation: "next_generation" })
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
