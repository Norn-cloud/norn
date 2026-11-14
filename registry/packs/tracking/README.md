# Tracking pack: experiment runs as Norn definitions

The data model for experiment tracking in the shape of MLflow's public
tracking API (MLflow is Apache-2.0, © the MLflow project authors; no MLflow
code or test data is copied here). Every write is one of the actions in
`tracking.ts`, called through the generic governed action route.

This pack is the model only. The MLflow wire adapter that serves unchanged
`mlflow` clients is not part of it yet.

## What it is

- **Experiment**: id, name, artifact location, tags. Names are unique: the
  id is derived from the name.
- **Run**: id, experiment, name, user, start time, artifact URI, the tags sent
  at creation, and `status` as a lifecycle: created `RUNNING`, then one
  transition to `FINISHED`, `FAILED` or `KILLED`. Another definition that
  needs a finished run reads `tracking.Run` by `run_id` and checks `status`.
- **RunLog**: one logged batch (params, tags and metric points, or a run's end
  time and name), written in one commit.

A run's batches are numbered 0, 1, 2, ... in commit order, and the cell
enforces the order: `(run_id, seq)` is a unique key, so two writers cannot
both commit batch `n` of one run, and batch `n > 0` requires batch `n - 1` of
the same run to exist.

## What it does not do yet

- **Not typed values.** Params, tags and metric points live inside RunLog as
  canonical JSON text, so typed questions and laws cannot read them. A
  definition that needs a metric in a law (a promotion threshold, say)
  records it as a typed field of its own (#1171).
- **Metric values** keep float64 exactly: the shortest round-trip decimal
  text, or `NaN`, `Infinity`, `-Infinity`.
- **Artifacts** are one root URI per run (`artifact_uri`); per-file URIs and
  digests are not modelled.
- **No RunLog→Run link.** Creating a link costs time linear in the links its
  target already has (measured on #1123), and a run has one RunLog per
  batch, so `run_id` names the run instead. The Run→Experiment link stays.
