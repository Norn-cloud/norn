"""Full-payload canonical receipt integrity for `norn.solver.rostering@0.1.0`.

Receipt Scope:
- `solver_receipt_digest` seals the complete core `SolveResponse` payload for the
  primary solve — including `assignments`, `metrics`, `unsat_summary`, `status`,
  `evidence_label`, `repair_optimality`, `repair_suggestions`, `alternative_repairs`,
  `verification`, `input_digest`, and contract identity fields.
- When the HTTP worker also returns a side-by-side `what_if` comparison envelope,
  `what_if.comparison_digest` independently seals the complete `WhatIfComparison`
  payload (including both `base_receipt_digest` and `alt_receipt_digest`).
"""

from __future__ import annotations

from collections.abc import Mapping

from .boundary import domain_sha256_digest

RECEIPT_DOMAIN = b"norn.solver.rostering/receipt/v1\0"
COMPARISON_DOMAIN = b"norn.solver.rostering/what-if/v1\0"

# Fields excluded from the primary `SolveResponse` receipt hash because they either
# ARE the digest, embed the digest (`perform_summary`), or are per-request HTTP
# envelope wrappers (`invocation_id`, `what_if`).
_NON_RECEIPT_FIELDS: frozenset[str] = frozenset(
    {
        "invocation_id",
        "perform_summary",
        "solver_receipt_digest",
        "what_if",
    }
)


def canonical_receipt_payload(response_dict: Mapping[str, object]) -> dict[str, object]:
    """Return the complete canonical core `SolveResponse` payload covered by `solver_receipt_digest`."""
    return {k: response_dict[k] for k in sorted(response_dict.keys()) if k not in _NON_RECEIPT_FIELDS}


def compute_receipt_digest(response_dict: Mapping[str, object]) -> str:
    """Compute the domain-separated SHA-256 receipt digest over the core `SolveResponse`."""
    return domain_sha256_digest(RECEIPT_DOMAIN, canonical_receipt_payload(response_dict))


def verify_receipt_digest(response_dict: Mapping[str, object]) -> bool:
    """Independently verify that a serialized `SolveResponse` has not been tampered with."""
    declared = response_dict.get("solver_receipt_digest")
    if not isinstance(declared, str) or not declared.startswith("sha256:"):
        return False

    expected = compute_receipt_digest(response_dict)
    if declared != expected:
        return False

    # Also verify consistency of derived `perform_summary` projection when present.
    summary = response_dict.get("perform_summary")
    if summary is not None:
        if not isinstance(summary, dict):
            return False
        assignments = response_dict.get("assignments")
        if not isinstance(assignments, list):
            return False
        if summary.get("solverDigest") != declared:
            return False
        if summary.get("status") != response_dict.get("status"):
            return False
        if summary.get("assignmentCount") != len(assignments):
            return False

    return True


def compute_comparison_digest(what_if_dict: Mapping[str, object]) -> str:
    """Compute the domain-separated SHA-256 digest over a `WhatIfComparison` payload."""
    payload = {k: what_if_dict[k] for k in sorted(what_if_dict.keys()) if k != "comparison_digest"}
    return domain_sha256_digest(COMPARISON_DOMAIN, payload)


def verify_comparison_digest(what_if_dict: Mapping[str, object]) -> bool:
    """Independently verify that a serialized `what_if` comparison has not been tampered with."""
    declared = what_if_dict.get("comparison_digest")
    if not isinstance(declared, str) or not declared.startswith("sha256:"):
        return False
    return declared == compute_comparison_digest(what_if_dict)
