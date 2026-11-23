"""CLI entrypoint for `norn.solver.rostering@0.1.0` (reads strict JSON on stdin, writes canonical JSON)."""

from __future__ import annotations

import sys

from .boundary import SolverContractError, canonical_json, parse_strict_json
from .models import SolveRequest
from .solver import solve_roster


def main() -> int:
    try:
        raw = parse_strict_json(sys.stdin.read())
        if not isinstance(raw, dict):
            raise SolverContractError("NORN-SOLVER-SCHEMA", "request payload must be a JSON object")
        request = SolveRequest.from_dict(raw)
        response = solve_roster(request)
        sys.stdout.write(canonical_json(response.to_dict()) + "\n")
        return 0
    except SolverContractError as exc:
        err = {"code": exc.code, "error": exc.message}
        sys.stderr.write(canonical_json(err) + "\n")
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
