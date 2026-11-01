import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Setting, Registry } from "./binding.source.ts"

const RateInput = Schema.Struct({ id: Schema.String, setting_key: Schema.String, value: Schema.Finite, effective_from: Schema.String, effective_to: Schema.String, review_state: Schema.Literal("approved"), org_id: Schema.String, environment_id: Schema.String, tenant_id: Schema.Null, workspace_id: Schema.Null, business_unit_id: Schema.Null })
export const SetRate = Norn.action({
  id: "demo.tax.set", title: "Set tax rate", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RateInput, output: Schema.Struct({ id: Schema.String }),
  preconditions: [Norn.expression(RateInput, input => input.value >= 0 && input.value <= 1)],
  writes: [{ type: Setting, change: "created" }, { type: Setting, change: "modified" }],
  effect: { kind: "upsert_input_object", type: Setting, objectIdField: "id", writeMode: { kind: "upsert" } },
})
export const Configure = Norn.action({
  id: "demo.tax.configure", title: "Configure tax settings", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ id: Schema.String, june: Schema.String, july: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  createLinks: [{ link: "settings", targetType: Setting, targetIdField: "june" }, { link: "settings", targetType: Setting, targetIdField: "july" }],
  writes: [{ type: Registry, change: "created" }],
  effect: { kind: "upsert_input_object", type: Registry, objectIdField: "id", writeMode: { kind: "create_only" } },
})
export const Invoice = Norn.type({
  name: "demo.TaxInvoice", title: "Tax invoice", version: "1.0.0", identity: [{ name: "primary", properties: ["id"] }],
  properties: Schema.Struct({ id: Schema.String, currency: Schema.String, net: Norn.decimal(2, { currency: "currency" }), tax: Norn.decimal(2, { currency: "currency" }), gross: Norn.decimal(2, { currency: "currency" }), rate: Norn.decimal(4), effective_at: Norn.timestamp("utc") }),
})
const Input = Schema.Struct({ id: Schema.String, registry: Schema.String, currency: Schema.Literal("USD"), net: Schema.Finite, tax: Schema.Finite, gross: Schema.Finite, rate: Schema.Finite, effective_at: Schema.String })
const Context = Schema.Struct({ input: Input, slice: Schema.Struct({ selected: Schema.Struct({ value: Schema.Finite }) }) })
export const PostInvoice = Norn.action({
  id: "demo.tax.post", title: "Post tax invoice", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Input, output: Schema.Struct({ id: Schema.String }),
  derive: [{ field: "tax", expression: Norn.expression(Input, input => input.net * input.rate), inputRequired: false }, { field: "gross", expression: Norn.expression(Input, input => input.net + input.tax), inputRequired: false }],
  slice: [
    { name: "registry", type: Registry, key: { property: { from: "input", property: "registry" } } },
    { name: "settings", type: Setting, key: { link: { from: "registry", link: "settings" } } },
    { name: "selected", type: Setting, key: { parameter: { from: "settings", settingKey: "tax.rate", effectiveAt: "effective_at", precedence: [{ orgId: "org_demo", environmentId: "dev" }, { orgId: "org_demo" }, {}] } } },
  ],
  preconditions: [Norn.expression(Context, context => context.input.rate === context.slice.selected.value)],
  writes: [{ type: Invoice, change: "created" }],
  effect: { kind: "upsert_input_object", type: Invoice, objectIdField: "id", writeMode: { kind: "create_only" } },
})
