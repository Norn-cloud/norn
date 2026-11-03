import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Setting, Registry } from "./binding.source.ts"
const RateValue = Schema.Struct({ value: Norn.decimal(4) })
export const TaxRateContract = Norn.interfaceType({ name: "demo.TaxRateContract", title: "Tax rate contract", version: "1.0.0", properties: Schema.Struct({ value: Norn.decimal(4) }), invariants: [{ id: "tax_rate_range", expression: Norn.expression(RateValue, input => input.value >= 0 && input.value <= 1), message: "Tax rate is between zero and one." }] })
export const TaxRate = Norn.type({ name: "demo.TaxRate", title: "Tax rate", version: "1.0.0", extends: Setting, implements: [TaxRateContract], properties: Schema.Struct({}) })
const RateInput = Schema.Struct({ id: Schema.String, setting_key: Schema.String, value: Norn.decimal(4), effective_from: Schema.String, effective_to: Schema.String, review_state: Schema.String, org_id: Schema.optionalKey(Schema.String), environment_id: Schema.optionalKey(Schema.String), tenant_id: Schema.Null, workspace_id: Schema.Null, business_unit_id: Schema.Null })
export const SetRate = Norn.action({
  id: "demo.tax.set", title: "Set tax rate", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RateInput, output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: TaxRate, change: "created" }, { type: TaxRate, change: "modified" }],
  effect: { kind: "upsert_input_object", type: TaxRate, objectIdField: "id", writeMode: { kind: "upsert" } },
})
export const Configure = Norn.action({
  id: "demo.tax.configure", title: "Configure tax settings", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({ id: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  writes: [{ type: Registry, change: "created" }],
  effect: { kind: "upsert_input_object", type: Registry, objectIdField: "id", writeMode: { kind: "create_only" } },
})
export const Invoice = Norn.type({
  links: [{ id: "configuration", target: Registry, cardinality: { to: "one", from: "many", required: true } }],
  name: "demo.TaxInvoice", title: "Tax invoice", version: "1.0.0", identity: [{ name: "primary", properties: ["id"] }],
  properties: Schema.Struct({ id: Schema.String, currency: Schema.Literal("USD"), net: Norn.decimal(2, { currency: "currency" }), tax: Norn.decimal(2, { currency: "currency" }), gross: Norn.decimal(2, { currency: "currency" }), rate: Norn.decimal(4), effective_at: Norn.timestamp("utc") }),
})
const Input = Schema.Struct({ id: Schema.String, registry: Schema.String, currency: Schema.Literal("USD"), net: Norn.decimal(2, { currency: "currency" }), tax: Norn.decimal(2, { currency: "currency" }), gross: Norn.decimal(2, { currency: "currency" }), rate: Norn.decimal(4), effective_at: Schema.String })
const Context = Schema.Struct({ input: Input, slice: Schema.Struct({ selected: Schema.Struct({ value: Norn.decimal(4) }) }) })
export const PostInvoice = Norn.action({
  id: "demo.tax.post", title: "Post tax invoice", version: "1.0.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Input, output: Schema.Struct({ id: Schema.String }),
  derive: [{ field: "tax", expression: Norn.expression(Input, input => input.net * input.rate), inputRequired: false }, { field: "gross", expression: Norn.expression(Input, input => input.net + input.tax), inputRequired: false }],
  slice: [
    { name: "registry", type: Registry, key: { property: { from: "input", property: "registry" } } },
    { name: "settings", type: Setting, key: { link: { from: "registry", link: "settings" } } },
    { name: "selected", type: Setting, key: { parameter: { from: "settings", settingKey: "tax.rate", effectiveAt: "effective_at", precedence: [{ orgId: "org_demo", environmentId: "dev" }, { orgId: "org_demo" }, {}] } } },
  ],
  createLinks: [{ link: "configuration", targetType: Registry, targetIdField: "registry" }],
  preconditions: [Norn.expression(Context, context => context.input.currency === "USD" && context.input.rate === context.slice.selected.value)],
  writes: [{ type: Invoice, change: "created" }],
  effect: { kind: "upsert_input_object", type: Invoice, objectIdField: "id", writeMode: { kind: "create_only" } },
})
