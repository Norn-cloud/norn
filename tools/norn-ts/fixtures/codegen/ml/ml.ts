// `norn codegen --ts` fixture: model training runs. Golden: ../ml.d.ts.
// Shared value types (`core.time.*`) resolve from registry/core.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const RunFields = Schema.Struct({
  run_id: Schema.String,
  model: Schema.String,
  learning_rate: Norn.decimal(6),
  epochs: Schema.Int,
  accuracy: Schema.NullOr(Norn.decimal(4)),
})

export const TrainingRun = Norn.type({
  name: "ml.TrainingRun",
  title: "Training run",
  version: "1.0.0",
  properties: Schema.Struct({
    run_id: Schema.String,
    model: Schema.String,
    learning_rate: Norn.decimal(6),
    epochs: Schema.Int,
    stage: Schema.Literals(["queued", "training", "evaluated"]),
    accuracy: Schema.NullOr(Norn.decimal(4)),
    window: Schema.NullOr(Norn.ref("core.time.proper_interval")),
  }),
  identity: [{ name: "primary", properties: ["run_id"] }],
  lifecycles: [{
    property: "stage",
    initial: ["queued"],
    transitions: [
      { id: "start", from: ["queued"], to: "training" },
      { id: "finish", from: ["training"], to: "evaluated" },
    ],
    terminal: ["evaluated"],
  }],
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

export const StartRun = Norn.action({
  id: "ml.run.start",
  title: "Start a training run",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    run_id: Schema.String,
    tags: Schema.Array(Schema.String),
    budget: Schema.Struct({ max_epochs: Schema.Int, max_cost: Norn.decimal(2) }),
    reason: Schema.optionalKey(Schema.String),
  }),
  output: Schema.Struct({ run_id: Schema.String }),
  writes: [{ type: TrainingRun, change: "modified" }],
  effect: { kind: "fire_transition", type: TrainingRun, objectIdField: "run_id", lifecycle: "stage", transition: "start" },
})

// A closed empty input: the caller sends `{}` and nothing else.
export const Heartbeat = Norn.action({
  id: "ml.scheduler.heartbeat",
  title: "Record a scheduler heartbeat",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: false,
  input: Schema.Struct({}),
  output: Schema.Struct({}),
})

// An input whose only member is the verified caller: emptied for the caller, never sent.
export const ClaimQueue = Norn.action({
  id: "ml.queue.claim",
  title: "Claim the training queue",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: false,
  input: Schema.Struct({ claimed_by: Schema.String }),
  output: Schema.Struct({}),
  fromCaller: ["claimed_by"],
})
