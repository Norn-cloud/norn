"""Apply a promoted model version to a cell's objects (#1171 M4).

A model is a `Norn.model` definition (ModelIR): its input object type, the
features it reads, its typed outputs, the contracts every output must meet,
and the typed object type its predictions are written to. Its internals stay
opaque: the caller passes `predict`, a function from a frame of feature
columns to a frame of output columns.

`apply_model` reads the definition and the input rows from the cell, runs
`predict`, and checks every output value against its declared type and every
declared contract on every row before writing anything. One violation refuses
the whole batch and nothing is written. Otherwise it records the batch
(`ml.predictionbatch.record`, which the cell admits only for a version in
production and links to that version), then one prediction object per input
row through the predictions type's record action.

Conventions:

- The predictions type's record action is `<namespace>.<lowercased type
  name>.record` (`sales.ChurnPrediction` -> `sales.churnprediction.record`)
  unless `record_action` names another. It takes `prediction_id`, `subject`
  (the scored object's id), `model_version` (the `ml.ModelVersion` id) and one
  input per output.
- A prediction's id is `<subject>@<batch_id>`, which is also its idempotency
  key, and the batch's key is its id: rerunning a batch replays the same calls.
- A missing value is `None` or `pandas.NA`; a float `NaN` is a value that is
  not finite. A decimal output is written at its declared scale, rounded half
  to even; contracts are checked on the value written.
"""

from __future__ import annotations

import math
import numbers
from dataclasses import dataclass, field
from decimal import ROUND_HALF_EVEN, Decimal, InvalidOperation
from typing import Any, Callable

import pandas as pd

from norn_regression.cell import Cell, Refused, revision

__all__ = ["Applied", "PredictionRefused", "Violation", "apply_model", "check_outputs", "model_definition"]

BATCH_ACTION = "ml.predictionbatch.record"


@dataclass(frozen=True)
class Violation:
    """One output value that may not be written. `contract` is the declared contract's id,
    or None when the value does not fit the output's declared type or nullability."""

    contract: str | None
    output: str
    row: str
    value: Any
    reason: str

    def __str__(self) -> str:
        what = f"contract {self.contract!r}" if self.contract else "the output's type"
        return f"row {self.row!r}: {self.output} = {self.value!r} violates {what} ({self.reason})"


class PredictionRefused(Exception):
    """The batch was refused before any write: at least one output value violates its type or a contract."""

    def __init__(self, violations: list[Violation]):
        self.violations = violations
        first = violations[0]
        self.contract, self.row, self.value = first.contract, first.row, first.value
        more = f" (and {len(violations) - 1} more)" if len(violations) > 1 else ""
        super().__init__(f"nothing written: {first}{more}")


@dataclass
class Applied:
    batch_id: str
    model_version: str
    rows: int
    input_revision: str
    receipts: list[str] = field(default_factory=list)


def model_definition(cell: Cell, model_id: str) -> dict:
    """The admitted `model` artifact's payload for `model_id`, from the cell's definitions catalog."""
    answer = cell.client.get_httpx_client().get(
        "/v1/definitions", params={"orgId": cell.org_id, "environmentId": cell.environment_id}
    )
    if answer.status_code != 200:
        raise Refused(f"reading definitions ({answer.status_code}): {answer.text}")
    for artifact in answer.json().get("artifacts", []):
        payload = artifact.get("payload") or {}
        if artifact.get("kind") == "model" and payload.get("modelId") == model_id:
            return payload
    raise LookupError(f"the cell has no admitted model {model_id!r}")


def _missing(value: Any) -> bool:
    return value is None or value is pd.NA or value is pd.NaT


def _number(value: Any) -> bool:
    return isinstance(value, numbers.Real) and not isinstance(value, bool)


def _written(output: dict, value: Any) -> tuple[Any, str | None]:
    """The wire value for a non-missing `value` of `output`, or why it does not fit the type."""
    kind = output["type"]["kind"]
    if kind == "decimal":
        if not _number(value):
            return None, "not a number"
        if not math.isfinite(float(value)):
            return None, "not a finite number"
        scale = int(output["type"]["scale"])
        try:
            exact = Decimal(value) if isinstance(value, numbers.Integral) else Decimal(repr(float(value)))
            return exact.quantize(Decimal(1).scaleb(-scale), rounding=ROUND_HALF_EVEN), None
        except InvalidOperation:
            return None, f"does not fit scale {scale}"
    if kind == "integer":
        if not _number(value) or not math.isfinite(float(value)) or float(value) != int(value):
            return None, "not an integer"
        return int(value), None
    if kind == "boolean":
        if not isinstance(value, bool):
            return None, "not a boolean"
        return value, None
    if kind == "enum":
        declared = [entry["value"] for entry in output["type"]["values"]]
        if not isinstance(value, str) or value not in declared:
            return None, f"not one of {declared}"
        return value, None
    if kind == "text":
        if not isinstance(value, str):
            return None, "not text"
        return value, None
    return None, f"an output of kind {kind} cannot be written"


def check_outputs(model: dict, subjects: list[str], outputs: pd.DataFrame) -> tuple[list[dict], list[Violation]]:
    """Every row's wire values and every violation, checking each output's type and nullability
    and each declared contract (range, notNull, finite, oneOf) on every row."""
    declared = {output["id"]: output for output in model["outputs"]}
    contracts: dict[str, list[dict]] = {}
    for contract in model.get("contracts", []):
        contracts.setdefault(contract["output"], []).append(contract)
    # Column by column, as plain Python values: a row-wise read would coerce a row's mixed types.
    columns = {output_id: outputs[output_id].tolist() for output_id in declared}
    rows: list[dict] = []
    violations: list[Violation] = []
    for index, subject in enumerate(subjects):
        values: dict = {}
        for output_id, output in declared.items():
            value = columns[output_id][index]
            rules = contracts.get(output_id, [])
            if _missing(value):
                values[output_id] = None
                for contract in rules:
                    if contract["rule"]["kind"] == "notNull":
                        violations.append(Violation(contract["id"], output_id, subject, None, "every object gets a value"))
                if not output["nullable"]:
                    violations.append(Violation(None, output_id, subject, None, "the output is not nullable"))
                continue
            if _number(value) and not math.isfinite(float(value)):
                finite = [contract for contract in rules if contract["rule"]["kind"] == "finite"]
                for contract in finite:
                    violations.append(Violation(contract["id"], output_id, subject, value, "not a finite number"))
                if not finite:
                    violations.append(Violation(None, output_id, subject, value, "not a finite number"))
                continue
            written, mismatch = _written(output, value)
            if mismatch:
                violations.append(Violation(None, output_id, subject, value, mismatch))
                continue
            for contract in rules:
                rule = contract["rule"]
                if rule["kind"] == "range":
                    low, high = rule.get("min"), rule.get("max")
                    if (low is not None and Decimal(written) < Decimal(low)) or (high is not None and Decimal(written) > Decimal(high)):
                        violations.append(Violation(contract["id"], output_id, subject, value,
                                                    f"outside [{low if low is not None else ''}, {high if high is not None else ''}]"))
                elif rule["kind"] == "oneOf" and str(written) not in rule["values"]:
                    violations.append(Violation(contract["id"], output_id, subject, value, f"not one of {rule['values']}"))
            values[output_id] = str(written) if isinstance(written, Decimal) else written
        rows.append(values)
    return rows, violations


def apply_model(cell: Cell, model_id: str, version_id: str, predict: Callable[[pd.DataFrame], pd.DataFrame], *,
                batch_id: str, record_action: str | None = None) -> Applied:
    """Score every input object the caller may read with `predict` and write the predictions,
    or refuse the whole batch before any write (see the module docs)."""
    model = model_definition(cell, model_id)
    predictions = (model.get("predictions") or {}).get("objectType")
    if not predictions:
        raise LookupError(f"model {model_id!r} names no predictions type")
    namespace, _, type_name = predictions.rpartition(".")
    action = record_action or f"{namespace}.{type_name.lower()}.record"
    input_type = model["input"]["objectType"]
    features = list(model["features"])

    rows = cell.rows(input_type, features)
    missing = [name for name in features if name not in rows.columns]
    if len(rows) and missing:
        raise ValueError(f"{input_type} has no column {', '.join(missing)}")
    input_revision = rows.attrs.get("snapshot") or revision(rows)
    subjects = [str(subject) for subject in rows["id"]] if len(rows) else []
    frame = rows[features].reset_index(drop=True) if len(rows) else pd.DataFrame(columns=features)

    outputs = predict(frame)
    if not isinstance(outputs, pd.DataFrame):
        raise TypeError("predict returns a pandas DataFrame of the model's outputs")
    expected = [output["id"] for output in model["outputs"]]
    if sorted(outputs.columns) != sorted(expected):
        raise ValueError(f"predict returned columns {list(outputs.columns)}; the model declares {expected}")
    if len(outputs) != len(subjects):
        raise ValueError(f"predict returned {len(outputs)} rows for {len(subjects)} input objects")

    values, violations = check_outputs(model, subjects, outputs)
    if violations:
        raise PredictionRefused(violations)

    applied = Applied(batch_id, version_id, len(subjects), str(input_revision))
    batch = {"batch_id": batch_id, "model_version": version_id, "input_type": input_type,
             "input_revision": applied.input_revision, "rows": applied.rows}
    applied.receipts.append(cell.call(BATCH_ACTION, batch, batch_id)["receipt"]["receiptHash"])
    for subject, row in zip(subjects, values):
        prediction_id = f"{subject}@{batch_id}"
        body = {"prediction_id": prediction_id, "subject": subject, "model_version": version_id, **row}
        applied.receipts.append(cell.call(action, body, prediction_id)["receipt"]["receiptHash"])
    return applied
