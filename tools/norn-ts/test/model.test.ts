import { expect, test } from "bun:test"
import { compileModule } from "../src/compile-stdin.ts"
import { checker } from "./checker.ts"

// ModelIR (#1171 M1): `Norn.model` lowers to one `model` artifact whose
// outputs reuse the property lowering; the Rust checker resolves the input
// type and its features.
const scope = { orgId: "org_demo", environmentId: "dev" }

const SOURCE = `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Customer = Norn.type({
  name: "sales.Customer",
  title: "Customer",
  version: "1.0.0",
  properties: Schema.Struct({
    tenure_months: Schema.Int,
    monthly_spend: Norn.decimal(2),
    plan: Schema.Literals(["basic", "pro"]),
    region: Schema.String,
    churned: Schema.Boolean,
    signed_up: Norn.date(),
  }),
})

export const Churn = Norn.model({
  id: "sales.churn_model",
  title: "Churn risk",
  version: "1.0.0",
  input: Customer,
  features: ["tenure_months", "monthly_spend", "plan", "region"],
  target: "churned",
  outputs: Schema.Struct({
    churn_probability: Norn.decimal(4),
    risk_band: Schema.NullOr(Schema.Literals(["low", "medium", "high"])),
    will_churn: Schema.Boolean,
  }),
  trust: "contract_checked",
  contracts: [
    { id: "probability", output: "churn_probability", rule: { kind: "range", min: "0", max: "1" } },
    { id: "band", output: "risk_band", rule: { kind: "oneOf", values: ["low", "high"] } },
  ],
  method: { label: "logistic regression", version: "sklearn 1.6" },
})
`

function compile(source: string): any[] {
  const result = compileModule(source, scope)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  return result.artifacts as any[]
}

test("Norn.model lowers to a model artifact with typed outputs", () => {
  const model = compile(SOURCE).find((a) => a.kind === "model")
  expect(model.artifactId).toBe("sales.churn_model.model")
  expect(model.namespace).toBe("sales")
  expect(model.name).toBe("Churn risk")
  expect(model.payload).toEqual({
    modelId: "sales.churn_model",
    title: "Churn risk",
    version: "1.0.0",
    input: { kind: "objectType", objectType: "sales.Customer" },
    features: ["tenure_months", "monthly_spend", "plan", "region"],
    target: "churned",
    outputs: [
      { id: "churn_probability", type: { kind: "decimal", scale: 4 }, nullable: false },
      { id: "risk_band", type: { kind: "enum", values: [{ value: "low" }, { value: "medium" }, { value: "high" }] }, nullable: true },
      { id: "will_churn", type: { kind: "boolean" }, nullable: false },
    ],
    trust: "contract_checked",
    contracts: [
      { id: "probability", output: "churn_probability", rule: { kind: "range", min: "0", max: "1" } },
      { id: "band", output: "risk_band", rule: { kind: "oneOf", values: ["low", "high"] } },
    ],
    method: { label: "logistic regression", version: "sklearn 1.6" },
  })
}, 30_000)

test("an opaque scoring model omits its target and empty contracts; an installed input is named", () => {
  const source = `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Score = Norn.model({
  id: "sales.lead_score", title: "Lead score", version: "0.1.0",
  input: "crm.Lead", features: ["source"],
  outputs: Schema.Struct({ score: Schema.Int }),
  trust: "opaque", method: { label: "vendor model" },
})
`
  const [model] = compile(source)
  expect(model.payload.input).toEqual({ kind: "objectType", objectType: "crm.Lead" })
  expect(model.payload.target).toBeUndefined()
  expect(model.payload.contracts).toBeUndefined()
  expect(model.payload.method).toEqual({ label: "vendor model" })
}, 30_000)

test("a malformed trust, rule or range bound is a compile error", () => {
  for (const [from, to] of [
    ['trust: "contract_checked"', 'trust: "trusted"'],
    ['rule: { kind: "range", min: "0", max: "1" }', 'rule: { kind: "between", min: "0", max: "1" }'],
    ['min: "0", max: "1"', 'min: "0", max: "1e3"'],
    ['rule: { kind: "oneOf", values: ["low", "high"] }', 'rule: { kind: "oneOf", values: ["low"], min: "0" }'],
  ] as const) {
    expect(compileModule(SOURCE.replace(from, to), scope).ok).toBe(false)
  }
}, 30_000)

test("a feature the referenced type does not declare is a compile-time type error", () => {
  const result = compileModule(SOURCE.replace('"region"],', '"region", "lifetime_value"],'), scope)
  expect(result.ok).toBe(false)
  expect((result.diagnostics ?? []).map((d) => d.code)).toContain("NORN-FLOW-TYPE")
}, 30_000)

test.skipIf(!checker)("Rust admits the model and refuses an unknown or non-admitted feature", () => {
  const codes = (artifacts: any[]): string[] =>
    (checker!.checkArtifactSet(artifacts).diagnostics.diagnostics as unknown as readonly { code: string }[]).map((d) => d.code)
  expect(codes(compile(SOURCE))).toEqual([])
  // Named by string, the input type is resolved by the checker alone.
  const byName = SOURCE.replace("input: Customer,", 'input: "sales.Customer",')
  expect(codes(compile(byName.replace('"region"],', '"region", "lifetime_value"],')))).toContain("model.admission.unknown_feature")
  expect(codes(compile(SOURCE.replace('"region"],', '"region", "signed_up"],')))).toContain("model.admission.feature_kind")
  expect(codes(compile(SOURCE.replace('input: Customer,', 'input: "sales.Prospect",')))).toContain("model.admission.unknown_input")
}, 30_000)

// M4: a model names the typed object type its predictions are written to.
const PREDICTION = `
export const ChurnPrediction = Norn.type({
  name: "sales.ChurnPrediction",
  title: "Churn prediction",
  version: "1.0.0",
  properties: Schema.Struct({
    prediction_id: Schema.String,
    subject: Schema.String,
    model_version: Schema.String,
    churn_probability: Norn.decimal(4),
    risk_band: Schema.NullOr(Schema.Literals(["low", "medium", "high"])),
    will_churn: Schema.Boolean,
  }),
})
`
const PREDICTING = SOURCE.replace("export const Churn = ", `${PREDICTION}\nexport const Churn = `)
  .replace('  method: { label: "logistic regression"', '  predictions: ChurnPrediction,\n  method: { label: "logistic regression"')

test("predictions name a referenced type or a qualified name", () => {
  const model = compile(PREDICTING).find((a) => a.kind === "model")
  expect(model.payload.predictions).toEqual({ objectType: "sales.ChurnPrediction" })
  const byName = compile(PREDICTING.replace("predictions: ChurnPrediction,", 'predictions: "sales.Forecast",')).find((a) => a.kind === "model")
  expect(byName.payload.predictions).toEqual({ objectType: "sales.Forecast" })
  // Omitted, the payload has no predictions member.
  expect(compile(SOURCE).find((a) => a.kind === "model").payload.predictions).toBeUndefined()
}, 30_000)

test.skipIf(!checker)("Rust admits a fitting predictions type and refuses one that does not hold an output", () => {
  const codes = (artifacts: any[]): string[] =>
    (checker!.checkArtifactSet(artifacts).diagnostics.diagnostics as unknown as readonly { code: string }[]).map((d) => d.code)
  expect(codes(compile(PREDICTING))).toEqual([])
  expect(codes(compile(PREDICTING.replace("    will_churn: Schema.Boolean,\n  }),\n})\n\nexport const Churn", "  }),\n})\n\nexport const Churn"))))
    .toContain("model.admission.prediction_type_mismatch")
}, 30_000)
