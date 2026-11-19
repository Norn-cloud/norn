"""Applying a model end to end on a dev host (#1171 M4): typed predictions with
lineage to the model version, refused before any write when a contract fails.

    python e2e_apply.py /path/to/project

Runs after e2e_promotion.py on the same project (it reuses that script's
finished runs), with the demo pack in demo/ served beside the tracking and ml
packs.
"""
import math
import sys
from pathlib import Path

import pandas as pd

sys.path.insert(0, str(Path(__file__).resolve().parent))
from norn_models import PredictionRefused, apply_model  # noqa: E402
from norn_regression import Cell  # noqa: E402
from norn_regression.cell import Refused  # noqa: E402

cell = Cell.from_project(sys.argv[1])
http = cell.client.get_httpx_client()
where = {"orgId": cell.org_id, "environmentId": cell.environment_id}
MODEL = "sales.churn_model"


def count(object_type: str) -> int:
    return len(cell.rows(object_type))


def links(object_type: str, object_id: str) -> dict:
    answer = http.get(f"/v1/objects/{object_type}/{object_id}/links", params=where)
    assert answer.status_code == 200, (object_type, object_id, answer.status_code, answer.text)
    return {item.get("link"): item for item in answer.json()["items"]}


customers = [("c-01", 2, "19.00", "basic"), ("c-02", 5, "29.00", "basic"), ("c-03", 11, "49.00", "pro"),
             ("c-04", 18, "49.00", "pro"), ("c-05", 30, "99.00", "pro"), ("c-06", 48, "19.00", "basic")]
for customer_id, tenure, spend, plan in customers:
    cell.call("sales.customer.record", {"customer_id": customer_id, "tenure_months": tenure, "monthly_spend": spend,
                                        "plan": plan, "churned": tenure < 6}, customer_id)

# Two versions of the churn model from the runs e2e_promotion.py finished: one promoted, one left a candidate.
base = {"model_id": MODEL, "training_run": "train-1", "evaluation_run": "eval-1",
        "artifact_uri": "s3://models/churn/train-1/model.pkl", "artifact_sha256": "cd" * 32, "artifact_size": 2048,
        "input_type": "sales.Customer", "input_revision": "c41", "pinned_dataset_revision": "c42",
        "evaluation_revision": "c42", "metric_name": "auc", "metric_value": "0.88", "contracts_passed": True}
cell.call("ml.modelversion.register", {**base, "version_id": "churn@10", "version": "10"}, "apply-reg-10")
cell.call("ml.modelversion.promote", {"version_id": "churn@10"}, "apply-promote-10")
cell.call("ml.modelversion.register", {**base, "version_id": "churn@11", "version": "11"}, "apply-reg-11")


def logistic(features: pd.DataFrame) -> pd.DataFrame:
    """Deterministic: churn risk falls with tenure."""
    probability = [1 / (1 + math.exp(0.2 * (int(tenure) - 12))) for tenure in features["tenure_months"]]
    band = ["high" if p >= 0.6 else "medium" if p >= 0.3 else "low" for p in probability]
    return pd.DataFrame({"churn_probability": probability, "risk_band": band})


def overconfident(features: pd.DataFrame) -> pd.DataFrame:
    """One probability above 1: the `probability` contract refuses the batch."""
    outputs = logistic(features)
    outputs.loc[0, "churn_probability"] = 1.5
    return outputs


# A promoted version: every prediction written, typed, and linked to its customer and its version.
applied = apply_model(cell, MODEL, "churn@10", logistic, batch_id="b-1")
assert applied.rows == len(customers), applied
predictions = cell.rows("sales.ChurnPrediction").set_index("prediction_id")
assert len(predictions) == len(customers), predictions
assert set(predictions["subject"]) == {customer_id for customer_id, *_ in customers}
assert set(predictions["model_version"]) == {"churn@10"}
first = predictions.loc["c-01@b-1"]
assert first["risk_band"] == "high" and str(first["churn_probability"]) == "0.8808", first.to_dict()
assert {"scored", "predicted_by"} <= set(links("sales.ChurnPrediction", "c-01@b-1"))
batch = cell.rows("ml.PredictionBatch").set_index("batch_id").loc["b-1"]
assert int(batch["rows"]) == len(customers) and batch["model_version"] == "churn@10", batch.to_dict()
assert batch["input_type"] == "sales.Customer" and batch["input_revision"] == applied.input_revision
assert "applied" in links("ml.PredictionBatch", "b-1")

# A contract violation refuses the whole batch before any write.
try:
    apply_model(cell, MODEL, "churn@10", overconfident, batch_id="b-2")
    raise AssertionError("an out-of-range probability was written")
except PredictionRefused as refused:
    assert refused.contract == "probability" and refused.value == 1.5, refused
    violation = str(refused)
assert count("sales.ChurnPrediction") == len(customers) and count("ml.PredictionBatch") == 1

# A candidate version is never applied: the batch record's precondition refuses it, and nothing is written.
try:
    apply_model(cell, MODEL, "churn@11", logistic, batch_id="b-3")
    raise AssertionError("a candidate version was applied")
except Refused as refused:
    candidate = str(refused)
    assert "ml.predictionbatch.record" in candidate, candidate
assert count("sales.ChurnPrediction") == len(customers) and count("ml.PredictionBatch") == 1

print(f"model apply: ok ({applied.rows} predictions linked to churn@10 and their customers, batch b-1 recorded; "
      f"refused before any write: {violation}; candidate refused: {candidate[:160]})")
