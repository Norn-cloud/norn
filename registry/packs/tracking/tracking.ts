import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// Experiment tracking as Norn definitions, in the shape of MLflow's tracking
// API (see README.md). Every write is one of the actions below, called through
// the generic governed action route.

/** A named group of runs. Its writer derives `experiment_id` from the name, so names are unique. */
export const Experiment = Norn.type({
  name: "tracking.Experiment", title: "Experiment", version: "0.1.0",
  properties: Schema.Struct({
    experiment_id: Schema.String,
    name: Schema.String,
    artifact_location: Schema.String,
    creation_time: Schema.Int,
    /** The experiment's tags as canonical JSON text: `[{"key","value"}]`. */
    tags: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["experiment_id"] }],
})

/** One run. Its status changes only through the lifecycle's transitions. */
export const Run = Norn.type({
  name: "tracking.Run", title: "Run", version: "0.1.0",
  properties: Schema.Struct({
    run_id: Schema.String,
    experiment_id: Schema.String,
    run_name: Schema.String,
    user_id: Schema.String,
    start_time: Schema.Int,
    artifact_uri: Schema.String,
    /** Tags sent with `runs/create`, as canonical JSON text. Later tags are in RunLog. */
    tags: Schema.String,
    status: Schema.Literals(["RUNNING", "FINISHED", "FAILED", "KILLED"]),
  }),
  defaults: { status: "RUNNING" },
  identity: [{ name: "primary", properties: ["run_id"] }],
  links: [{ id: "experiment", target: "tracking.Experiment", cardinality: { to: "one", from: "many" } }],
  lifecycles: [{
    property: "status",
    initial: ["RUNNING"],
    terminal: ["FINISHED", "FAILED", "KILLED"],
    transitions: [
      { id: "finish", from: ["RUNNING"], to: "FINISHED" },
      { id: "fail", from: ["RUNNING"], to: "FAILED" },
      { id: "kill", from: ["RUNNING"], to: "KILLED" },
    ],
  }],
})

/**
 * One logged batch of a run: the params, tags and metric points of one
 * `log-batch` (or one `log-param`, `set-tag`, `log-metric`, or the end time and
 * name of one `runs/update`), committed together. `seq` numbers a run's batches
 * without gaps, in commit order: batch `n` can only be created once batch
 * `n - 1` exists, and two writers cannot both create the same `n`.
 */
export const RunLog = Norn.type({
  name: "tracking.RunLog", title: "Run log", version: "0.1.0",
  properties: Schema.Struct({
    log_id: Schema.String,
    run_id: Schema.String,
    seq: Schema.Int,
    /** SHA-256 of the batch as the client sent it; a retried batch is found by it. */
    digest: Schema.String,
    /** Canonical JSON text: `[{"key","value"}]`. */
    params: Schema.String,
    /** Canonical JSON text: `[{"key","value"}]`. */
    tags: Schema.String,
    /**
     * Canonical JSON text: `[{"key","value","timestamp","step"}]`. `value` is
     * the float64 as its shortest round-trip decimal text, or `"NaN"`,
     * `"Infinity"`, `"-Infinity"`.
     */
    metrics: Schema.String,
    end_time: Schema.NullOr(Schema.Int),
    run_name: Schema.NullOr(Schema.String),
    /** The previous batch's id; null for batch 0. */
    prev_id: Schema.NullOr(Schema.String),
  }),
  // `position` is unique per run and number: two writers cannot both commit
  // batch n of one run, whatever ids they send.
  identity: [
    { name: "primary", properties: ["log_id"] },
    { name: "position", properties: ["run_id", "seq"] },
  ],
  // No link to the Run: creating a link costs time linear in the links the
  // target already has (measured; see README.md), and a run has one RunLog per
  // logged batch. `run_id` names the run instead.
})

export const CreateExperiment = Norn.action({
  id: "tracking.experiment.create", title: "Create experiment", version: "0.1.0",
  input: Schema.Struct({ experiment_id: Schema.String, name: Schema.String, artifact_location: Schema.String, creation_time: Schema.Int, tags: Schema.String }),
  output: Schema.Struct({ experiment_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Experiment, change: "created" }],
  effect: { kind: "upsert_input_object", type: Experiment, objectIdField: "experiment_id", writeMode: { kind: "create_only" } },
})

export const CreateRun = Norn.action({
  id: "tracking.run.create", title: "Create run", version: "0.1.0",
  input: Schema.Struct({
    run_id: Schema.String, experiment_id: Schema.String, run_name: Schema.String, user_id: Schema.String,
    start_time: Schema.Int, artifact_uri: Schema.String, tags: Schema.String,
  }),
  output: Schema.Struct({ run_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  // The experiment must exist and be readable by the caller.
  slice: [{ name: "experiment", type: Experiment, key: { property: { from: "input", property: "experiment_id" } } }],
  writes: [{ type: Run, change: "created" }],
  createLinks: [{ link: "experiment", targetType: Experiment, targetIdField: "experiment_id" }],
  effect: { kind: "upsert_input_object", type: Run, objectIdField: "run_id", writeMode: { kind: "create_only" } },
})

// A run's batches are numbered 0, 1, 2, ... in commit order. The identity
// (run_id, seq) makes each number unique per run; batch n > 0 requires batch
// n - 1 of the same run to exist. norn-zen/6 cannot turn the number into the id
// text, so `log_id` is the caller's: a writer sends `{run_id}/{seq}`, and a
// reader refuses a run whose chain is not at those ids.
export const AddFirstRunLog = Norn.action({
  id: "tracking.runlog.first", title: "Log a run's first batch", version: "0.1.0",
  input: Schema.Struct({
    run_id: Schema.String, seq: Schema.Int, log_id: Schema.String, digest: Schema.String,
    params: Schema.String, tags: Schema.String, metrics: Schema.String,
    end_time: Schema.optionalKey(Schema.Int), run_name: Schema.optionalKey(Schema.String),
  }),
  output: Schema.Struct({ log_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  slice: [{ name: "run", type: Run, key: { property: { from: "input", property: "run_id" } } }],
  preconditions: [Norn.unsafeZen<boolean>('input.seq == 0 and startsWith(input.log_id, input.run_id + "/")')],
  writes: [{ type: RunLog, change: "created" }],
  effect: { kind: "upsert_input_object", type: RunLog, objectIdField: "log_id", writeMode: { kind: "create_only" } },
})

export const AddRunLog = Norn.action({
  id: "tracking.runlog.add", title: "Log a batch to a run", version: "0.1.0",
  input: Schema.Struct({
    run_id: Schema.String, seq: Schema.Int, log_id: Schema.String, digest: Schema.String,
    params: Schema.String, tags: Schema.String, metrics: Schema.String,
    end_time: Schema.optionalKey(Schema.Int), run_name: Schema.optionalKey(Schema.String),
    prev_id: Schema.String,
  }),
  output: Schema.Struct({ log_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  slice: [
    { name: "run", type: Run, key: { property: { from: "input", property: "run_id" } } },
    { name: "prev", type: RunLog, key: { property: { from: "input", property: "prev_id" } } },
  ],
  preconditions: [Norn.unsafeZen<boolean>('slice.prev.run_id == input.run_id and slice.prev.seq == input.seq - 1 and startsWith(input.log_id, input.run_id + "/")')],
  writes: [{ type: RunLog, change: "created" }],
  effect: { kind: "upsert_input_object", type: RunLog, objectIdField: "log_id", writeMode: { kind: "create_only" } },
})

// Status changes, one action per declared transition.
export const FinishRun = Norn.action({
  id: "tracking.run.finish", title: "Finish run", version: "0.1.0",
  input: Schema.Struct({ run_id: Schema.String }),
  output: Schema.Struct({ run_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Run, change: "modified" }],
  effect: { kind: "fire_transition", type: Run, objectIdField: "run_id", lifecycle: "status", transition: "finish" },
})

export const FailRun = Norn.action({
  id: "tracking.run.fail", title: "Fail run", version: "0.1.0",
  input: Schema.Struct({ run_id: Schema.String }),
  output: Schema.Struct({ run_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Run, change: "modified" }],
  effect: { kind: "fire_transition", type: Run, objectIdField: "run_id", lifecycle: "status", transition: "fail" },
})

export const KillRun = Norn.action({
  id: "tracking.run.kill", title: "Kill run", version: "0.1.0",
  input: Schema.Struct({ run_id: Schema.String }),
  output: Schema.Struct({ run_id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Run, change: "modified" }],
  effect: { kind: "fire_transition", type: Run, objectIdField: "run_id", lifecycle: "status", transition: "kill" },
})
