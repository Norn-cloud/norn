"""Model promotion end to end on a dev host (#1171 M2/M3): registered versions
and the promotion law.

    python e2e_promotion.py /path/to/project
"""
import sys
from pathlib import Path

from norn_regression import Cell

cell = Cell.from_project(sys.argv[1])
http = cell.client.get_httpx_client()


def call(action: str, values: dict, key: str):
    """The answer's status and body, without raising on a refusal."""
    answer = http.post(f"/v1/actions/{action}", params={"orgId": cell.org_id, "environmentId": cell.environment_id},
                       json={"input": values, "idempotencyKey": key})
    return answer.status_code, answer.json()


# DEV-REVIEWER-1: the dev host's second local principal, with the developer's grants.
REVIEWER = {"Authorization": "Bearer " + (Path(sys.argv[1]) / ".norn/dev.reviewer.token").read_text().strip()}


def call_as_reviewer(action: str, values: dict, key: str):
    """Call as the reviewer: separation of duties needs a principal other than the registrant."""
    answer = http.post(f"/v1/actions/{action}", params={"orgId": cell.org_id, "environmentId": cell.environment_id},
                       json={"input": values, "idempotencyKey": key}, headers=REVIEWER)
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
        "input_type": "sales.Customer", "input_revision": "c41", "pinned_dataset_revision": "c42"}

# A version is registered only from finished runs.
status, body = call("ml.modelversion.register", {**base, "version_id": "churn@0", "version": "0", "evaluation_run": "running-1"}, "reg-0")
assert status != 200, ("registered from a running evaluation", body)

# Caller-supplied evaluation metrics / booleans are refused at registration (no self-attestation).
status, body = call("ml.modelversion.register", {**base, "version_id": "churn@forged", "version": "forged",
                                                 "evaluation_revision": "c42", "contracts_passed": True}, "reg-forged")
assert status != 200, ("caller self-attested evaluation evidence on register", body)

# Registered as a candidate: promoting without an authenticated evaluation receipt is refused.
ok("ml.modelversion.register", {**base, "version_id": "churn@1", "version": "1"}, "reg-1")
status, body = call("ml.modelversion.promote", {"version_id": "churn@1"}, "promote-1")
assert status != 200 and version("churn@1")["status"] == "candidate", (status, body)
refused_code = body.get("code")

# An unretained/forged evaluation receipt digest is refused by ml.modelversion.accept_evaluation.
status, body = call("ml.modelversion.accept_evaluation", {"version_id": "churn@1", "evaluation_receipt": "sha256:" + "a" * 64}, "accept-forged")
assert status != 200 and version("churn@1")["status"] == "candidate", (status, body)

# Host-authenticated receipt intake (`SqliteCell::retain_function_receipt`) binds
# evaluation_revision, metric_name, metric_value, and contracts_passed from a
# verified `ml.forecast.evaluate` FunctionReceipt before promotion. See
# `crates/norn-cell/tests/supply_lab_stress.rs` (axis 6) for the end-to-end
# durable cell receipt intake, replay refusal, SoD, and promotion flow.
print(f"model promotion: ok (registration needs finished runs and refuses caller-supplied evaluation evidence; "
      f"unevaluated promotion and forged evaluation receipt refused: {refused_code})")
