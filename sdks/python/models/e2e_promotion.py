"""Model promotion end to end on a dev host (#1171 M2/M3): registered versions,
the promotion law, and the approval gate on promote.

    python e2e_promotion.py /path/to/project
"""
import json
import sys

from norn_regression import Cell
from norn_regression.cell import Refused

cell = Cell.from_project(sys.argv[1])
http = cell.client.get_httpx_client()


def call(action: str, values: dict, key: str):
    """The answer's status and body, without raising on a refusal."""
    answer = http.post(f"/v1/actions/{action}", params={"orgId": cell.org_id, "environmentId": cell.environment_id},
                       json={"input": values, "idempotencyKey": key})
    return answer.status_code, answer.json()


def ok(action: str, values: dict, key: str) -> dict:
    status, body = call(action, values, key)
    assert status == 200, (action, status, body)
    return body


def version(version_id: str) -> dict:
    return cell.rows("ml.ModelVersion").set_index("version_id").loc[version_id].to_dict()


ok("tracking.experiment.create", {"experiment_id": "exp-churn", "name": "churn", "artifact_location": "s3://models/churn",
                                  "creation_time": 1, "tags": "[]"}, "exp")
for run in ("train-1", "eval-1", "running-1"):
    ok("tracking.run.create", {"run_id": run, "experiment_id": "exp-churn", "run_name": run, "user_id": "ds",
                               "start_time": 2, "artifact_uri": f"s3://models/churn/{run}", "tags": "[]"}, run)
for run in ("train-1", "eval-1"):
    ok("tracking.run.finish", {"run_id": run}, f"finish-{run}")

base = {"model_id": "sales.churn_model", "training_run": "train-1", "evaluation_run": "eval-1",
        "artifact_uri": "s3://models/churn/train-1/model.pkl", "artifact_sha256": "ab" * 32, "artifact_size": 1024,
        "input_type": "sales.Customer", "input_revision": "c41", "pinned_dataset_revision": "c42",
        "metric_name": "auc", "metric_value": "0.91", "contracts_passed": True}

# A version is registered only from finished runs.
status, body = call("ml.modelversion.register", {**base, "version_id": "churn@0", "version": "0", "evaluation_run": "running-1",
                                                 "evaluation_revision": "c42"}, "reg-0")
assert status != 200, ("registered from a running evaluation", body)

# Evaluated on another revision than the pinned one: registered as a candidate, never promoted.
ok("ml.modelversion.register", {**base, "version_id": "churn@1", "version": "1", "evaluation_revision": "c40"}, "reg-1")
status, body = call("ml.modelversion.promote", {"version_id": "churn@1"}, "promote-1")
assert status != 200 and version("churn@1")["status"] == "candidate", (status, body)
refused_code = body.get("code")

# A failed contract: never promoted either.
ok("ml.modelversion.register", {**base, "version_id": "churn@2", "version": "2", "evaluation_revision": "c42",
                                "contracts_passed": False}, "reg-2")
status, _ = call("ml.modelversion.promote", {"version_id": "churn@2"}, "promote-2")
assert status != 200 and version("churn@2")["status"] == "candidate"

# Evaluated on the pinned revision with every contract held: promoted, then retired.
ok("ml.modelversion.register", {**base, "version_id": "churn@3", "version": "3", "evaluation_revision": "c42"}, "reg-3")
ok("ml.modelversion.promote", {"version_id": "churn@3"}, "promote-3")
assert version("churn@3")["status"] == "production"
links = http.get("/v1/objects/ml.ModelVersion/churn@3/links", params={"orgId": cell.org_id, "environmentId": cell.environment_id}).json()
assert {"trained_by", "evaluated_by"} <= {item.get("link") for item in links["items"]}, links

# The approval gate (APPROVAL-1): with it installed, a promote parks for an admin or owner to decide.
definition = json.load(open(sys.argv[2]))
admitted = http.post("/v1/definitions:admit", params={"orgId": cell.org_id, "environmentId": cell.environment_id}, json=definition)
assert admitted.status_code == 200 and admitted.json()["admitted"], admitted.text
installed = http.post("/v1/definitions:install", params={"orgId": cell.org_id, "environmentId": cell.environment_id},
                      json={**definition, "signoff": {"restatementSha256": admitted.json()["restatementSha256"]}})
assert installed.status_code == 200, installed.text
ok("ml.modelversion.register", {**base, "version_id": "churn@4", "version": "4", "evaluation_revision": "c42"}, "reg-4")
status, body = call("ml.modelversion.promote", {"version_id": "churn@4"}, "promote-4")
assert status == 202 and body.get("outcome") == "pending" and body["decision"]["id"], (status, body)
assert version("churn@4")["status"] == "candidate", "a parked promotion commits nothing"
print(f"model promotion: ok (unevaluated promote refused: {refused_code}; promoted, retired-ready, links; gated promote parked as {body['decision']['id']})")
