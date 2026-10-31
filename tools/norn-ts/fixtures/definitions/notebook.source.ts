import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// Fictional notebook row; reports come from the server's admitted analysis job.
export const AnalysisResult = Norn.type({
  name: "synthetic.AnalysisResult", title: "Synthetic analytical evidence", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, state: Schema.Literals(["published", "descriptive", "blocked", "predicted"]), report: Norn.json() }),
  identity: [{ name: "primary", properties: ["id"] }],
})
