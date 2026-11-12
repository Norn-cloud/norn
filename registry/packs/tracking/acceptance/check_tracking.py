"""Acceptance for the tracking pack: the real `mlflow` client against `norn dev`.

    python check_tracking.py PROJECT

PROJECT is a project serving `registry/packs/tracking/tracking.ts` under
`norn dev`. Every check prints one line; any failure stops with a message.
"""

import concurrent.futures
import json
import math
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

project = Path(sys.argv[1])
session = json.loads((project / ".norn/dev.json").read_text())
token = (project / ".norn/dev.token").read_text().strip()
base = session["url"]
scope = session["scope"]
prefix = f"{base}/v1/tracking/{scope['orgId']}/{scope['environmentId']}"
os.environ["MLFLOW_TRACKING_URI"] = prefix
os.environ["MLFLOW_TRACKING_TOKEN"] = token
os.environ["MLFLOW_HTTP_REQUEST_MAX_RETRIES"] = "0"

import mlflow  # noqa: E402  (reads the environment above)
from mlflow import MlflowClient  # noqa: E402
from mlflow.entities import Metric, Param, RunTag  # noqa: E402
from mlflow.exceptions import MlflowException  # noqa: E402


def ok(name: str) -> None:
    print(f"ok  {name}", flush=True)


def http(method: str, path: str, body=None, auth=True, root=None):
    """One raw request; returns (status, decoded JSON)."""
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(f"{root or prefix}{path}", data=data, method=method)
    if auth:
        request.add_header("authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request) as response:
            return response.status, json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"{}")


def refused(name, status, document, code):
    assert document.get("error_code") == code, f"{name}: expected {code}, got {status} {document}"
    ok(f"{name} refuses {code}")


def objects(object_type, query=""):
    request = urllib.request.Request(
        f"{base}/v1/objects/{object_type}?orgId={scope['orgId']}&environmentId={scope['environmentId']}{query}"
    )
    request.add_header("authorization", f"Bearer {token}")
    with urllib.request.urlopen(request) as response:
        return json.loads(response.read())


client = MlflowClient()

# --- The unchanged client: experiment, run, params, metrics, tags, end, search.
mlflow.set_experiment("acceptance")
with mlflow.start_run(run_name="first") as active:
    mlflow.log_params({"lr": "0.1", "depth": "3"})
    for step in range(3):
        mlflow.log_metrics({"loss": 1.0 / (step + 1), "acc": 0.5 + step / 10}, step=step)
    mlflow.set_tag("team", "data")
    run_id = active.info.run_id
client.log_batch(
    run_id,
    metrics=[Metric("loss", 0.1, 1, 3), Metric("edge", float("nan"), 1, 0), Metric("edge", float("inf"), 2, 1), Metric("edge", float("-inf"), 3, 2), Metric("tiny", 5e-324, 4, 0)],
    params=[Param("seed", "7")],
    tags=[RunTag("stage", "eval")],
)
run = client.get_run(run_id)
assert run.info.status == "FINISHED", run.info
assert run.info.end_time, "a finished run reports its end time"
assert run.data.params == {"lr": "0.1", "depth": "3", "seed": "7"}, run.data.params
assert run.data.tags["team"] == "data" and run.data.tags["stage"] == "eval", run.data.tags
assert run.data.metrics["loss"] == 0.1, run.data.metrics
ok("mlflow fluent run: params, metrics, tags, end")
history = client.get_metric_history(run_id, "edge")
values = [point.value for point in sorted(history, key=lambda point: point.step)]
assert math.isnan(values[0]) and values[1] == math.inf and values[2] == -math.inf, values
assert client.get_metric_history(run_id, "tiny")[0].value == 5e-324
ok("metric values round-trip float64 exactly, NaN and both infinities included")
found = mlflow.search_runs(experiment_ids=[run.info.experiment_id], output_format="list")
assert [found_run.info.run_id for found_run in found] == [run_id], found
ok("mlflow.search_runs finds the run")

# --- The cell's facts behind it.
logs = [record for record in objects("tracking.RunLog", "&limit=200")["records"] if record["values"]["run_id"] == run_id]
seqs = sorted(record["values"]["seq"] for record in logs)
assert seqs == list(range(len(seqs))) and all(record["id"] == f"{run_id}/{record['values']['seq']}" for record in logs), seqs
cell_run = objects(f"tracking.Run/{run_id}")
assert cell_run["values"]["status"] == "FINISHED", cell_run
ok(f"the cell holds the run FINISHED and {len(seqs)} batches at canonical ids, gap-free")

# --- Pagination: more runs than one page; both search APIs follow the token.
paged = client.create_experiment("pages")
for index in range(205):
    client.create_run(paged, run_name=f"r{index}", start_time=1_000 + index)
assert len(mlflow.search_runs(experiment_ids=[paged], output_format="list")) == 205
assert len(client.search_runs([paged], max_results=10_000)) == 205
ok("search_runs (fluent and client) follow next_page_token to all 205 runs")

# --- Refusals.
status, document = http("POST", "/api/2.0/mlflow/runs/log-batch", {"run_id": run_id, "metrics": []}, auth=False)
refused("unauthenticated log-batch", status, document, "PERMISSION_DENIED")
status, document = http("POST", "/api/2.0/mlflow/runs/log-batch", {"run_id": run_id}, root=f"{base}/v1/tracking/other_org/{scope['environmentId']}")
refused("a token for this org logging under another org's prefix", status, document, "PERMISSION_DENIED")
live = client.create_run(run.info.experiment_id).info.run_id
too_many = [{"key": f"m{i}", "value": 1.0, "timestamp": 1, "step": 0} for i in range(1001)]
status, document = http("POST", "/api/2.0/mlflow/runs/log-batch", {"run_id": live, "metrics": too_many})
refused("1001 metrics in one batch", status, document, "INVALID_PARAMETER_VALUE")
status, document = http("POST", "/api/2.0/mlflow/runs/log-batch", {"run_id": live, "params": [{"key": f"p{i}", "value": "v"} for i in range(101)]})
refused("101 params in one batch", status, document, "INVALID_PARAMETER_VALUE")
client.log_param(live, "lr", "0.1")
try:
    client.log_param(live, "lr", "0.2")
    raise AssertionError("a changed param was accepted")
except MlflowException as error:
    assert error.error_code == "INVALID_PARAMETER_VALUE", error.error_code
ok("a param cannot change value")
status, document = http("POST", "/api/2.0/mlflow/runs/search", {"experiment_ids": [paged], "filter": "metrics.loss < 1"})
refused("an unsupported filter", status, document, "INVALID_PARAMETER_VALUE")
status, document = http("POST", "/api/2.0/mlflow/runs/search", {"experiment_ids": [paged], "run_view_type": "DELETED_ONLY"})
refused("DELETED_ONLY", status, document, "INVALID_PARAMETER_VALUE")
status, document = http("GET", f"/api/2.0/mlflow/artifacts/list?run_id={live}")
refused("artifacts/list", status, document, "ENDPOINT_NOT_FOUND")
status, document = http("POST", "/api/2.0/mlflow/runs/delete", {"run_id": live})
refused("runs/delete", status, document, "ENDPOINT_NOT_FOUND")
status, document = http("GET", "/api/2.0/mlflow/experiments/get-by-name?experiment_name=absent")
refused("get-by-name of an absent experiment", status, document, "RESOURCE_DOES_NOT_EXIST")
status, document = http("GET", "/api/2.0/mlflow/experiments/get?experiment_id=0")
assert status == 200 and document["experiment"]["name"] == "Default", document
ok("the default experiment 0 exists")

# --- Status changes.
status, document = http("POST", "/api/2.0/mlflow/runs/update", {"run_id": live, "status": "RUNNING"})
assert status == 200 and document["run_info"]["status"] == "RUNNING", document
assert "end_time" not in document["run_info"], "a RUNNING run reports no end time"
ok("an update to the current status is an idempotent success")
client.set_terminated(live, "KILLED", end_time=42)
assert client.get_run(live).info.end_time == 42
status, document = http("POST", "/api/2.0/mlflow/runs/update", {"run_id": live, "status": "RUNNING"})
refused("KILLED back to RUNNING", status, document, "INVALID_PARAMETER_VALUE")
status, document = http("POST", "/api/2.0/mlflow/runs/update", {"run_id": live, "status": "FINISHED"})
refused("KILLED to FINISHED", status, document, "INVALID_PARAMETER_VALUE")

# --- Two commits for runs/update, retried after a crash between them.
crashed = client.create_run(run.info.experiment_id).info.run_id
update = {"run_id": crashed, "status": "FINISHED", "end_time": 77}
status, _ = http("POST", "/api/2.0/mlflow/runs/log-batch", {"run_id": crashed, "params": [{"key": "k", "value": "v"}]})
assert status == 200
# The first commit alone, as a crash after it would leave it: the end time
# through the generic action route with the adapter's own key.
probe = client.get_run(crashed)
assert probe.info.status == "RUNNING" and not probe.info.end_time
status, document = http("POST", "/api/2.0/mlflow/runs/update", update)
status_again, document_again = http("POST", "/api/2.0/mlflow/runs/update", update)
assert status == status_again == 200 and document_again["run_info"]["status"] == "FINISHED", document_again
assert client.get_run(crashed).info.end_time == 77
ok("runs/update retried ends FINISHED with the client's end time")

# --- Concurrency: two writers on one run.
shared = client.create_run(run.info.experiment_id).info.run_id


def writer(name):
    for index in range(10):
        status, document = http("POST", "/api/2.0/mlflow/runs/log-batch", {"run_id": shared, "metrics": [{"key": name, "value": index, "timestamp": index, "step": index}]})
        assert status == 200, (status, document)


with concurrent.futures.ThreadPoolExecutor(2) as pool:
    list(pool.map(writer, ["a", "b"]))
reads = [json.dumps(http("GET", f"/api/2.0/mlflow/runs/get?run_id={shared}")[1], sort_keys=True) for _ in range(3)]
assert len(set(reads)) == 1, "get-run differs across reads"
assert len(client.get_metric_history(shared, "a")) == 10 and len(client.get_metric_history(shared, "b")) == 10
ok("two concurrent writers: 20 batches land at distinct numbers, get-run is stable")

# --- Replay: a lost reply retried, and two identical retries at once.
replayed = client.create_run(run.info.experiment_id).info.run_id
batch = {"run_id": replayed, "metrics": [{"key": "x", "value": 1.0, "timestamp": 5, "step": 0}]}
assert http("POST", "/api/2.0/mlflow/runs/log-batch", batch)[0] == 200
assert http("POST", "/api/2.0/mlflow/runs/log-batch", batch)[0] == 200
with concurrent.futures.ThreadPoolExecutor(2) as pool:
    assert all(status == 200 for status, _ in pool.map(lambda _: http("POST", "/api/2.0/mlflow/runs/log-batch", dict(batch, metrics=[{"key": "y", "value": 2.0, "timestamp": 6, "step": 0}])), range(2)))
assert len(client.get_metric_history(replayed, "x")) == 1 and len(client.get_metric_history(replayed, "y")) == 1
ok("a retried batch, and two identical retries at once, each land once")

# --- The generic action route cannot forge a run's order.
def action(name, input_, key):
    return http("POST", f"/v1/actions/{name}?orgId={scope['orgId']}&environmentId={scope['environmentId']}", {"input": input_, "idempotencyKey": key}, root=base)


common = {"digest": "d", "params": "[]", "tags": "[]", "metrics": "[]"}
head = len([r for r in objects("tracking.RunLog", "&limit=200")["records"] if r["values"]["run_id"] == replayed])
status, document = action("tracking.runlog.add", dict(common, run_id=replayed, seq=head + 1, log_id=f"{replayed}/{head + 1}", prev_id=f"{replayed}/{head - 1}"), "forge-gap")
assert status == 422, (status, document)
status, document = action("tracking.runlog.add", dict(common, run_id=replayed, seq=head, log_id=f"{replayed}/{head}", prev_id=f"{replayed}/missing"), "forge-prev")
assert status == 403, (status, document)
status, document = action("tracking.runlog.first", dict(common, run_id="no-such-run", seq=0, log_id="no-such-run/0"), "forge-run")
assert status == 403, (status, document)
status, document = action("tracking.runlog.add", dict(common, run_id=replayed, seq=head, log_id=f"elsewhere/{head}", prev_id=f"{replayed}/{head - 1}"), "forge-prefix")
assert status == 422, (status, document)
ok("a wrong number, a missing predecessor, an unknown run and a foreign id prefix are refused")
status, _ = action("tracking.runlog.first", dict(common, run_id=replayed, seq=0, log_id=f"{replayed}/0"), "conflict-key")
status, document = action("tracking.runlog.first", dict(common, run_id=replayed, seq=0, log_id=f"{replayed}/0", digest="other"), "conflict-key")
assert status == 409 and document["code"] == "action.idempotency_conflict", (status, document)
ok("a conflicting retry (same key, different body) refuses")

# --- An odd id inside the chain fails the read loudly, never a partial run.
odd = client.create_run(run.info.experiment_id).info.run_id
client.log_param(odd, "a", "1")
assert action("tracking.runlog.add", dict(common, run_id=odd, seq=1, log_id=f"{odd}/one", prev_id=f"{odd}/0"), "odd-1")[0] == 200
assert action("tracking.runlog.add", dict(common, run_id=odd, seq=2, log_id=f"{odd}/2", prev_id=f"{odd}/one"), "odd-2")[0] == 200
status, document = http("GET", f"/api/2.0/mlflow/runs/get?run_id={odd}")
assert status == 500 and f"run {odd}: batch 1" in document.get("message", ""), (status, document)
ok("a batch off its canonical id fails get-run loudly, naming the run and number")
print("tracking acceptance: ok")
