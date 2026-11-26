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
from collections.abc import Callable
from dataclasses import dataclass, field
from decimal import ROUND_HALF_EVEN, Decimal, InvalidOperation
from typing import Any

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
    return isinstance(value, (numbers.Real, Decimal)) and not isinstance(value, bool)


def _is_finite_number(value: Any) -> bool:
    if isinstance(value, Decimal):
        return value.is_finite()
    if isinstance(value, numbers.Integral):
        return True
    return math.isfinite(float(value))


def _written(
    output: dict,
    value: Any,
    *,
    enum_set: frozenset[str] | None = None,
    enum_list: list[str] | None = None,
) -> tuple[Any, str | None]:
    """The wire value for a non-missing `value` of `output`, or why it does not fit the type."""
    kind = output["type"]["kind"]
    if kind == "decimal":
        if not _number(value):
            return None, "not a number"
        if not _is_finite_number(value):
            return None, "not a finite number"
        scale = int(output["type"]["scale"])
        try:
            if isinstance(value, Decimal):
                exact = value
            elif isinstance(value, numbers.Integral):
                exact = Decimal(int(value))
            else:
                exact = Decimal(repr(float(value)))
            return exact.quantize(Decimal(1).scaleb(-scale), rounding=ROUND_HALF_EVEN), None
        except InvalidOperation:
            return None, f"does not fit scale {scale}"
    if kind == "integer":
        if not _number(value):
            return None, "not an integer"
        if isinstance(value, numbers.Integral):
            return int(value), None
        if isinstance(value, Decimal):
            if not value.is_finite() or value != value.to_integral_value():
                return None, "not an integer"
            return int(value), None
        as_float = float(value)
        if not math.isfinite(as_float) or not as_float.is_integer():
            return None, "not an integer"
        return int(as_float), None
    if kind == "boolean":
        if not isinstance(value, bool):
            return None, "not a boolean"
        return value, None
    if kind == "enum":
        if enum_set is None or enum_list is None:
            enum_list = [entry["value"] for entry in output["type"]["values"]]
            enum_set = frozenset(enum_list)
        if not isinstance(value, str) or value not in enum_set:
            return None, f"not one of {enum_list}"
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
    enum_meta: dict[str, tuple[frozenset[str], list[str]]] = {}
    for output_id, output in declared.items():
        if output["type"]["kind"] == "enum":
            vals = [entry["value"] for entry in output["type"]["values"]]
            enum_meta[output_id] = (frozenset(vals), vals)

    contracts: dict[str, list[tuple[str, str, Any]]] = {}
    for contract in model.get("contracts", []):
        rule = contract["rule"]
        kind = rule["kind"]
        if kind == "range":
            low, high = rule.get("min"), rule.get("max")
            payload: Any = (
                Decimal(low) if low is not None else None,
                Decimal(high) if high is not None else None,
                f"outside [{low if low is not None else ''}, {high if high is not None else ''}]",
            )
        elif kind == "oneOf":
            vals = rule["values"]
            payload = (frozenset(vals), f"not one of {vals}")
        else:
            payload = None
        contracts.setdefault(contract["output"], []).append((contract["id"], kind, payload))

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
                for contract_id, kind, _ in rules:
                    if kind == "notNull":
                        violations.append(Violation(contract_id, output_id, subject, None, "every object gets a value"))
                if not output["nullable"]:
                    violations.append(Violation(None, output_id, subject, None, "the output is not nullable"))
                continue
            if _number(value) and not _is_finite_number(value):
                finite = [contract_id for contract_id, kind, _ in rules if kind == "finite"]
                for contract_id in finite:
                    violations.append(Violation(contract_id, output_id, subject, value, "not a finite number"))
                if not finite:
                    violations.append(Violation(None, output_id, subject, value, "not a finite number"))
                continue
            enum_set, enum_list = enum_meta.get(output_id, (None, None))
            written, mismatch = _written(output, value, enum_set=enum_set, enum_list=enum_list)
            if mismatch:
                violations.append(Violation(None, output_id, subject, value, mismatch))
                continue
            for contract_id, kind, payload in rules:
                if kind == "range":
                    low_dec, high_dec, reason = payload
                    as_dec = written if isinstance(written, Decimal) else Decimal(written)
                    if (low_dec is not None and as_dec < low_dec) or (high_dec is not None and as_dec > high_dec):
                        violations.append(Violation(contract_id, output_id, subject, value, reason))
                elif kind == "oneOf":
                    allowed, reason = payload
                    if str(written) not in allowed:
                        violations.append(Violation(contract_id, output_id, subject, value, reason))
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
    for subject, row in zip(subjects, values, strict=True):
        prediction_id = f"{subject}@{batch_id}"
        body = {"prediction_id": prediction_id, "subject": subject, "model_version": version_id, **row}
        applied.receipts.append(cell.call(action, body, prediction_id)["receipt"]["receiptHash"])
    return applied
