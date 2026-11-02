import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Measurement = Norn.interfaceType({
  name: "demo.erpnext.Measurement", title: "Synthetic ERPNext measurement evidence", version: "0.1.0",
  properties: Schema.Struct({
    id: Schema.String, metal: Schema.Literals(["gold", "silver"]),
    role: Schema.Literals(["input", "output", "loss"]),
    gross_grams: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }), purity: Norn.decimal(6),
    fine_grams: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }), occurred_at: Schema.String,
  }),
})

export const StockEntryMeasurement = Norn.type({
  name: "demo.erpnext.StockEntryMeasurement", title: "Stock Entry measurement row", version: "0.1.0",
  implements: [Measurement],
  properties: Schema.Struct({
    id: Schema.String, metal: Schema.Literals(["gold", "silver"]),
    role: Schema.Literals(["input", "output", "loss"]),
    gross_grams: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }), purity: Norn.decimal(6),
    fine_grams: Norn.decimal(6, { unit: { system: "ucum", code: "g" } }), occurred_at: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "lot", target: "core.Resource", cardinality: { to: "one", from: "many" } },
    { id: "event", target: "core.EconomicEvent", cardinality: { to: "one", from: "many" } },
  ],
})

export const Source = Norn.sourceSchema({
  id: "demo.erpnext.measurements.source", title: "Synthetic ERPNext custom measurement profile", version: "0.1.0",
  source: { system: "synthetic-erpnext", instance: "demo", version: "16-custom-measurements-v1" },
  observedAt: "2026-10-25T23:59:59Z", adapter: { name: "synthetic-feed-replay", version: "0.1.0" },
  rawMetadata: { uri: "evidence://demo-erpnext/contract-v1", sha256: "91385b57e376c46d0b3b53db6f746a5f0ab7891a37d5046a6a568edf8234ddaa" },
  datasets: [{ id: "Stock Entry Measurement", nativeName: "Synthetic Stock Entry custom measurement row", kind: "child_document" }],
  fields: [
    { dataset: "Stock Entry Measurement", name: "name", ordinal: 0, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Stock Entry Measurement", name: "parent", ordinal: 1, nativeType: "Link", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Stock Entry Measurement", name: "custom_metal", ordinal: 2, nativeType: "Select", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Stock Entry Measurement", name: "custom_role", ordinal: 3, nativeType: "Select", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Stock Entry Measurement", name: "custom_gross_grams", ordinal: 4, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Stock Entry Measurement", name: "custom_purity", ordinal: 5, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Stock Entry Measurement", name: "custom_fine_grams", ordinal: 6, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Stock Entry Measurement", name: "custom_occurred_at", ordinal: 7, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Stock Entry Measurement", name: "custom_lot", ordinal: 8, nativeType: "Link", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  codeSets: [
    { id: "metal", dataset: "Stock Entry Measurement", field: "custom_metal", origin: "declared", values: [{ value: "gold" }, { value: "silver" }] },
    { id: "role", dataset: "Stock Entry Measurement", field: "custom_role", origin: "declared", values: [{ value: "input" }, { value: "output" }, { value: "loss" }] },
  ],
  constraints: [{ id: "row_identity", kind: "primary_key", dataset: "Stock Entry Measurement", fields: ["name"], origin: "declared" }],
})

export const Mapping = Norn.mapping({
  id: "demo.erpnext.measurements.mapping", title: "Synthetic ERPNext measurement evidence mapping", version: "0.1.0",
  source: { schemaId: "demo.erpnext.measurements.source", version: "0.1.0", sha256: "4c178bb1e5b4862823ab2219ced970afccfc3a6efa564764dce0f9e2a8d947c3" },
  sourceContract: { kind: "generic" }, interfaces: [Measurement],
  maps: [{
    id: "measurement", source: { dataset: "Stock Entry Measurement" }, target: { type: StockEntryMeasurement, interface: Measurement },
    subject: "input.name", sourceRecord: "input.parent + '/' + input.name",
    properties: [
      { property: "id", value: "input.name" }, { property: "metal", value: "input.custom_metal" },
      { property: "role", value: "input.custom_role" }, { property: "gross_grams", value: "input.custom_gross_grams" },
      { property: "purity", value: "input.custom_purity" }, { property: "fine_grams", value: "input.custom_fine_grams" },
      { property: "occurred_at", value: "input.custom_occurred_at" },
    ],
    gaps: [
      { member: "lot", reason: "deferred", note: "Requires the admitted lane C Holding/MetalLot mapping; a catalog Item is never a lot." },
      { member: "event", reason: "deferred", note: "Requires lane C conversion identity and multi-object roles; these rows grant no posting authority." },
    ],
    ignoredFields: [{ field: "custom_lot", reason: "Retained in the source document; canonical lot resolution is an explicit gap." }],
  }],
})

export const PurityWithinBounds = Norn.law({
  id: "demo.erpnext.purity.law", title: "Measurement purity is within zero and one", version: "0.1.0",
  wordings: { "demo.erpnext.purity_in_range": {
    lawDigest: "5f430bdd5473e7eac2d6f5ebb08d1f2b399598000772d774b64221e9065c4a5f",
    slots: {
      scope: { en: "Each synthetic ERPNext measurement", ar: "كل قياس اصطناعي من ERPNext" },
      activation: { en: "At every committed state", ar: "في كل حالة معتمدة" },
      component: { en: "Its recorded purity", ar: "نقاؤه المسجّل" },
      response: { en: "Purity is between zero and one, inclusive", ar: "النقاء بين الصفر والواحد شاملاً الحدين" },
    },
  } },
  dependencies: [{ artifactId: "demo.erpnext.StockEntryMeasurement.entity", kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id: "demo.erpnext.purity_in_range", owner: "demo.erpnext.controls",
    severity: "error", rationale: "Measurement purity must be within zero and one.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/demo-erpnext/measurements.ts" },
    subject: "demo.erpnext.StockEntryMeasurement",
    scope: { root: { kind: "entity", id: "demo.erpnext.StockEntryMeasurement" }, objectTypes: ["demo.erpnext.StockEntryMeasurement"] },
    modality: { kind: "always" }, response: { kind: "predicate", expression: "self.purity >= 0 and self.purity <= 1" },
  },
})

export const FineMassDefinition = Norn.law({
  id: "demo.erpnext.fine_mass_definition.law", title: "Fine mass equals gross mass times purity", version: "0.1.0",
  dependencies: [{ artifactId: "demo.erpnext.StockEntryMeasurement.entity", kind: "entity", version: "0.1.0", required: true }],
  wordings: { "demo.erpnext.fine_mass_definition": {
    lawDigest: "195ace9eb8fd04495af8fa8609378cfcbd330279a83e5f758829d7ab1113e91c",
    slots: {
      scope: { en: "Each synthetic ERPNext measurement", ar: "كل قياس اصطناعي من ERPNext" },
      activation: { en: "At every committed state", ar: "في كل حالة معتمدة" },
      component: { en: "Its fine-metal mass in grams", ar: "كتلة معدنه الخالص بالغرام" },
      response: { en: "Fine mass equals gross mass multiplied by purity", ar: "كتلة المعدن الخالص تساوي الكتلة الإجمالية مضروبة في النقاء" },
    },
  } },
  law: {
    lawVersion: "0.1", id: "demo.erpnext.fine_mass_definition", owner: "demo.erpnext.controls",
    severity: "error", rationale: "Fine-metal mass is defined exactly by gross mass times purity.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/demo-erpnext/measurements.ts" },
    subject: "demo.erpnext.StockEntryMeasurement",
    scope: { root: { kind: "entity", id: "demo.erpnext.StockEntryMeasurement" }, objectTypes: ["demo.erpnext.StockEntryMeasurement"] },
    modality: { kind: "always" }, response: { kind: "predicate", expression: "self.fine_grams == self.gross_grams * self.purity" },
  },
})

/** DATA-QUALITY-PACK-1: inherit the core conversion group; no pack conservation alias. */
export const ScrapMelt = Norn.type({
  name: "demo.erpnext.ScrapMelt", title: "Synthetic scrap melt measurements", version: "0.1.0",
  description: "Accepted fine-metal input, output and recorded loss; stock and value effects remain unqualified.",
  implements: ["core.Conversion"],
  properties: Schema.Struct({
    id: Schema.String,
    input_quantity: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    output_quantity: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    loss_quantity: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})
