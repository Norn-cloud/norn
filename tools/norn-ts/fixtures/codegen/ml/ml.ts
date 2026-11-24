// `norn codegen --ts` fixture: model training runs. Golden: ../ml.d.ts.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const RunFields = Schema.Struct({
  run_id: Schema.String,
  model: Schema.String,
  learning_rate: Norn.decimal(6),
  epochs: Schema.Int,
  stage: Schema.Literals(["queued", "training", "evaluated"]),
  accuracy: Schema.NullOr(Norn.decimal(4)),
})

export const TrainingRun = Norn.type({
  name: "ml.TrainingRun",
  title: "Training run",
  version: "1.0.0",
  properties: RunFields,
  identity: [{ name: "primary", properties: ["run_id"] }],
})

export const RecordRun = Norn.action({
  id: "ml.run.record",
  title: "Record a training run",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: RunFields,
  output: Schema.Struct({ run_id: Schema.String }),
  writes: [{ type: TrainingRun, change: "created" }, { type: TrainingRun, change: "modified" }],
  effect: { kind: "upsert_input_object", type: TrainingRun, objectIdField: "run_id" },
})
