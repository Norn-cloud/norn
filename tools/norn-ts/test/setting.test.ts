// Governed effective-dated settings (`Norn.setting` -> `SettingRoot`,
// PARAM-EXACT-1 / PARAM-BOUND-1 / VAL-EXACT-1) and end-to-end parameter slice
// admission through the Rust checker.
import { describe, expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { parseRfc3339Instant } from "../src/builder/setting.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"
if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const scope = { orgId: "org_demo", environmentId: "dev" }

function compileSnippet(source: string) {
  const dir = mkdtempSync(join(tmpdir(), "norn-setting-"))
  try {
    const file = join(dir, "settings.ts")
    writeFileSync(file, source)
    return compileDefinitions(file, { scope, sourcePath: "settings.ts" })
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

describe("Norn.setting", () => {
  test("compiles exact decimal, text, boolean, and json settings to canonical SettingRoot IR and admits via norn-checker", () => {
    const compiled = compileSnippet(`
import { Norn } from "@norn/authoring"

export const VatRateJune = Norn.setting({
  id: "tax.setting.vat_rate_june",
  title: "VAT Rate June",
  version: "0.1.0",
  settingKey: "tax.vat_rate",
  valueType: "number",
  value: "0.10",
  effectiveFrom: "2026-06-01T00:00:00Z",
  effectiveTo: "2026-07-01T00:00:00Z",
  review: {
    state: "approved",
    reviewedBy: "finance_lead",
    reviewedAt: "2026-05-20T09:00:00Z",
    reason: "H1 statutory rate",
  },
  scopeRequired: ["orgId", "environmentId"],
})

export const VatRateJuly = Norn.setting({
  id: "tax.setting.vat_rate_july",
  title: "VAT Rate July",
  version: "0.2.0",
  settingKey: "tax.vat_rate",
  valueType: "number",
  value: "0.12",
  effectiveFrom: "2026-07-01T00:00:00Z",
  supersedes: { artifactId: "tax.setting.vat_rate_june", version: "0.1.0" },
  review: { state: "active" },
  scopeRequired: ["orgId", "environmentId"],
})

export const RegionLabel = Norn.setting({
  id: "tax.setting.region_label",
  title: "Region Label",
  version: "0.1.0",
  settingKey: "tax.region_label",
  valueType: "text",
  value: "EU-VAT",
  effectiveFrom: "2026-01-01T00:00:00Z",
  review: { state: "draft" },
  scopeRequired: ["orgId"],
})

export const ReverseCharge = Norn.setting({
  id: "tax.setting.reverse_charge",
  title: "Reverse Charge Enabled",
  version: "0.1.0",
  settingKey: "tax.reverse_charge",
  valueType: "boolean",
  value: true,
  effectiveFrom: "2026-01-01T00:00:00Z",
  review: { state: "approved" },
  scopeRequired: ["orgId"],
})

export const NotificationConfig = Norn.setting({
  id: "tax.setting.notification_config",
  title: "Notification Config",
  version: "0.1.0",
  settingKey: "tax.notifications",
  valueType: "json",
  value: { channels: ["email"], quietHours: null, retries: 2 },
  effectiveFrom: "2026-01-01T00:00:00Z",
  review: { state: "proposed" },
  scopeRequired: ["orgId"],
})
`)
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    expect(compiled.artifacts).toHaveLength(5)
    expect(compiled.artifacts[0]).toEqual({
      irVersion: "0.1",
      artifactId: "tax.setting.vat_rate_june",
      kind: "setting",
      namespace: "tax",
      name: "VAT Rate June",
      version: "0.1.0",
      lifecycle: "draft",
      scope,
      dependencies: [],
      provenance: { source: "authoring_sdk", sourcePath: "settings.ts" },
      payload: {
        settingVersion: "0.1",
        settingKey: "tax.vat_rate",
        valueType: "number",
        value: "0.10",
        effectiveFrom: "2026-06-01T00:00:00Z",
        effectiveTo: "2026-07-01T00:00:00Z",
        review: {
          state: "approved",
          reviewedBy: "finance_lead",
          reviewedAt: "2026-05-20T09:00:00Z",
          reason: "H1 statutory rate",
        },
        scopeRequired: ["orgId", "environmentId"],
      },
    })
    expect(compiled.artifacts[1]).toMatchObject({
      artifactId: "tax.setting.vat_rate_july",
      kind: "setting",
      payload: {
        settingVersion: "0.1",
        settingKey: "tax.vat_rate",
        valueType: "number",
        value: "0.12",
        supersedes: { artifactId: "tax.setting.vat_rate_june", version: "0.1.0" },
        review: { state: "active" },
      },
    })
    if (checker) {
      const checked = checker.checkArtifactSet(compiled.artifacts)
      expect(checked.diagnostics.diagnostics).toEqual([])
      expect(checked.admitted).toBe(true)
    }
  }, 30_000)

  test("end-to-end authored setting + parameter slice action admits cleanly through norn-checker", () => {
    const compiled = compileSnippet(`
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const NullableScope = Schema.NullOr(Schema.String)

export const TaxSetting = Norn.setting({
  id: "billing.setting.vat_rate",
  title: "VAT Rate",
  version: "0.1.0",
  settingKey: "billing.vat_rate",
  valueType: "number",
  value: "0.10",
  effectiveFrom: "2026-06-01T00:00:00Z",
  effectiveTo: "2026-07-01T00:00:00Z",
  review: { state: "approved" },
  scopeRequired: ["orgId", "environmentId"],
})

export const NumericSetting = Norn.type({
  name: "billing.NumericSetting",
  title: "Numeric Setting",
  version: "1.0.0",
  properties: Schema.Struct({
    id: Schema.String,
    setting_key: Schema.String,
    value: Norn.decimal(4),
    effective_from: Norn.timestamp("utc"),
    effective_to: Schema.NullOr(Norn.timestamp("utc")),
    review_state: Schema.Literals(["draft", "proposed", "approved", "active", "rejected", "superseded"]),
    org_id: NullableScope,
    tenant_id: NullableScope,
    workspace_id: NullableScope,
    environment_id: NullableScope,
    business_unit_id: NullableScope,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const SettingRegistry = Norn.type({
  name: "billing.SettingRegistry",
  title: "Setting Registry",
  version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "settings", target: NumericSetting, cardinality: { to: "many", from: "many" } }],
})

export const Invoice = Norn.type({
  name: "billing.Invoice",
  title: "Invoice",
  version: "1.0.0",
  properties: Schema.Struct({
    id: Schema.String,
    tax_rate: Norn.decimal(4),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

const PostInput = Schema.Struct({
  id: Schema.String,
  registry: Schema.String,
  default_setting: Schema.String,
  tax_rate: Norn.decimal(4),
  effective_at: Schema.String,
})
const PostContext = Schema.Struct({
  input: PostInput,
  slice: Schema.Struct({ selected: Schema.Struct({ value: Norn.decimal(4) }) }),
})

export const PostInvoice = Norn.action({
  id: "billing.invoice.post",
  title: "Post Invoice",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: PostInput,
  output: Schema.Struct({ id: Schema.String }),
  slice: [
    { name: "registry", type: SettingRegistry, key: { property: { from: "input", property: "registry" } } },
    { name: "settings", type: NumericSetting, key: { link: { from: "registry", link: "settings" } } },
    { name: "fallback", type: NumericSetting, key: { property: { from: "input", property: "default_setting" } } },
    {
      name: "selected",
      type: NumericSetting,
      key: {
        parameter: {
          from: "settings",
          settingKey: "billing.vat_rate",
          effectiveAt: "effective_at",
          precedence: [{ orgId: "org_demo", environmentId: "dev" }, { orgId: "org_demo" }, {}],
          defaultFrom: "fallback",
        },
      },
    },
  ],
  preconditions: [
    Norn.expression(PostContext, (context) => context.input.tax_rate === context.slice.selected.value, { root: "context" }),
  ],
  writes: [{ type: Invoice, change: "created" }],
  effect: { kind: "upsert_input_object", type: Invoice, objectIdField: "id", writeMode: { kind: "create_only" } },
})
`)
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    expect(compiled.artifacts.some((a) => (a as { kind: string }).kind === "setting")).toBe(true)
    if (checker) {
      const checked = checker.checkArtifactSet(compiled.artifacts)
      expect(checked.diagnostics.diagnostics).toEqual([])
      expect(checked.admitted).toBe(true)
    }
  }, 30_000)

  test("refuses binary JS numbers on decimal settings (VAL-EXACT-1)", () => {
    for (const literal of ["0.12", "-0.12", "10"]) {
      const refused = compileSnippet(`
import { Norn } from "@norn/authoring"
export const Bad = Norn.setting({
  id: "tax.setting.bad_float",
  title: "Bad Float",
  version: "0.1.0",
  settingKey: "tax.rate",
  valueType: "number",
  value: ${literal} as unknown as string,
  effectiveFrom: "2026-06-01T00:00:00Z",
  review: { state: "approved" },
  scopeRequired: ["orgId"],
})
`)
      expect(refused.ok).toBe(false)
      if (!refused.ok) {
        expect(refused.diagnostics.length).toBeGreaterThan(0)
      }
    }
    const untypedFloat = compileSnippet(`
import { Norn } from "@norn/authoring"
export const Bad = Norn.setting({
  id: "tax.setting.bad_float_direct",
  title: "Bad Float",
  version: "0.1.0",
  settingKey: "tax.rate",
  valueType: "number",
  value: 0.12,
  effectiveFrom: "2026-06-01T00:00:00Z",
  review: { state: "approved" },
  scopeRequired: ["orgId"],
})
`)
    expect(untypedFloat.ok).toBe(false)
  }, 30_000)

  test("refuses invalid decimal strings, windows, unknown fields, non-serializable values, and missing required fields", () => {
    const cases: ReadonlyArray<readonly [string, string]> = [
      [
        "exponent decimal",
        `valueType: "number", value: "1e2", effectiveFrom: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId"]`,
      ],
      [
        "leading plus decimal",
        `valueType: "number", value: "+0.12", effectiveFrom: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId"]`,
      ],
      [
        "leading zero decimal",
        `valueType: "number", value: "01.5", effectiveFrom: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId"]`,
      ],
      [
        "scale over 28",
        `valueType: "number", value: "0.${"1".repeat(29)}", effectiveFrom: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId"]`,
      ],
      [
        "mantissa over 96 bits",
        `valueType: "number", value: "999999999999999999999999999999", effectiveFrom: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId"]`,
      ],
      [
        "invalid calendar date",
        `valueType: "number", value: "0.10", effectiveFrom: "2026-02-30T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId"]`,
      ],
      [
        "inverted effective window",
        `valueType: "number", value: "0.10", effectiveFrom: "2026-07-01T00:00:00Z", effectiveTo: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId"]`,
      ],
      [
        "zero-width effective window across offsets",
        `valueType: "number", value: "0.10", effectiveFrom: "2026-07-01T00:00:00Z", effectiveTo: "2026-07-01T02:00:00+02:00", review: { state: "approved" }, scopeRequired: ["orgId"]`,
      ],
      [
        "unknown top-level field",
        `valueType: "number", value: "0.10", effectiveFrom: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId"], extra: true`,
      ],
      [
        "non-serializable json value",
        `valueType: "json", value: () => 1, effectiveFrom: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId"]`,
      ],
      [
        "missing review",
        `valueType: "number", value: "0.10", effectiveFrom: "2026-06-01T00:00:00Z", scopeRequired: ["orgId"]`,
      ],
      [
        "empty scopeRequired",
        `valueType: "number", value: "0.10", effectiveFrom: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: []`,
      ],
      [
        "duplicate scopeRequired",
        `valueType: "number", value: "0.10", effectiveFrom: "2026-06-01T00:00:00Z", review: { state: "approved" }, scopeRequired: ["orgId", "orgId"]`,
      ],
    ]
    for (const [label, body] of cases) {
      const refused = compileSnippet(`
import { Norn } from "@norn/authoring"
export const Bad = Norn.setting({
  id: "tax.setting.bad",
  title: "Bad",
  version: "0.1.0",
  settingKey: "tax.rate",
  ${body},
})
`)
      expect({ label, ok: refused.ok }).toEqual({ label, ok: false })
    }

    for (const badId of [" .foo", "tax. ", " tax.vat", "tax.vat ", "tax..vat", "tax. .vat", "tax. vat", "tax", ".vat", "tax."]) {
      const refused = compileSnippet(`
import { Norn } from "@norn/authoring"
export const Bad = Norn.setting({
  id: ${JSON.stringify(badId)},
  title: "Bad",
  version: "0.1.0",
  settingKey: "tax.rate",
  valueType: "number",
  value: "0.10",
  effectiveFrom: "2026-06-01T00:00:00Z",
  review: { state: "approved" },
  scopeRequired: ["orgId"],
})
`)
      expect({ badId, ok: refused.ok }).toEqual({ badId, ok: false })
    }
  }, 30_000)

  test("parseRfc3339Instant preserves literal 2-digit years (0000..0099) without JS 1900 offset", () => {
    const year50 = parseRfc3339Instant("0050-01-01T00:00:00Z")
    const year100 = parseRfc3339Instant("0100-01-01T00:00:00Z")
    const year1950 = parseRfc3339Instant("1950-01-01T00:00:00Z")
    expect(year50).toBeDefined()
    expect(year100).toBeDefined()
    expect(year1950).toBeDefined()
    expect(year50! < year100!).toBe(true)
    expect(year50! < year1950!).toBe(true)
    expect(year100! < year1950!).toBe(true)
  })
})
