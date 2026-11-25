// PRINCIPAL-INPUT-1 (#1091): typed authoring of a decide rule's declared principal
// inputs and of the principal attribute catalog, admitted through @norn/checker-wasm.
import { describe, expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"
if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const scope = { orgId: "org_demo", environmentId: "dev" }

function compileSnippet(source: string) {
  const dir = mkdtempSync(join(tmpdir(), "norn-principal-inputs-"))
  try {
    const file = join(dir, "defs.ts")
    writeFileSync(file, source)
    return compileDefinitions(file, { scope, sourcePath: "defs.ts" })
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

// An approval decision over a synthetic expense: the principal record carries
// `columns` and the rule declares `inputs`; with neither, nothing about the
// principal is declared or read.
const definitions = (columns: string, inputs: string) => `
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Expense = Norn.type({
  name: "expense.Expense", title: "Expense", version: "1.0.0",
  properties: Schema.Struct({ expense_id: Schema.String, approver_confirmed: Schema.Boolean }),
  defaults: { approver_confirmed: false },
  identity: [{ name: "primary", properties: ["expense_id"] }],
})

export const ExpenseAccess = Norn.access({
  id: "expense.access", title: "Expense access", version: "1.0.0",
  rules: [],${inputs === "" ? "" : `
  principalCatalog: [
    { name: "approver", type: "bool", source: { kind: "role" } },
    { name: "finance", type: "stringSet", source: { kind: "roleProfile", roles: ["approver", "auditor"] } },
  ],`}
})

export const ApprovalDecision = Norn.decision({
  id: "expense.approval.decide", title: "Approval decision", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],${inputs === "" ? "" : `
  principalInputs: [${inputs}],`}
  input: Schema.Struct({ slice: Schema.Struct({}), event: Schema.Struct({ expense_id: Schema.String })${columns === "" ? "" : `, principal: Schema.Struct({ ${columns} })`} }),
  output: Schema.Struct({ facts: Schema.Array(Schema.Struct({ op: Schema.Literal("set"), objectType: Schema.Literal("expense.Expense"), key: Schema.String, property: Schema.Literal("approver_confirmed"), value: Schema.Boolean })), commands: Schema.Array(Schema.Struct({})), timers: Schema.Array(Schema.Struct({})) }),
}, input => ({ facts: [{ op: "set", objectType: "expense.Expense", key: input.event.expense_id, property: "approver_confirmed", value: ${columns === "" ? "true" : "input.principal.isApprover"} }], commands: [], timers: [] }))
`

const COLUMNS = "isApprover: Schema.Boolean, financeRoles: Schema.Array(Schema.String)"
const INPUTS = `{ name: "isApprover", kind: { holdsRole: { role: "approver", at: "resource" } } }, { name: "financeRoles", kind: { roleProfile: { profile: "finance" } } }`

const payloadOf = (artifacts: ReadonlyArray<unknown>, kind: string) =>
  (artifacts.find((artifact) => (artifact as { kind: string }).kind === kind) as { payload: Record<string, unknown> }).payload

describe("principalInputs on Norn.decision and principalCatalog on Norn.access", () => {
  test("compile to canonical RuleIR and AccessIR and are admitted", () => {
    const compiled = compileSnippet(definitions(COLUMNS, INPUTS))
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    const rule = payloadOf(compiled.artifacts, "rule")
    expect(rule.principalInputs).toEqual([
      { name: "isApprover", kind: { holdsRole: { role: "approver", at: "resource" } } },
      { name: "financeRoles", kind: { roleProfile: { profile: "finance" } } },
    ])
    const principal = (rule.inputSchema as { properties: { principal: { properties: Record<string, unknown>; required: string[] } } }).properties.principal
    expect(principal.properties).toEqual({ isApprover: { type: "boolean" }, financeRoles: { type: "array", items: { type: "string" } } })
    expect([...principal.required].sort()).toEqual(["financeRoles", "isApprover"])
    // The callback's `input.principal.isApprover` is the reserved column, read by name.
    expect(JSON.stringify(rule.graph)).toContain("principal.isApprover")
    expect(payloadOf(compiled.artifacts, "policy").principalCatalog).toEqual([
      { name: "approver", type: "bool", source: { kind: "role" } },
      { name: "finance", type: "stringSet", source: { kind: "roleProfile", roles: ["approver", "auditor"] } },
    ])
    if (checker) {
      const checked = checker.checkArtifactSet(compiled.artifacts)
      expect(checked.diagnostics.diagnostics).toEqual([])
      expect(checked.admitted).toBe(true)
    }
  })

  test("a rule without principal inputs emits neither member", () => {
    const compiled = compileSnippet(definitions("", ""))
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    expect("principalInputs" in payloadOf(compiled.artifacts, "rule")).toBe(false)
    expect("principalCatalog" in payloadOf(compiled.artifacts, "policy")).toBe(false)
  })

  test("the checker refuses an undeclared read, an unknown role and a column of another type", () => {
    if (!checker) return
    const refusals: ReadonlyArray<readonly [string, string, string]> = [
      // The column exists but the rule does not declare it.
      [COLUMNS, `{ name: "financeRoles", kind: { roleProfile: { profile: "finance" } } }`, "rule.validation.principal_input_undeclared"],
      [COLUMNS, INPUTS.replace(`role: "approver"`, `role: "treasurer"`), "artifact_set.decision.principal_input_unknown"],
      [COLUMNS.replace("Schema.Array(Schema.String)", "Schema.String"), INPUTS, "artifact_set.decision.principal_input_type"],
    ]
    for (const [columns, inputs, code] of refusals) {
      const compiled = compileSnippet(definitions(columns, inputs))
      if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
      const checked = checker.checkArtifactSet(compiled.artifacts)
      expect(checked.admitted).toBe(false)
      expect(checked.diagnostics.diagnostics.map((diagnostic) => diagnostic.code)).toContain(code)
    }
  })
})
