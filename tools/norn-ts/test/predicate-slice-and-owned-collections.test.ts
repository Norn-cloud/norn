// Issue #1586: typed authoring for SliceKey::Predicate, ActionMeta.aggregates,
// TypeMeta.ownedCollections, and ActionEffectSpec mutate_parent_with_rows,
// admitted end-to-end through @norn/checker-wasm.
import { describe, expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"
if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const scope = { orgId: "org_demo", environmentId: "dev" }

function compileSnippet(source: string) {
  const dir = mkdtempSync(join(tmpdir(), "norn-pred-owned-"))
  try {
    const file = join(dir, "defs.ts")
    writeFileSync(file, source)
    return compileDefinitions(file, { scope, sourcePath: "defs.ts" })
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

describe("SliceKey::Predicate, aggregates, and ownedCollections / mutate_parent_with_rows", () => {
  test("compiles predicate slices and aggregates to canonical ActionIR and admits via norn-checker", () => {
    const compiled = compileSnippet(`
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Commitment = Norn.type({
  name: "schedule.Commitment",
  title: "Commitment",
  version: "1.0.0",
  properties: Schema.Struct({
    commitment_id: Schema.String,
    agent_id: Schema.String,
    starts_at: Norn.timestamp("utc"),
    ends_at: Norn.timestamp("utc"),
    hours: Norn.decimal(2, { additivity: { kind: "additive" } }),
  }),
  identity: [{ name: "primary", properties: ["commitment_id"] }],
})

const CreateInput = Schema.Struct({
  commitment_id: Schema.String,
  agent_id: Schema.String,
  window_start: Schema.String,
  window_end: Schema.String,
  starts_at: Schema.String,
  ends_at: Schema.String,
  hours: Norn.decimal(2),
})

const PreconditionCtx = Schema.Struct({
  input: CreateInput,
  slice: Schema.Struct({
    existing: Schema.Array(
      Schema.Struct({
        commitment_id: Schema.String,
        agent_id: Schema.String,
        starts_at: Schema.String,
        ends_at: Schema.String,
      }),
    ),
  }),
  aggregates: Schema.Struct({
    total_hours: Norn.decimal(2),
    slot_count: Schema.Int,
  }),
})

export const ScheduleCommitment = Norn.action({
  id: "schedule.commitment.create",
  title: "Schedule Commitment",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: CreateInput,
  output: Schema.Struct({}),
  slice: [
    {
      name: "existing",
      type: Commitment,
      key: {
        predicate: {
          filter: { agent_id: "agent_id" },
          window: {
            property: "starts_at",
            start: "window_start",
            end: "window_end",
            startInclusive: true,
            endInclusive: false,
          },
          maxItems: 50,
        },
      },
    },
  ],
  aggregates: [
    {
      name: "total_hours",
      type: Commitment,
      filter: { agent_id: "agent_id" },
      window: {
        property: "starts_at",
        start: "window_start",
        end: "window_end",
        startInclusive: true,
        endInclusive: false,
      },
      aggregate: { kind: "sum", property: "hours" },
      maxItems: 50,
    },
    {
      name: "slot_count",
      type: Commitment,
      filter: { agent_id: "agent_id" },
      aggregate: { kind: "count" },
      maxItems: 50,
    },
  ],
  preconditions: [
    Norn.expression(PreconditionCtx, (ctx) => ctx.aggregates.slot_count < 20, { root: "context" }),
  ],
  writes: [{ type: Commitment, change: "created" }],
  effect: {
    kind: "upsert_input_object",
    type: Commitment,
    objectIdField: "commitment_id",
    writeMode: { kind: "create_only" },
  },
})
`)
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    const actionArtifact = compiled.artifacts.find((a) => (a as { kind: string }).kind === "action") as {
      payload: Record<string, unknown>
    }
    expect(actionArtifact.payload.slice).toEqual([
      {
        name: "existing",
        objectType: "schedule.Commitment",
        key: {
          predicate: {
            filter: { agent_id: "agent_id" },
            window: {
              property: "starts_at",
              start: "window_start",
              end: "window_end",
              startInclusive: true,
              endInclusive: false,
            },
            maxItems: 50,
          },
        },
      },
    ])
    expect(actionArtifact.payload.aggregates).toEqual([
      {
        name: "total_hours",
        objectType: "schedule.Commitment",
        filter: { agent_id: "agent_id" },
        window: {
          property: "starts_at",
          start: "window_start",
          end: "window_end",
          startInclusive: true,
          endInclusive: false,
        },
        aggregate: { kind: "sum", property: "hours" },
        maxItems: 50,
      },
      {
        name: "slot_count",
        objectType: "schedule.Commitment",
        filter: { agent_id: "agent_id" },
        aggregate: { kind: "count" },
        maxItems: 50,
      },
    ])
    if (checker) {
      const checked = checker.checkArtifactSet(compiled.artifacts)
      expect(checked.diagnostics.diagnostics).toEqual([])
      expect(checked.admitted).toBe(true)
    }
  }, 30_000)

  test("compiles ownedCollections and mutate_parent_with_rows to canonical IR and admits via norn-checker", () => {
    const compiled = compileSnippet(`
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const LineProps = Schema.Struct({
  line_id: Schema.String,
  seq: Schema.Int,
  qty: Norn.decimal(2),
  unit_price: Norn.decimal(2),
  line_total: Norn.decimal(2),
})

export const OrderLine = Norn.type({
  name: "commerce.OrderLine",
  title: "Order Line",
  version: "1.0.0",
  properties: LineProps,
  identity: [{ name: "primary", properties: ["line_id"] }],
})

export const SalesOrder = Norn.type({
  name: "commerce.SalesOrder",
  title: "Sales Order",
  version: "1.0.0",
  properties: Schema.Struct({
    order_id: Schema.String,
    note: Schema.String,
  }),
  links: [
    {
      id: "lines",
      target: OrderLine,
      cardinality: { to: "many", from: "one" },
    },
  ],
  ownedCollections: [
    {
      id: "lines",
      childType: OrderLine,
      rowIdProperty: "line_id",
      orderingKey: "seq",
      cascadeOnParentDelete: true,
      maxRows: 50,
    },
  ],
  identity: [{ name: "primary", properties: ["order_id"] }],
})

const MutateInput = Schema.Struct({
  order_id: Schema.String,
  parent_version: Schema.String,
  parent_values: Schema.Struct({
    note: Schema.optionalKey(Schema.String),
  }),
  row_ops: Schema.Array(Schema.Struct({})),
})

export const SaveOrderWithLines = Norn.action({
  id: "commerce.sales_order.save_lines",
  title: "Save Sales Order With Lines",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: MutateInput,
  output: Schema.Struct({}),
  writes: [
    { type: SalesOrder, change: "created" },
    { type: SalesOrder, change: "modified" },
    { type: OrderLine, change: "created" },
    { type: OrderLine, change: "modified" },
    { type: OrderLine, change: "deleted" },
  ],
  effect: {
    kind: "mutate_parent_with_rows",
    type: SalesOrder,
    objectIdField: "order_id",
    parentValuesField: "parent_values",
    rowOpsField: "row_ops",
    parentVersionField: "parent_version",
    collections: [
      {
        id: "lines",
        preconditions: [Norn.expression(LineProps, (self) => self.qty > 0)],
        derive: {
          line_total: Norn.expression(LineProps, (self) => self.qty * self.unit_price),
        },
      },
    ],
  },
})
`)
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    const orderEntity = compiled.artifacts.find(
      (a) => (a as { artifactId: string }).artifactId === "commerce.SalesOrder.entity",
    ) as { payload: { objectType: { ownedCollections: unknown } } }
    expect(orderEntity.payload.objectType.ownedCollections).toEqual([
      {
        id: "lines",
        childType: "commerce.OrderLine",
        rowIdProperty: "line_id",
        orderingKey: "seq",
        cascadeOnParentDelete: true,
        maxRows: 50,
      },
    ])
    const actionArtifact = compiled.artifacts.find((a) => (a as { kind: string }).kind === "action") as {
      dependencies: ReadonlyArray<{ artifactId: string; version: string; kind: string; required: boolean }>
      payload: {
        inputSchema: { properties: { row_ops: unknown } }
        effectProgram: unknown
      }
    }
    expect(actionArtifact.dependencies).toContainEqual({
      artifactId: "commerce.OrderLine.entity",
      version: "1.0.0",
      kind: "entity",
      required: true,
    })
    expect(actionArtifact.payload.inputSchema.properties.row_ops).toEqual({
      type: "array",
      items: { type: ["null", "boolean", "number", "string", "array", "object"] },
    })
    expect(actionArtifact.payload.effectProgram).toEqual({
      kind: "mutate_parent_with_rows",
      objectIdField: "order_id",
      objectKind: "commerce.SalesOrder",
      entityArtifactId: "commerce.SalesOrder.entity",
      entityArtifactVersion: "1.0.0",
      parentValuesField: "parent_values",
      rowOpsField: "row_ops",
      parentVersionField: "parent_version",
      collections: [
        {
          id: "lines",
          preconditions: ["self.qty > 0"],
          derive: { line_total: "self.qty * self.unit_price" },
        },
      ],
    })
    if (checker) {
      const checked = checker.checkArtifactSet(compiled.artifacts)
      expect(checked.diagnostics.diagnostics).toEqual([])
      expect(checked.admitted).toBe(true)
    }
  }, 30_000)

  test("rejects float/zero bounds and closed-record violations during extraction", () => {
    const badFloat = compileSnippet(`
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Item = Norn.type({
  name: "inv.Item",
  title: "Item",
  version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String }),
})
export const Bad = Norn.action({
  id: "inv.item.check",
  title: "Check",
  version: "1.0.0",
  scopeRequired: ["orgId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({}),
  slice: [{ name: "items", type: Item, key: { predicate: { maxItems: 1.5 } } }],
})
`)
    expect(badFloat.ok).toBe(false)

    const badZero = compileSnippet(`
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Child = Norn.type({
  name: "inv.Child",
  title: "Child",
  version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String, seq: Schema.Int }),
})
export const Parent = Norn.type({
  name: "inv.Parent",
  title: "Parent",
  version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String }),
  ownedCollections: [{ id: "children", childType: Child, rowIdProperty: "id", orderingKey: "seq", cascadeOnParentDelete: true, maxRows: 0 }],
})
`)
    expect(badZero.ok).toBe(false)

    const badOverMaxRows = compileSnippet(`
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Child = Norn.type({
  name: "inv.Child",
  title: "Child",
  version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String, seq: Schema.Int }),
})
export const Parent = Norn.type({
  name: "inv.Parent",
  title: "Parent",
  version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String }),
  ownedCollections: [{ id: "children", childType: Child, rowIdProperty: "id", orderingKey: "seq", cascadeOnParentDelete: true, maxRows: 101 }],
})
`)
    expect(badOverMaxRows.ok).toBe(false)
  }, 30_000)

  test("auto-pins inherited ownedCollections child entities on mutate_parent_with_rows", () => {
    const compiled = compileSnippet(`
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const LineProps = Schema.Struct({
  line_id: Schema.String,
  seq: Schema.Int,
})

export const BaseLine = Norn.type({
  name: "commerce.BaseLine",
  title: "Base Line",
  version: "1.0.0",
  properties: LineProps,
  identity: [{ name: "primary", properties: ["line_id"] }],
})

export const BaseOrder = Norn.type({
  name: "commerce.BaseOrder",
  title: "Base Order",
  version: "1.0.0",
  properties: Schema.Struct({ order_id: Schema.String }),
  links: [{ id: "lines", target: BaseLine, cardinality: { to: "many", from: "one" } }],
  ownedCollections: [
    {
      id: "lines",
      childType: BaseLine,
      rowIdProperty: "line_id",
      orderingKey: "seq",
      cascadeOnParentDelete: true,
      maxRows: 50,
    },
  ],
  identity: [{ name: "primary", properties: ["order_id"] }],
})

export const SubOrder = Norn.type({
  name: "commerce.SubOrder",
  title: "Sub Order",
  version: "1.0.0",
  extends: BaseOrder,
  properties: Schema.Struct({ note: Schema.String }),
})

const MutateInput = Schema.Struct({
  order_id: Schema.String,
  parent_version: Schema.String,
  parent_values: Schema.Struct({ note: Schema.optionalKey(Schema.String) }),
  row_ops: Schema.Array(Schema.Struct({})),
})

export const SaveSubOrder = Norn.action({
  id: "commerce.sub_order.save_lines",
  title: "Save Sub Order",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: MutateInput,
  output: Schema.Struct({}),
  writes: [
    { type: SubOrder, change: "created" },
    { type: SubOrder, change: "modified" },
    { type: BaseLine, change: "created" },
    { type: BaseLine, change: "modified" },
    { type: BaseLine, change: "deleted" },
  ],
  effect: {
    kind: "mutate_parent_with_rows",
    type: SubOrder,
    objectIdField: "order_id",
    parentValuesField: "parent_values",
    rowOpsField: "row_ops",
    parentVersionField: "parent_version",
    collections: [{ id: "lines" }],
  },
})
`)
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    const actionArtifact = compiled.artifacts.find((a) => (a as { kind: string }).kind === "action") as {
      dependencies: ReadonlyArray<{ artifactId: string; version: string; kind: string; required: boolean }>
    }
    expect(actionArtifact.dependencies).toContainEqual({
      artifactId: "commerce.BaseLine.entity",
      version: "1.0.0",
      kind: "entity",
      required: true,
    })
  }, 30_000)
})
