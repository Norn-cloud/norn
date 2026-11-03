import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Measurement = Norn.type({
  name: "demo.Measurement", title: "Synthetic source mass", version: "0.1.0",
  properties: Schema.Struct({ mass_mg: Schema.Int, effective_at: Schema.String, source_document: Schema.String, source_sha256: Schema.String, table_sha256: Schema.String }),
})
export const Forecast = Norn.type({
  name: "demo.ScrapForecast", title: "Synthetic recovered mass forecast", version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  properties: Schema.Struct({ input: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }), recovered: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }) }),
})
