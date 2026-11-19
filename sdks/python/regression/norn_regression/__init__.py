"""One-click regression assumptions for a dataset in a Norn cell (#521 demo).

`suite` is pure statistics; `cell` talks to a cell through the generated
client (`norn_client`), which is imported only when a cell name is used.
"""

from .suite import Check, assumption_suite

_CELL = {"Cell", "RegressionChecks", "Refused", "check_regression", "revision"}
__all__ = ["Check", "assumption_suite", *sorted(_CELL)]


def __getattr__(name: str):
    if name in _CELL:
        from . import cell
        return getattr(cell, name)
    raise AttributeError(name)
