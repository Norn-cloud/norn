# Tracking pack: MLflow-compatible experiment tracking

Norn definitions for experiment tracking, served to unchanged `mlflow` clients.
Point a client at a cell and it logs to Norn:

```sh
export MLFLOW_TRACKING_URI=http://127.0.0.1:PORT/v1/tracking/ORG/ENV
export MLFLOW_TRACKING_TOKEN="$(cat .norn/dev.token)"   # local development host only
```

The REST shapes follow MLflow's public tracking API documentation
(MLflow is Apache-2.0, © the MLflow project authors). No MLflow code or test
data is copied here. The acceptance run drives the real client,
`mlflow-skinny` 3.18.0.

## What it is

`tracking.ts` declares three object types and their actions:

- **Experiment**: id, name, artifact location, tags. The id is derived from
  the name, so names are unique. Experiment `0` ("Default") is created on
  first use.
- **Run**: id, experiment, name, user, start time, artifact URI, the tags sent
  with `runs/create`, and `status` as a lifecycle: created `RUNNING`, then one
  transition to `FINISHED`, `FAILED` or `KILLED`.
- **RunLog**: one logged batch. A `runs/log-batch`, `log-metric`,
  `log-parameter` or `set-tag`, or the end time and name of a `runs/update`,
  becomes one RunLog in **one commit**.

A run's batches are numbered 0, 1, 2, ... in commit order, and the cell
enforces the order:

- `(run_id, seq)` is a unique key, so two writers cannot both commit batch `n`
  of one run.
- Batch `n > 0` requires batch `n - 1` of the same run to exist.

The tracking routes (`crates/norn-cell-do/src/tracking.rs`) only translate
MLflow's wire format into calls to these actions and into authorized object
reads, as the verified caller. The server's policy decides every read and
write, exactly as for any other client.

## Supported subset

- **Experiments:** `create`, `get`, `get-by-name`, `search` (no filter, no
  `order_by`).
- **Runs:** `create`, `get`, `update`, `log-batch`, `log-metric`,
  `log-parameter`, `set-tag`, `search`.
- **Metrics:** `get-history`.

Anything else refuses in MLflow's error shape and names what is unsupported.
Nothing unimplemented returns an empty success.

- **Search filter:** clauses joined by `and`, each `attributes.status = '...'`
  or `attributes.run_name = '...'`. Metric, param and tag filters refuse with
  `INVALID_PARAMETER_VALUE`.
- **Order:** `attributes.start_time` only.
- **View type:** `ACTIVE_ONLY` or `ALL`; nothing can be deleted, so
  `DELETED_ONLY` refuses.
- **Pagination:** a search returns at most 200 per page and a
  `next_page_token`. `mlflow.search_runs` and `MlflowClient.search_runs`
  follow it. A filtered or sorted search scans at most 10,000 visible runs in
  the cell.
- **Batch limits:** MLflow's per-request limits: 1,000 metrics, 100 params,
  100 tags, 1,000 entities in total. Keys are at most 250 characters, param
  values at most 6,000, tag values at most 8,000. There is no per-run cap.
- **Params:** a param cannot change value. A changed value refuses with
  `INVALID_PARAMETER_VALUE` when the request can see the earlier value. Two
  concurrent writers can both be acknowledged; the earlier batch wins on every
  read, and the run carries a `norn.param_conflicts` tag naming the ignored
  values.
- **Tags:** the latest batch wins.
- **Status:** a `RUNNING` run may become `FINISHED`, `FAILED` or `KILLED`.
  An update to the status it already has succeeds without a commit. Any other
  change refuses. Runs are always created `RUNNING`.
- **`runs/update` is two commits:** the end time and name (a RunLog), then the
  status transition. A transition writes only its lifecycle property, so the
  two cannot be one commit yet; a transition with property writes is a
  follow-up on #1171. Each commit has its own key, so a retry after a crash
  between them completes the update. Until then the run stays `RUNNING`, and
  `end_time` is shown only once the run is terminal.
- **Artifacts:** `artifact_uri` is an external location (an experiment's
  `artifact_location`, or `s3://norn-tracking-artifacts/{experiment_id}`). The
  client uploads there directly; no bytes pass through the cell.
  `artifacts/list` refuses as unsupported: registering artifact digests is a
  later unit.
- **Auth:** the client sends `MLFLOW_TRACKING_TOKEN` as a bearer token. On
  the local development host that is the session token. A published host's
  tokens come from its identity service and live at most five minutes, so
  using one from `mlflow` needs an MLflow auth plugin (not built yet). The
  server decides scope from the verified token; a token for one organization
  cannot log under another's prefix.

## What this costs

- **Not typed objects.** Params, tags and metric points live inside RunLog as
  canonical JSON text, so they cannot appear in typed questions or laws yet.
  The route to typed Param, Tag and Metric objects is a bounded multi-object
  effect (#1171).
- **Metric values** keep float64 exactly: the shortest round-trip decimal
  text, or `NaN`, `Infinity`, `-Infinity`. JSON cannot carry those three, and
  the kernel has no float type (an open question on #1171).
- **Reads fold.** `runs/get` and `metrics/get-history` read every batch of the
  run. A batch that is not at its canonical id fails the read loudly, naming
  the run and the number, never a partial run. A record forged at a run's
  *next* position by a direct caller of the generic action route is not on
  the chain until then; the next write to that run fails loudly.
- **No RunLog→Run link.** Creating a link costs time linear in the links its
  target already has. Measured on one Boat VM (4 vCPU), release build,
  sequential one-point batches over HTTP, median per 1,000 commits:

  | RunLogs in one run | with a link to the Run | without |
  |---|---|---|
  | 1,000 | 16.0 ms | 10.9 ms |
  | 10,000 | 57.3 ms | 10.1 ms |
  | 100,000 | not run (≈0.5 s by the trend) | 10.6 ms |

  So RunLog has no link and `run_id` names its run. Studio's links view shows
  no RunLog edges. The Run→Experiment link stays: a run is created once.
- **Batch your logging.** Each commit writes about 15–16.5 KB to the cell,
  so 100,000 one-point batches take about 1.5 GB. Use `log_batch` (up to
  1,000 points per commit), or log several metrics per step. Per-step fluent
  `log_metric` works but costs a commit, and its disk, per call. Metrics stay
  in the cell for now, because the analytics engine has no principals yet.
  Above roughly 10^5 batches per cell, the disk and fold cost say: move
  points to the analytics engine.

## Acceptance

`acceptance/run.sh WORKDIR` starts `norn dev` on a project with this pack and
runs `acceptance/check_tracking.py`. It drives the real client:

- a fluent run with params, metrics (including NaN and both infinities) and
  tags, ended and searched;
- the cell's facts behind it;
- pagination past 200 runs;
- refusals: unauthenticated, another organization's prefix, oversized
  batches, a changed param, unsupported filters and endpoints;
- status rules, and `runs/update` retried;
- two concurrent writers on one run;
- replayed and concurrently replayed batches;
- forged orders through the generic action route;
- a batch off its canonical id failing the read loudly.
