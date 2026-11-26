import { expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"

const SOURCE = `
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Rule = Norn.type({
  name: "hr.SafeDepartureRule",
  title: "Safe departure rule",
  version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, branch_id: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const SetRule = Norn.action({
  id: "hr.safe_departure.set",
  title: "Set rule",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String, branch_id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Rule, change: "created" }],
  effect: { kind: "upsert_input_object", type: Rule, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const Policy = Norn.access({
  id: "hr.access",
  title: "HR access",
  version: "0.1.0",
  scopedRoles: [{ role: "branch_manager", attribute: "managed_branches", property: "branch_id" }],
  rules: [
    {
      ruleId: "bm_rule",
      actions: [SetRule],
      resourceType: Rule,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager"] } }],
    },
  ],
})
`

test("SCOPE-KEY-1: scopedRoles auto-injects resource in principalAttribute onto matching rules", () => {
  const dir = mkdtempSync(join(tmpdir(), "norn-pr-c-"))
  try {
    const file = join(dir, "policy.ts")
    writeFileSync(file, SOURCE)
    const compiled = compileDefinitions(file, { scope: { orgId: "soliman", environmentId: "prod" }, sourcePath: "policy.ts" })
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    const policy = compiled.artifacts.find((a) => (a as { artifactId: string }).artifactId === "hr.access.policy") as {
      payload: { rules: Array<{ ruleId: string; allOf: unknown[] }> }
    }
    expect(policy.payload.rules[0].allOf).toEqual([
      { role: { anyOf: ["branch_manager"] } },
      { resource: { name: "branch_id", op: "in", value: { principalAttribute: "managed_branches" } } },
    ])

    // Mixed scoped + unscoped role rule without explicit scope predicate must fail closed!
    const badFile = join(dir, "bad-mixed.ts")
    writeFileSync(
      badFile,
      SOURCE.replace('anyOf: ["branch_manager"]', 'anyOf: ["branch_manager", "hr_admin"]'),
    )
    const bad = compileDefinitions(badFile, { scope: { orgId: "soliman", environmentId: "prod" }, sourcePath: "bad-mixed.ts" })
    expect(bad.ok).toBe(false)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
