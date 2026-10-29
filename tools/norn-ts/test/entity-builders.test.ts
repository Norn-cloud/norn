/**
 * T7-TS-2 (#2690): `Norn.type` (links, keys, defaults, lifecycles),
 * `Norn.interfaceType`, `Norn.action`, and `Norn.access` lower to canonical
 * EntityIR v2 / ActionIR / AccessIR that the Rust checker admits.
 *
 * Run: `NORN_CHECKER_WASM_PKG=/path/to/pkg bun --cwd tools/norn-ts test entity-builders`.
 * Refresh goldens after an intentional change: `UPDATE_GOLDENS=1 bun --cwd tools/norn-ts test entity-builders`.
 */
import { describe, expect, test } from "bun:test"
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { A, E, M, P, S } from "../src/builder/index.ts"
import { type Json, canonicalJson } from "../src/canonical.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
const UPDATE = process.env.UPDATE_GOLDENS === "1"
const scope = { orgId: "org_demo", environmentId: "dev" }

// Round trips compile two fresh TypeScript programs; cold Boat runs can exceed
// Bun's 5s default even with the shared WASM checker (#3340).
test("action slices and ordered derivations survive syntax compilation and canonical lowering", () => {
  const file = join(TOOL_ROOT, "fixtures/definitions/action-preconditions.source.ts")
  const options = { scope, sourcePath: "tools/norn-ts/fixtures/definitions/action-preconditions.source.ts" }
  const compiled = compileDefinitions(file, options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const action = compiled.artifacts.find((artifact) => (artifact as { kind: string }).kind === "action")
  expect(action).toMatchObject({
    payload: {
      derive: [
        { field: "code", expression: "trim(input.code)" },
        { field: "code", expression: "upper(input.code)" },
        { field: "brand", expression: 'input.brand ?? "NORN"', inputRequired: false },
      ],
      slice: [{ name: "existing", objectType: "fashion.Season", key: { property: { from: "input", property: "code" } } }],
      preconditions: ["slice.existing.brand == input.brand"],
    },
  })
  const again = compileDefinitions(file, options)
  if (!again.ok) throw new Error(JSON.stringify(again.diagnostics))
  expect(canonicalJson(again.artifacts)).toBe(canonicalJson(compiled.artifacts))
  if (checker) {
    const admitted = checker.checkArtifactSet(compiled.artifacts)
    expect(admitted.diagnostics.diagnostics).toEqual([])
    expect(admitted.admitted).toBe(true)
  }
}, 30_000)

describe("Norn.type / action / access goldens", () => {
  const dir = join(TOOL_ROOT, "fixtures/definitions")
  const options = { scope, sourcePath: "tools/norn-ts/fixtures/definitions/crm.source.ts" }
  const compile = () => {
    const result = compileDefinitions(join(dir, "crm.source.ts"), options)
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics, null, 2))
    return result
  }

  test("crm: golden IR (types with links, actions, access), byte-stable, admitted by the Rust checker", () => {
    const result = compile()
    const json = canonicalJson(result.artifacts)
    const golden = join(dir, "crm.ir.json")
    if (UPDATE) writeFileSync(golden, json)
    expect(json).toBe(readFileSync(golden, "utf8"))
    expect(canonicalJson(compile().artifacts)).toBe(json)
    if (checker) {
      const admitted = checker.checkArtifactSet(result.artifacts)
      expect(admitted.diagnostics.diagnostics).toEqual([])
      expect(admitted.admitted).toBe(true)
    }
  }, 30_000)

  test("the data builder writes the same artifacts by hand", () => {
    const account = E.objectType({
      name: "crm.Account",
      title: "Account",
      version: "0.1.0",
      properties: [E.property("account_id", E.text())],
      identity: [{ name: "primary", properties: ["account_id"] }],
    })
    const contact = E.objectType({
      name: "crm.Contact",
      title: "Contact",
      version: "0.1.0",
      description: "A person at an account.",
      properties: [
        E.property("contact_id", E.text()),
        E.property("email", E.text(), { nullable: true }),
        E.property("status", E.enumeration("active", "archived"), { default: "active" }),
      ],
      links: [{ id: "account", description: "The account the contact works for.", target: account.name, cardinality: { to: "one", from: "many" }, inverse: "contacts" }],
      identity: [{ name: "primary", properties: ["contact_id"] }],
      lifecycles: [
        {
          property: "status",
          initial: ["active"],
          transitions: [
            { id: "archive", from: ["active"], to: "archived" },
            { id: "restore", from: ["archived"], to: "active" },
          ],
        },
      ],
    })
    const ref = S.record({ contact_id: S.text() })
    const common = { version: "0.1.0", scopeRequired: ["orgId", "environmentId"], ledgered: true, input: ref, output: ref }
    const upsert = A.action({
      ...common,
      id: "crm.contact.upsert",
      title: "Upsert contact",
      idempotent: true,
      input: S.record({ contact_id: S.text(), email: S.text() }, ["email"]),
      writes: [
        { entityKind: contact.name, change: "created" },
        { entityKind: contact.name, change: "modified" },
      ],
      effect: A.upsertInputObject(contact, "contact_id"),
    })
    const restore = A.action({
      ...common,
      id: "crm.contact.restore",
      title: "Restore contact",
      idempotent: false,
      writes: [{ entityKind: contact.name, change: "modified" }],
      effect: A.fireTransition(contact, "contact_id", "status", "restore"),
    })
    const archive = A.action({
      ...common,
      id: "crm.contact.archive",
      title: "Archive contact",
      description: "Archive a contact; restoring it compensates.",
      idempotent: false,
      ledgerCategory: "crm.contact",
      preconditions: ["len(contact_id) > 0"],
      writes: [{ entityKind: contact.name, change: "modified" }],
      effect: A.fireTransition(contact, "contact_id", "status", "archive"),
      compensation: restore.id,
    })
    const tenant = { dimensions: ["org", "environment"] } as const
    const policy = P.access({
      id: "crm.contact.access",
      title: "Contact access",
      version: "0.1.0",
      rules: [
        { ruleId: "staff_manage_contacts", actions: [upsert.id, archive.id, restore.id], resourceType: contact.name, scope: tenant, allOf: [{ role: { anyOf: ["Staff"] } }] },
        {
          ruleId: "owners_read_contacts",
          actions: ["norn.read"],
          resourceType: contact.name,
          scope: tenant,
          allOf: [{ role: { anyOf: ["Staff"] } }, { rel: { from: "resource", role: "account" } }],
        },
      ],
    })
    const artifacts: Json[] = []
    for (const built of [
      E.lowerEntity(account, options),
      E.lowerEntity(contact, options),
      A.lowerAction(upsert, options),
      A.lowerAction(restore, options),
      A.lowerAction(archive, options),
      P.lowerAccess(policy, options),
    ]) {
      if (!built.ok) throw new Error(JSON.stringify(built.diagnostics))
      artifacts.push(built.artifact)
    }
    expect(canonicalJson(artifacts)).toBe(canonicalJson(compile().artifacts))
  })

  test("the syntax compiler preserves principal attribute resource operands", () => {
    const result = compileDefinitions(
      join(dir, "access-principal-attribute.source.ts"),
      options,
    )
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics, null, 2))
    const isJsonObject = (
      value: Json | undefined,
    ): value is { readonly [key: string]: Json } =>
      value !== undefined &&
      value !== null &&
      typeof value === "object" &&
      !Array.isArray(value)
    const policy = result.artifacts.find(
      (artifact) => isJsonObject(artifact) && artifact.kind === "policy",
    )
    if (!isJsonObject(policy)) throw new Error("access policy artifact is missing")
    const payload = policy.payload
    if (!isJsonObject(payload)) throw new Error("access policy payload is malformed")
    const rules = payload.rules
    if (!Array.isArray(rules) || !isJsonObject(rules[0])) {
      throw new Error("access policy rules are missing or malformed")
    }
    expect(rules[0].allOf).toEqual([
      {
        resource: {
          name: "business_unit",
          op: "in",
          value: { principalAttribute: "business_units" },
        },
      },
    ])
  })
})

test("interface invariants lower from authoring syntax and empty lists stay omitted", () => {
  const dir = join(TOOL_ROOT, "fixtures/definitions")
  const file = join(dir, "interface-invariant.source.ts")
  const result = compileDefinitions(file, {
    scope,
    sourcePath: "tools/norn-ts/fixtures/definitions/interface-invariant.source.ts",
  })
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics, null, 2))
  const interfacePayload = (artifact: Json) =>
    (artifact as unknown as { payload: { interface: { invariants?: unknown } } }).payload.interface

  expect(interfacePayload(result.artifacts[0]!)).toMatchObject({
    invariants: [{
      id: "non_negative_total",
      classification: "quality",
      expression: "self.total >= 0",
      message: "total must be non-negative",
    }],
  })
  expect(interfacePayload(result.artifacts[1]!)).toMatchObject({
    invariants: [
      {
        id: "name_present_default_integrity",
        expression: 'self.name != ""',
        message: "name must not be empty",
      },
      {
        id: "name_present_explicit_integrity",
        expression: 'self.name != ""',
        message: "name must not be empty",
      },
    ],
  })
  const legacyInvariants = (
    interfacePayload(result.artifacts[1]!) as { invariants: ReadonlyArray<Record<string, unknown>> }
  ).invariants
  for (const invariant of legacyInvariants) {
    expect(invariant).not.toHaveProperty("classification")
  }
  expect(interfacePayload(result.artifacts[2]!)).not.toHaveProperty("invariants")
  if (checker) {
    const admitted = checker.checkArtifactSet(result.artifacts)
    expect(admitted.diagnostics.diagnostics).toEqual([])
    expect(admitted.admitted).toBe(true)
  }
})

test("the direct entity builder omits default and explicit integrity classifications", () => {
  const built = E.lowerEntity(E.interfaceType({
    name: "commerce.ConstraintBearing",
    title: "Constraint bearing",
    version: "0.1.0",
    properties: [E.property("name", E.text())],
    invariants: [
      {
        id: "quality",
        classification: "quality",
        expression: 'self.name != ""',
        message: "name must not be empty",
      },
      {
        id: "default_integrity",
        expression: 'self.name != ""',
        message: "name must not be empty",
      },
      {
        id: "explicit_integrity",
        classification: "integrity",
        expression: 'self.name != ""',
        message: "name must not be empty",
      },
    ],
  }), {
    scope,
    sourcePath: "tools/norn-ts/test/entity-builders.test.ts",
  })
  if (!built.ok) throw new Error(JSON.stringify(built.diagnostics))
  const payload = built.artifact as unknown as {
    payload: { interface: { invariants: ReadonlyArray<Record<string, unknown>> } }
  }
  expect(payload.payload.interface.invariants).toEqual([
    {
      id: "quality",
      classification: "quality",
      expression: 'self.name != ""',
      message: "name must not be empty",
    },
    {
      id: "default_integrity",
      expression: 'self.name != ""',
      message: "name must not be empty",
    },
    {
      id: "explicit_integrity",
      expression: 'self.name != ""',
      message: "name must not be empty",
    },
  ])
})

test("ActionIR upsert write modes lower to canonical mode declarations", () => {
  const type = { name: "crm.Contact", version: "0.1.0" }
  const input = S.record({ contact_id: S.text() })
  const action = A.action({
    id: "crm.contact.create",
    title: "Create contact",
    version: "0.1.0",
    scopeRequired: ["orgId"],
    idempotent: true,
    ledgered: true,
    input,
    output: input,
    writes: [{ entityKind: type.name, change: "created" }],
    effect: A.upsertInputObject(type, "contact_id", { kind: "create_only" }),
  })
  const lowered = A.lowerAction(action, { scope, sourcePath: "test/action.ts" })
  expect(lowered.ok).toBe(true)
  if (lowered.ok) {
    expect(lowered.artifact).toMatchObject({
      payload: {
        effectProgram: {
          kind: "upsert_input_object",
          writeMode: { kind: "create_only" },
        },
      },
    })
  }
})

describe("definition reference errors", () => {
  test("a mapping pinning one artifact at two versions is refused, not deduplicated", () => {
    const pin = (version: string) => ({ artifactId: "commerce.Order.interface", version })
    const def = M.mapping({
      id: "erpnext.sales_order",
      title: "sales_order",
      version: "0.1.0",
      source: { schemaId: "erpnext.source_schema", version: "0.1.0", sha256: "0".repeat(64) },
      interfaces: [pin("0.1.0")],
      maps: [
        {
          id: "sales_order",
          source: { dataset: "Sales Order" },
          target: { entity: { artifactId: "commerce.SalesOrder.entity", version: "0.1.0" }, interface: pin("0.2.0") },
          subject: "input.name",
          sourceRecord: "input.name",
        },
      ],
    })
    const built = M.lowerMapping(def, { scope, sourcePath: "x" })
    expect(built.ok).toBe(false)
    if (!built.ok) expect(built.diagnostics.map((d) => d.code)).toEqual(["NORN-FLOW-LITERAL"])
  })

  test("a literal __proto__ member is refused, not silently dropped (NORN-FLOW-LITERAL)", () => {
    const result = compileDefinitions(join(TOOL_ROOT, "fixtures/definitions/invalid/proto-condition.ts"), { scope, sourcePath: "x" })
    expect(result.ok).toBe(false)
    if (!result.ok) expect(result.diagnostics.map((d) => d.code)).toContain("NORN-FLOW-LITERAL")
  })

  test("a non-interface in implements is NORN-FLOW-VALUE", () => {
    const result = compileDefinitions(join(TOOL_ROOT, "fixtures/definitions/invalid/implements-type.ts"), { scope, sourcePath: "x" })
    expect(result.ok).toBe(false)
    if (!result.ok) expect(result.diagnostics.map((d) => d.code)).toContain("NORN-FLOW-VALUE")
  })
})

test("an explicit empty constraint classification is refused", () => {
  const result = compileDefinitions(join(TOOL_ROOT, "fixtures/definitions/invalid/invariant-classification.ts"), {
    scope, sourcePath: "invalid/invariant-classification.ts",
  })
  expect(result.ok).toBe(false)
  if (!result.ok) expect(result.diagnostics.map((d) => d.code)).toContain("NORN-FLOW-TYPE")
})

test("acyclic authoring compiles and extracts canonical IR with false omitted", () => {
  const options = { scope, sourcePath: "tools/norn-ts/fixtures/definitions/acyclic.source.ts" }
  const compiled = compileDefinitions(join(TOOL_ROOT, "fixtures/definitions/acyclic.source.ts"), options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const ir = JSON.parse(canonicalJson(compiled.artifacts))
  expect(ir[0].payload.objectType.links[0].acyclic).toBe(true)
  expect(ir[0].payload.objectType.links[1]).not.toHaveProperty("acyclic")
  const built = E.lowerEntity(E.objectType({
    name: "tree.Node", title: "Node", version: "0.1.0",
    properties: [E.property("name", E.text())],
    links: [
      { id: "parent", target: "tree.Node", cardinality: { to: "one", from: "many" }, acyclic: true },
      { id: "related", target: "tree.Node", cardinality: { to: "many", from: "many" }, acyclic: false },
    ],
  }), options)
  if (!built.ok) throw new Error(JSON.stringify(built.diagnostics))
  expect(canonicalJson(compiled.artifacts)).toBe(canonicalJson([built.artifact]))
  if (checker) expect(checker.checkArtifactSet(compiled.artifacts).admitted).toBe(true)
})
