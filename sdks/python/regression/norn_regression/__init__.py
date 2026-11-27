"""One-click regression assumptions for a dataset in a Norn cell (#521 demo).

`suite` is pure statistics; `receipt` builds authenticated `norn.function/v1`
evaluation manifests, invocations, and receipts (pure stdlib); `cell` talks to
a cell through the generated client (`norn_client`), imported lazily.
"""

from .receipt import (
    FORECAST_EVALUATOR_FUNCTION_ID,
    FORECAST_EVALUATOR_MANIFEST,
    FORECAST_EVALUATOR_MANIFEST_DIGEST,
    build_evaluation_invocation,
    build_evaluation_receipt,
    evaluate_forecast_receipt,
    receipt_digest,
)

_SUITE = {"Check", "assumption_suite"}
_CELL = {"Cell", "RegressionChecks", "Refused", "check_regression", "revision"}
__all__ = [
    "FORECAST_EVALUATOR_FUNCTION_ID",
    "FORECAST_EVALUATOR_MANIFEST",
    "FORECAST_EVALUATOR_MANIFEST_DIGEST",
    "build_evaluation_invocation",
    "build_evaluation_receipt",
    "evaluate_forecast_receipt",
    "receipt_digest",
    *sorted(_SUITE),
    *sorted(_CELL),
]


def __getattr__(name: str):
    if name in _SUITE:
        from . import suite
        return getattr(suite, name)
    if name in _CELL:
        from . import cell
        return getattr(cell, name)
    raise AttributeError(name)
