/**
 * Public builder → canonical IR → Rust checker → `norn codegen --ts` → `tsc`
 * round-trip inventory (Lane A4).
 *
 * Exercises the authored TypeScript syntax (`Norn.type`, `Norn.action`,
 * `Norn.access`) and the closure-free data builders (`E`, `A`, `P`, `S`) across
 * canonical `derive` (`inputRequired` omitted/true/false, `fromCaller`,
 * multi-step), outbound `commands` (`create`, `update` with `expectedModified`
 * and `childMerges`, `submit`, `cancel`), `uiEffects`, `slice`, `aggregates`,
 * `createLinks`, `independentOf`, and strict TypeScript compilation of the
 * generated `.d.ts`.
 */
import { expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import ts from "typescript"
import { A, E, P, S } from "../src/builder/index.ts"
import { type Json, canonicalJson } from "../src/canonical.ts"
import { generateTypes } from "../src/codegen.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const scope = { orgId: "org_demo", environmentId: "dev" }

const INVENTORY_SOURCE = `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Warehouse = Norn.type({
  name: "inv.Warehouse",
  title: "Warehouse",
  version: "1.0.0",
  properties: Schema.Struct({
    warehouse_id: Schema.String,
    code: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["warehouse_id"] }],
})

export const Shipment = Norn.type({
  name: "inv.Shipment",
  title: "Shipment",
  version: "1.0.0",
  properties: Schema.Struct({
    shipment_id: Schema.String,
    warehouse_id: Schema.String,
    code: Schema.String,
    note: Schema.NullOr(Schema.String),
    amount: Norn.decimal(2),
    created_by: Schema.String,
    status: Schema.Literals(["draft", "dispatched", "cancelled"]),
  }),
  protected: ["created_by"],
  links: [
    {
      id: "warehouse",
      target: Warehouse,
      cardinality: { to: "one", from: "many" },
    },
  ],
  identity: [{ name: "primary", properties: ["shipment_id"] }],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["dispatched", "cancelled"],
      transitions: [
        { id: "dispatch", from: ["draft"], to: "dispatched" },
        { id: "cancel", from: ["draft"], to: "cancelled" },
      ],
    },
  ],
})

const CreateInput = Schema.Struct({
  shipment_id: Schema.String,
  warehouse_id: Schema.String,
  code: Schema.String,
  note: Schema.NullOr(Schema.String),
  amount: Norn.decimal(2),
  created_by: Schema.String,
  status: Schema.Literals(["draft"]),
})

const ShipmentRef = Schema.Struct({
  shipment_id: Schema.String,
})

export const CreateShipment = Norn.action({
  id: "inv.shipment.create",
  title: "Create shipment",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: CreateInput,
  output: ShipmentRef,
  fromCaller: ["created_by"],
  slice: [
    {
      name: "warehouse",
      type: Warehouse,
      key: { property: { from: "input", property: "warehouse_id" } },
    },
  ],
  aggregates: [
    {
      name: "draft_count",
      type: Shipment,
      filter: { status: "draft" },
      aggregate: { kind: "count" },
      maxItems: 100,
    },
  ],
  derive: [
    { field: "code", expression: Norn.unsafeZen("trim(input.code)") },
    { field: "code", expression: Norn.unsafeZen("upper(input.code)"), inputRequired: true },
    { field: "note", expression: Norn.unsafeZen('input.note ?? "standard"'), inputRequired: false },
  ],
  preconditions: [Norn.unsafeZen("aggregates.draft_count < 100")],
  createLinks: [
    { link: "warehouse", targetType: Warehouse, targetIdField: "warehouse_id" },
  ],
  writes: [{ type: Shipment, change: "created" }],
  effect: { kind: "upsert_input_object", type: Shipment, objectIdField: "shipment_id", writeMode: { kind: "create_only" } },
  uiEffects: {
    effectsVersion: "0.1",
    irreversible: false,
    destructive: false,
    money: ["USD"],
    errorCodes: ["SHIPMENT_LIMIT_REACHED"],
    resultCodes: ["SHIPMENT_CREATED"],
  },
})

const DispatchInput = Schema.Struct({
  shipment_id: Schema.String,
  source_name: Schema.String,
  source_modified: Schema.String,
  owner_tag: Schema.String,
  rows: Schema.Array(Schema.String),
})

export const DispatchShipment = Norn.action({
  id: "inv.shipment.dispatch",
  title: "Dispatch shipment",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: false,
  ledgered: true,
  input: DispatchInput,
  output: ShipmentRef,
  writes: [{ type: Shipment, change: "modified" }],
  effect: { kind: "fire_transition", type: Shipment, objectIdField: "shipment_id", lifecycle: "status", transition: "dispatch" },
  independentOf: ["created_by"],
  commands: [
    {
      connector: "erpnext",
      operation: "update",
      doctype: "Delivery Note",
      name: Norn.expression(DispatchInput, (x) => x.source_name),
      expectedModified: Norn.expression(DispatchInput, (x) => x.source_modified),
      fields: {
        status: Norn.unsafeZen('"Submitted"'),
      },
      childMerges: [
        {
          field: "items",
          ownershipField: "norn_owner",
          ownershipValue: Norn.expression(DispatchInput, (x) => x.owner_tag),
          rowKeyField: "item_code",
          rows: Norn.expression(DispatchInput, (x) => x.rows),
        },
      ],
    },
  ],
  uiEffects: {
    irreversible: true,
    errorCodes: ["DISPATCH_CONFLICT"],
  },
})

const CancelInput = Schema.Struct({
  shipment_id: Schema.String,
  source_name: Schema.String,
})

export const CancelShipment = Norn.action({
  id: "inv.shipment.cancel",
  title: "Cancel shipment",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: false,
  ledgered: true,
  input: CancelInput,
  output: ShipmentRef,
  writes: [{ type: Shipment, change: "modified" }],
  effect: { kind: "fire_transition", type: Shipment, objectIdField: "shipment_id", lifecycle: "status", transition: "cancel" },
  commands: [
    {
      connector: "erpnext",
      operation: "cancel",
      doctype: "Delivery Note",
      name: Norn.expression(CancelInput, (x) => x.source_name),
    },
  ],
  uiEffects: {
    destructive: true,
    resultCodes: ["SHIPMENT_CANCELLED"],
  },
})

export const InventoryAccess = Norn.access({
  id: "inv.shipment.access",
  title: "Inventory shipment access",
  version: "1.0.0",
  rules: [
    {
      ruleId: "ops_shipments",
      actions: [CreateShipment, DispatchShipment, CancelShipment],
      resourceType: Shipment,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["Operator"] } }],
    },
  ],
})
`

function buildEquivalentDataArtifacts(sourcePath: string): Json[] {
  const options = { scope, sourcePath }
  const warehouse = E.objectType({
    name: "inv.Warehouse",
    title: "Warehouse",
    version: "1.0.0",
    properties: [
      E.property("warehouse_id", E.text()),
      E.property("code", E.text()),
    ],
    identity: [{ name: "primary", properties: ["warehouse_id"] }],
  })
  const shipment = E.objectType({
    name: "inv.Shipment",
    title: "Shipment",
    version: "1.0.0",
    properties: [
      E.property("shipment_id", E.text()),
      E.property("warehouse_id", E.text()),
      E.property("code", E.text()),
      E.property("note", E.text(), { nullable: true }),
      E.property("amount", E.decimal(2)),
      E.property("created_by", E.text(), { protected: true }),
      E.property("status", E.enumeration("draft", "dispatched", "cancelled")),
    ],
    links: [{ id: "warehouse", target: warehouse.name, cardinality: { to: "one", from: "many" } }],
    identity: [{ name: "primary", properties: ["shipment_id"] }],
    lifecycles: [
      {
        property: "status",
        initial: ["draft"],
        terminal: ["dispatched", "cancelled"],
        transitions: [
          { id: "dispatch", from: ["draft"], to: "dispatched" },
          { id: "cancel", from: ["draft"], to: "cancelled" },
        ],
      },
    ],
  })
  const shipmentRef = S.record({ shipment_id: S.text() })
  const createShipment = A.action({
    id: "inv.shipment.create",
    title: "Create shipment",
    version: "1.0.0",
    scopeRequired: ["orgId", "environmentId"],
    idempotent: true,
    ledgered: true,
    input: S.record({
      shipment_id: S.text(),
      warehouse_id: S.text(),
      code: S.text(),
      note: S.nullable(S.text()),
      amount: { type: "number", nornType: { kind: "decimal", scale: 2 } },
      created_by: S.text(),
      status: S.text(["draft"]),
    }),
    output: shipmentRef,
    slice: [{ name: "warehouse", objectType: warehouse.name, key: { property: { from: "input", property: "warehouse_id" } } }],
    aggregates: [{ name: "draft_count", objectType: shipment.name, filter: { status: "draft" }, aggregate: { kind: "count" }, maxItems: 100 }],
    derive: [
      { field: "created_by", expression: "actor.subjectId", inputRequired: false },
      { field: "code", expression: "trim(input.code)" },
      { field: "code", expression: "upper(input.code)", inputRequired: true },
      { field: "note", expression: 'input.note ?? "standard"', inputRequired: false },
    ],
    preconditions: ["aggregates.draft_count < 100"],
    createLinks: [{ link: "warehouse", targetType: warehouse.name, targetIdField: "warehouse_id" }],
    writes: [{ entityKind: shipment.name, change: "created" }],
    effect: A.upsertInputObject(shipment, "shipment_id", { kind: "create_only" }),
    uiEffects: {
      effectsVersion: "0.1",
      irreversible: false,
      destructive: false,
      money: ["USD"],
      errorCodes: ["SHIPMENT_LIMIT_REACHED"],
      resultCodes: ["SHIPMENT_CREATED"],
    },
  })
  const dispatchShipment = A.action({
    id: "inv.shipment.dispatch",
    title: "Dispatch shipment",
    version: "1.0.0",
    scopeRequired: ["orgId", "environmentId"],
    idempotent: false,
    ledgered: true,
    input: S.record({
      shipment_id: S.text(),
      source_name: S.text(),
      source_modified: S.text(),
      owner_tag: S.text(),
      rows: S.array(S.text()),
    }),
    output: shipmentRef,
    writes: [{ entityKind: shipment.name, change: "modified" }],
    effect: A.fireTransition(shipment, "shipment_id", "status", "dispatch"),
    independentOf: ["created_by"],
    commands: [
      {
        connector: "erpnext",
        operation: "update",
        doctype: "Delivery Note",
        name: "input.source_name",
        expectedModified: "input.source_modified",
        fields: { status: '"Submitted"' },
        childMerges: [
          {
            field: "items",
            ownershipField: "norn_owner",
            ownershipValue: "input.owner_tag",
            rowKeyField: "item_code",
            rows: "input.rows",
          },
        ],
      },
    ],
    uiEffects: {
      effectsVersion: "0.1",
      irreversible: true,
      errorCodes: ["DISPATCH_CONFLICT"],
    },
  })
  const cancelShipment = A.action({
    id: "inv.shipment.cancel",
    title: "Cancel shipment",
    version: "1.0.0",
    scopeRequired: ["orgId", "environmentId"],
    idempotent: false,
    ledgered: true,
    input: S.record({ shipment_id: S.text(), source_name: S.text() }),
    output: shipmentRef,
    writes: [{ entityKind: shipment.name, change: "modified" }],
    effect: A.fireTransition(shipment, "shipment_id", "status", "cancel"),
    commands: [
      {
        connector: "erpnext",
        operation: "cancel",
        doctype: "Delivery Note",
        name: "input.source_name",
        childMerges: [],
      },
    ],
    uiEffects: {
      effectsVersion: "0.1",
      destructive: true,
      resultCodes: ["SHIPMENT_CANCELLED"],
    },
  })
  const access = P.access({
    id: "inv.shipment.access",
    title: "Inventory shipment access",
    version: "1.0.0",
    rules: [
      {
        ruleId: "ops_shipments",
        actions: [createShipment.id, dispatchShipment.id, cancelShipment.id],
        resourceType: shipment.name,
        scope: { dimensions: ["org", "environment"] },
        allOf: [{ role: { anyOf: ["Operator"] } }],
      },
    ],
  })
  const out: Json[] = []
  for (const built of [
    E.lowerEntity(warehouse, options),
    E.lowerEntity(shipment, options),
    A.lowerAction(createShipment, options),
    A.lowerAction(dispatchShipment, options),
    A.lowerAction(cancelShipment, options),
    P.lowerAccess(access, options),
  ]) {
    if (!built.ok) throw new Error(JSON.stringify(built.diagnostics))
    out.push(built.artifact)
  }
  return out
}

test("A4 builder inventory: syntax compiler == data builder == checker admission == codegen --ts == strict tsc", () => {
  const dir = mkdtempSync(join(tmpdir(), "norn-a4-inventory-"))
  try {
    const sourceFile = join(dir, "inventory.source.ts")
    const sourcePath = "tools/norn-ts/test/inventory.source.ts"
    writeFileSync(sourceFile, INVENTORY_SOURCE)

    const compiled = compileDefinitions(sourceFile, { scope, sourcePath })
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics, null, 2))

    // Syntax compilation and closure-free data builders lower to byte-identical canonical IR.
    const handBuilt = buildEquivalentDataArtifacts(sourcePath)
    expect(canonicalJson(compiled.artifacts)).toBe(canonicalJson(handBuilt))

    // Verify canonical omission of default fields on uiEffects, derive, and commands.
    const createPayload = (compiled.artifacts.find((a) => (a as { artifactId: string }).artifactId === "inv.shipment.create.action") as { payload: Record<string, unknown> }).payload
    expect(createPayload["uiEffects"]).toEqual({
      effectsVersion: "0.1",
      money: ["USD"],
      errorCodes: ["SHIPMENT_LIMIT_REACHED"],
      resultCodes: ["SHIPMENT_CREATED"],
    })
    expect(createPayload["derive"]).toEqual([
      { field: "created_by", expression: "actor.subjectId", inputRequired: false },
      { field: "code", expression: "trim(input.code)" },
      { field: "code", expression: "upper(input.code)" },
      { field: "note", expression: 'input.note ?? "standard"', inputRequired: false },
    ])
    const cancelPayload = (compiled.artifacts.find((a) => (a as { artifactId: string }).artifactId === "inv.shipment.cancel.action") as { payload: Record<string, unknown> }).payload
    const cancelCommand = (cancelPayload["commands"] as Array<Record<string, unknown>>)[0]!
    expect(cancelCommand["operation"]).toBe("cancel")
    expect(Object.hasOwn(cancelCommand, "childMerges")).toBe(false)

    const dispatchPayload = (compiled.artifacts.find((a) => (a as { artifactId: string }).artifactId === "inv.shipment.dispatch.action") as { payload: Record<string, unknown> }).payload
    const dispatchCommand = (dispatchPayload["commands"] as Array<Record<string, unknown>>)[0]!
    expect(dispatchCommand["expectedModified"]).toBe("input.source_modified")
    expect(dispatchCommand["childMerges"]).toEqual([
      {
        field: "items",
        ownershipField: "norn_owner",
        ownershipValue: "input.owner_tag",
        rowKeyField: "item_code",
        rows: "input.rows",
      },
    ])

    // Rust checker admission (enforced in CI via NORN_CHECKER_WASM_PKG).
    if (checker) {
      const admitted = checker.checkArtifactSet(compiled.artifacts)
      expect(admitted.diagnostics.diagnostics).toEqual([])
      expect(admitted.admitted).toBe(true)
    }

    // Codegen emits deterministic types-only `.d.ts` and compiles cleanly under strict TypeScript.
    const dts = generateTypes(compiled.artifacts)
    expect(generateTypes([...compiled.artifacts].reverse())).toBe(dts)
    const dtsFile = join(dir, "inventory.d.ts")
    const usageFile = join(dir, "usage.ts")
    writeFileSync(dtsFile, dts)
    writeFileSync(
      usageFile,
      `import type { ActionMap, ObjectMap } from "./inventory.d.ts"

const validCreate: ActionMap["inv.shipment.create"]["input"] = {
  shipment_id: "ship_1",
  warehouse_id: "wh_1",
  code: "ab-1",
  amount: "42.50",
  status: "draft",
}
const validCreateWithNote: ActionMap["inv.shipment.create"]["input"] = {
  ...validCreate,
  note: null,
}
// @ts-expect-error \`code\` has canonical omitted \`inputRequired\` (defaults to true) and must remain required
const missingCode: ActionMap["inv.shipment.create"]["input"] = {
  shipment_id: "ship_1",
  warehouse_id: "wh_1",
  amount: "42.50",
  status: "draft",
}
const spoofedActor: ActionMap["inv.shipment.create"]["input"] = {
  ...validCreate,
  // @ts-expect-error \`created_by\` is actor-derived (\`fromCaller\`) and excluded from caller input
  created_by: "attacker",
}
const numericAmount: ActionMap["inv.shipment.create"]["input"] = {
  ...validCreate,
  // @ts-expect-error exact decimal must be a string, never a JS number
  amount: 42.5,
}
const stored: ObjectMap["inv.Shipment"] = {
  shipment_id: "ship_1",
  warehouse_id: "wh_1",
  code: "AB-1",
  note: null,
  amount: "42.50",
  created_by: "user_1",
  status: "draft",
}
void validCreateWithNote
void missingCode
void spoofedActor
void numericAmount
void stored
`,
    )
    const program = ts.createProgram([usageFile], {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.ESNext,
      moduleResolution: ts.ModuleResolutionKind.Bundler,
      lib: ["lib.esnext.d.ts"],
      types: [],
      strict: true,
      noUncheckedIndexedAccess: true,
      exactOptionalPropertyTypes: true,
      noEmit: true,
      skipLibCheck: true,
      allowImportingTsExtensions: true,
      verbatimModuleSyntax: true,
      erasableSyntaxOnly: true,
    })
    const diagnostics = ts.getPreEmitDiagnostics(program).map((d) => ts.flattenDiagnosticMessageText(d.messageText, "\n"))
    expect(diagnostics).toEqual([])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}, 30_000)

test("A4 syntax compiler rejects non-0.1 uiEffects.effectsVersion", () => {
  const dir = mkdtempSync(join(tmpdir(), "norn-a4-uieffects-"))
  try {
    const badFile = join(dir, "bad.source.ts")
    writeFileSync(
      badFile,
      `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Ref = Schema.Struct({ id: Schema.String })
export const Bad = Norn.action({
  id: "inv.bad.act",
  title: "Bad",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Ref,
  output: Ref,
  uiEffects: { effectsVersion: "0.2" },
})
`,
    )
    const result = compileDefinitions(badFile, { scope, sourcePath: "bad.source.ts" })
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.diagnostics.some((d) => d.code === "NORN-FLOW-TYPE" && d.message.includes('"0.1"'))).toBe(true)
    }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}, 30_000)
