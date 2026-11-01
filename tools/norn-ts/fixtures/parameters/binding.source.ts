import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const NullableScope = Schema.NullOr(Schema.String)
export const SettingFields = Schema.Struct({
  id: Schema.String, setting_key: Schema.String, value: Norn.decimal(4),
  effective_from: Norn.timestamp("utc"), effective_to: Schema.NullOr(Norn.timestamp("utc")),
  review_state: Schema.Literal("approved"),
  org_id: NullableScope, tenant_id: NullableScope, workspace_id: NullableScope,
  environment_id: NullableScope, business_unit_id: NullableScope,
})
export const Setting = Norn.type({ name: "demo.NumericSetting", title: "Setting", version: "1.0.0", properties: SettingFields, identity: [{ name: "primary", properties: ["id"] }] })
export const Registry = Norn.type({
  name: "demo.SettingRegistry", title: "Registry", version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String }), identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "settings", target: Setting, cardinality: { to: "many", from: "many" } }],
})
export const Probe = Norn.type({ name: "demo.ParameterProbe", title: "Probe", version: "1.0.0", properties: Schema.Struct({ id: Schema.String, value: Norn.decimal(4) }), identity: [{ name: "primary", properties: ["id"] }] })
const Input = Schema.Struct({ id: Schema.String, registry: Schema.String, value: Schema.Finite, effective_at: Schema.String })
const Context = Schema.Struct({ input: Input, slice: Schema.Struct({ selected: Schema.Struct({ value: Schema.Finite }) }) })
export const RecordProbe = Norn.action({
  id: "demo.parameter.probe", title: "Record parameter probe", version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Input, output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "registry", type: Registry, key: { property: { from: "input", property: "registry" } } },
    { name: "settings", type: Setting, key: { link: { from: "registry", link: "settings" } } },
    { name: "selected", type: Setting, key: { parameter: { from: "settings", settingKey: "example.rate", effectiveAt: "effective_at", precedence: [{ orgId: "org_demo", environmentId: "dev" }, { orgId: "org_demo" }, {}] } } },
  ],
  preconditions: [Norn.expression(Context, context => context.input.value === context.slice.selected.value, { root: "context" })],
  writes: [{ type: Probe, change: "created" }],
  effect: { kind: "upsert_input_object", type: Probe, objectIdField: "id", writeMode: { kind: "create_only" } },
})
