import { expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, relative } from "node:path"
import { derivedMetricId } from "../src/builder/question.ts"
import { CHECKER_PKG_ENV, staleCheckerWarning } from "../src/checker-abi.ts"
import { compileDefinitionFiles, compileDefinitions, compileQuestions, PENDING_SOURCE_SHA256 } from "../src/compiler/definitions.ts"
import { REGISTRY_VALUE_TYPES } from "../src/compiler/value-types.ts"
import { TOOL_ROOT } from "../src/profile.ts"

// The authoring shell matches the Rust IR (Supply Lab RC-1): registry value
// types in typed expressions, timestamp action inputs, linked question paths,
// derived metric ids, questions beside a pack's definitions, source-schema
// pins computed at compile time, and a stale cached checker never trusted.

const REPO = join(TOOL_ROOT, "../..")

function withModules<T>(files: Record<string, string>, run: (dir: string) => T): T {
  const dir = mkdtempSync(join(tmpdir(), "norn-shell-"))
  try {
    for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text)
    return run(dir)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

function ok<T extends { readonly ok: boolean }>(result: T): Extract<T, { ok: true }> {
  if (!result.ok) throw new Error(JSON.stringify((result as { diagnostics?: unknown }).diagnostics))
  return result as Extract<T, { ok: true }>
}

test("the registry value-type table equals the committed core and std value types", () => {
  const committed: Record<string, unknown> = {}
  for (const file of ["core.value_types.entity.json", "std.value_types.entity.json"]) {
    const artifact = JSON.parse(readFileSync(join(REPO, "registry/core/artifacts", file), "utf8"))
    for (const def of artifact.payload.propertyTypes as { name: string; type: { kind: string; format?: string } }[]) {
      if (!["decimal", "integer", "text", "boolean", "timestamp", "date"].includes(def.type.kind)) continue
      const { format: _format, ...type } = def.type
      committed[def.name] = type
    }
  }
  expect(REGISTRY_VALUE_TYPES as Record<string, unknown>).toEqual(committed)
})

const pack = `
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Item = Norn.type({
  name: "shell.Item", title: "Item", version: "0.1.0",
  properties: Schema.Struct({ item_id: Schema.String, qty: Norn.ref("core.quantity"), seen_at: Norn.timestamp("utc") }),
  identity: [{ name: "primary", properties: ["item_id"] }],
})
export const QtyNonNegative = Norn.law("qty_non_negative", {
  on: Item, must: (self) => self.qty >= 0, because: "A quantity is never negative.",
})
const ItemInput = Schema.Struct({ item_id: Schema.String, qty: Norn.ref("core.quantity"), seen_at: Norn.timestamp("utc") })
export const Record = Norn.action({
  id: "shell.item.record", title: "Record an item", version: "0.1.0",
  input: ItemInput, output: Schema.Struct({ item_id: Schema.String }),
  scopeRequired: ["orgId"], idempotent: true, ledgered: true,
  writes: [{ type: Item, change: "created" }],
  effect: { kind: "upsert_input_object", type: Item, objectIdField: "item_id", writeMode: { kind: "create_only" } },
})
export const Recount = Norn.action({
  id: "shell.item.recount", title: "Recount an item", version: "0.1.0",
  input: ItemInput, output: Schema.Struct({ item_id: Schema.String }),
  scopeRequired: ["orgId"], idempotent: true, ledgered: true,
  writes: [{ type: Item, change: "modified" }],
  effect: { kind: "upsert_input_object", type: Item, objectIdField: "item_id", writeMode: { kind: "update_only" } },
})
export const Volume = Norn.question({
  name: "shell.volume", of: Item, measure: Norn.metricId({ kind: "sum", of: Item, property: "qty", time: "seen_at" }),
  where: [], by: [{ property: "seen_at", grain: "week" }],
  asOf: { basis: "effective", from: "2026-07-06T00:00:00Z", to: "2026-10-26T00:00:00Z", freshness: { kind: "live", maxAgeMs: 60000 } },
  population: { kind: "authorized_objects" },
})
`

test("a registry quantity compares numerically, timestamps are wire text, update_only declares a modify", () => {
  withModules({ "pack.ts": pack }, (dir) => {
    const artifacts = ok(compileDefinitions(join(dir, "pack.ts"), { scope: {}, sourcePath: "pack.ts" })).artifacts as any[]
    const byId = new Map(artifacts.map((a) => [a.artifactId, a]))
    expect(byId.get("shell.Item.qty_non_negative.law")).toBeDefined()
    const record = byId.get("shell.item.record.action").payload
    expect(record.inputSchema.properties.qty).toEqual({ type: "number", nornType: { kind: "decimal", scale: 6 } })
    expect(record.inputSchema.properties.seen_at).toEqual({ type: "string" })
    const recount = byId.get("shell.item.recount.action").payload
    expect(recount.effectProgram.writeMode).toEqual({ kind: "update_only" })
    expect(recount.writes).toEqual([{ change: "modified", entityKind: "shell.Item" }])
    // The module's question sits beside its definitions and is not an artifact.
    expect(artifacts.some((a) => a.artifactId.includes("volume"))).toBe(false)
  })
})

test("questions take linked paths and derived metric ids named as the checker derives them", () => {
  expect(derivedMetricId({ kind: "sum", of: "supply.ShipmentReceipt", property: "received_qty", time: { property: "received_at" } }))
    .toBe("analysis.object_737570706c792e536869706d656e7452656365697074.received_qty.by_received_at")
  expect(derivedMetricId({ kind: "as_of", of: "supply.InventorySnapshot", property: "on_hand_qty", time: { property: "as_of" } }))
    .toBe("analysis.object_737570706c792e496e76656e746f7279536e617073686f74.on_hand_qty.as_of_as_of")
  expect(derivedMetricId({ kind: "ratio", of: "supply.OrderLine", property: "fill_rate", time: { property: "promised_at" } }))
    .toBe("analysis.object_737570706c792e4f726465724c696e65.fill_rate.by_promised_at.ratio_of_sums")
  // `norn_checker::metric::field_name`: length-prefixed components, `_` escaped as `_u`.
  expect(derivedMetricId({ kind: "sum", of: "a.B", property: "qty", time: { links: ["receipt"], property: "received_at" } }))
    .toBe("analysis.object_612e42.qty.by_norn_path_7_receipt_11_received_uat_value")
  expect(() => derivedMetricId({ kind: "as_of", of: "a.B", property: "qty", time: { links: ["x"], property: "at" } })).toThrow()
  withModules({ "pack.ts": pack }, (dir) => {
    const [question] = ok(compileQuestions(join(dir, "pack.ts"))).questions as any[]
    expect(question.aggregate).toEqual([{ metric: `analysis.object_${Buffer.from("shell.Item").toString("hex")}.qty.by_seen_at` }])
  })
  const linked = `
import { Norn } from "@norn/authoring"
export const Q = Norn.question({
  name: "shell.linked", of: "shell.Inspection",
  measure: Norn.metricId({ kind: "sum", of: "shell.Inspection", property: "qty", time: { link: "receipt", property: "received_at" } }),
  where: [{ links: ["receipt"], property: "unit", operator: "eq", value: "each" }],
  by: [{ link: "receipt", property: "received_at", grain: "week" }],
  asOf: { basis: "effective", from: "2026-07-06T00:00:00Z", to: "2026-10-26T00:00:00Z", freshness: { kind: "live", maxAgeMs: 60000 } },
  population: { kind: "authorized_objects" },
})`
  withModules({ "q.ts": linked }, (dir) => {
    const [question] = ok(compileQuestions(join(dir, "q.ts"))).questions as any[]
    expect(question.filter).toEqual([{ path: { links: ["receipt"], property: "unit" }, operator: "eq", value: "each" }])
    expect(question.groupBy).toEqual([{ path: { links: ["receipt"], property: "received_at" }, grain: "week" }])
  })
})

test("a mapping bound to a source schema by reference carries the schema's Norn1 hash", () => {
  const sources = join(REPO, "registry/packs/supply-lab/sources.ts")
  // As the registry compiles a pack: its envelopes carry their source paths.
  const results = compileDefinitionFiles([sources], (path) => ({ scope: {}, sourcePath: relative(REPO, path), registryPackagePath: "registry/packs/supply-lab" })).map(ok)
  const artifacts = results.flatMap((result) => result.artifacts) as any[]
  const erp = artifacts.find((a) => a.artifactId === "supply.erp_supplier.mapping")
  // The hash Phase 1 pinned by hand from the Rust checker's refusal, now computed.
  expect(erp.payload.source.sha256).toBe("30ec888a4da1e67bd9b4d690bd3eb18cd9b559ca31622de64017dd8b6e7b4ffb")
  expect(artifacts.some((a) => a.payload?.source?.sha256 === PENDING_SOURCE_SHA256)).toBe(false)
})

test("Norn.law compiles optional-link when-narrowing and typed inverses (LAW-LINK-1 / LAW-LINK-MANY-1)", () => {
  const source = `
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Parent = Norn.type({
  name: "shell.Parent", title: "Parent", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, limit: Norn.decimal(2), closed: Schema.Boolean }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const Child = Norn.type({
  name: "shell.Child", title: "Child", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, amount: Norn.decimal(2) }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "parent", target: Parent, cardinality: { to: "one", from: "many" }, inverse: "children" }],
})

export const ChildWithinParent = Norn.law("within_parent", {
  on: Child,
  must: (self) => self.parent === null || self.amount <= self.parent.limit,
  because: "A linked child stays within its parent's limit.",
})

export const ParentSumWithinLimit = Norn.law("children_within_limit", {
  on: Parent,
  inverses: { children: Child },
  when: (self) => self.children.length > 0,
  must: (self) => Norn.sum(self.children.map((c) => c.amount)) <= self.limit,
  because: "Sum of linked children stays within the parent's limit.",
})
`
  withModules({ "laws.ts": source }, (dir) => {
    const { artifacts } = ok(compileDefinitions(join(dir, "laws.ts"), { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "laws.ts" }))
    const forward = (artifacts as any[]).find((a) => a.artifactId === "shell.Child.within_parent.law")
    expect(forward.payload.response.expression).toBe("(self.parent == null) or (self.amount <= self.parent.limit)")

    const inverse = (artifacts as any[]).find((a) => a.artifactId === "shell.Parent.children_within_limit.law")
    expect(inverse.payload.condition).toBe("len(self.children) > 0")
    expect(inverse.payload.response.expression).toBe("sum(map(self.children as b0, b0.amount)) <= self.limit")
    expect(inverse.dependencies.map((d: any) => d.artifactId)).toEqual(["shell.Parent.entity", "shell.Child.entity"])
  })

  const badInverse = `
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const A = Norn.type({ name: "shell.A", title: "A", version: "0.1.0", properties: Schema.Struct({ id: Schema.String }), identity: [{ name: "primary", properties: ["id"] }] })
export const B = Norn.type({ name: "shell.B", title: "B", version: "0.1.0", properties: Schema.Struct({ id: Schema.String }), identity: [{ name: "primary", properties: ["id"] }] })
export const Bad = Norn.law("bad", { on: A, inverses: { missing: B }, must: (self) => self.missing.length === 0, because: "bad" })
`
  withModules({ "bad.ts": badInverse }, (dir) => {
    const result = compileDefinitions(join(dir, "bad.ts"), { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "bad.ts" })
    expect(result.ok).toBe(false)
  })
})

test("Norn.action compiles quantityEffect reserve and post_reservation to ActionIR", () => {
  const source = `
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const StockOrder = Norn.type({
  name: "shell.StockOrder", title: "Stock order", version: "0.1.0",
  properties: Schema.Struct({
    id: Schema.String,
    status: Schema.Literals(["draft", "reserved", "posted"]),
    qty: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "draft" },
  lifecycles: [{
    property: "status", initial: ["draft"], terminal: ["posted"],
    transitions: [
      { id: "reserve", from: ["draft"], to: "reserved" },
      { id: "post", from: ["reserved"], to: "posted" },
    ],
  }],
})

const ReserveInput = Schema.Struct({
  id: Schema.String,
  reservation_id: Schema.String,
  qty_units: Schema.String,
  balance_versions: Schema.Array(Schema.String),
})

export const ReserveStock = Norn.action({
  id: "shell.stock.reserve", title: "Reserve stock", version: "0.1.0",
  input: ReserveInput, output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: StockOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: StockOrder, objectIdField: "id", lifecycle: "status", transition: "reserve" },
  quantityEffect: {
    ledgerId: "stock_main",
    balanceVersionsField: "balance_versions",
    operation: {
      kind: "reserve",
      reservationIdField: "reservation_id",
      templateId: "transfer_out",
      parameters: { qty: { field: "qty_units", scale: 6 } },
      ttlMs: 60000,
    },
  },
})

const PostInput = Schema.Struct({
  id: Schema.String,
  reservation_id: Schema.String,
  posting_id: Schema.String,
  balance_versions: Schema.Array(Schema.String),
})

export const PostStock = Norn.action({
  id: "shell.stock.post", title: "Post reserved stock", version: "0.1.0",
  input: PostInput, output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: StockOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: StockOrder, objectIdField: "id", lifecycle: "status", transition: "post" },
  quantityEffect: {
    ledgerId: "stock_main",
    balanceVersionsField: "balance_versions",
    operation: {
      kind: "post_reservation",
      reservationIdField: "reservation_id",
      postingIdField: "posting_id",
    },
  },
})
`
  withModules({ "qty.ts": source }, (dir) => {
    const { artifacts } = ok(compileDefinitions(join(dir, "qty.ts"), { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "qty.ts" }))
    const reserve = (artifacts as any[]).find((a) => a.artifactId === "shell.stock.reserve.action")
    expect(reserve.payload.quantityEffect).toEqual({
      ledgerId: "stock_main",
      balanceVersionsField: "balance_versions",
      operation: {
        kind: "reserve",
        reservationIdField: "reservation_id",
        templateId: "transfer_out",
        parameters: { qty: { field: "qty_units", scale: 6 } },
        ttlMs: 60000,
      },
    })
    const post = (artifacts as any[]).find((a) => a.artifactId === "shell.stock.post.action")
    expect(post.payload.quantityEffect).toEqual({
      ledgerId: "stock_main",
      balanceVersionsField: "balance_versions",
      operation: {
        kind: "post_reservation",
        reservationIdField: "reservation_id",
        postingIdField: "posting_id",
      },
    })
  })
})

test("a cached checker a source checkout did not build is reported, never silently trusted", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-stale-"))
  try {
    const cached = join(root, "cache", "runtime", "checker-wasm")
    mkdirSync(cached, { recursive: true })
    writeFileSync(join(cached, "norn_checker_wasm.js"), "")
    writeFileSync(join(cached, "norn_checker_wasm_bg.wasm"), "")
    const crate = join(root, "Cargo.toml")
    writeFileSync(crate, "")
    const env = { NORN_CACHE_DIR: join(root, "cache") }
    expect(staleCheckerWarning(env, join(root, "no-bundled"), crate)).toContain("may predate it")
    expect(staleCheckerWarning({ ...env, [CHECKER_PKG_ENV]: cached }, join(root, "no-bundled"), crate)).toBeUndefined()
    expect(staleCheckerWarning(env, join(root, "no-bundled"), join(root, "not-a-checkout"))).toBeUndefined()
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
