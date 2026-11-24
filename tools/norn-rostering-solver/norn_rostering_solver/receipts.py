"""Full-payload canonical receipt integrity for `norn.solver.rostering@0.1.0`.

Every solver receipt hashes the COMPLETE canonical response payload — including
`assignments`, `metrics`, `unsat_summary`, `status`, `evidence_label`,
`repair_optimality`, `repair_suggestions`, `alternative_repairs`, `verification`,
`input_digest`, and contract identity fields — so tampering with ANY field
invalidates `verify_receipt_digest`.
"""

from __future__ import annotations

from collections.abc import Mapping

from .boundary import domain_sha256_digest

RECEIPT_DOMAIN = b"norn.solver.rostering/receipt/v1\0"

# Fields excluded from the hashed payload because they either ARE the digest,
# embed the digest (`perform_summary`), or are per-request HTTP envelope wrappers.
_NON_RECEIPT_FIELDS: frozenset[str] = frozenset(
    {
        "invocation_id",
        "perform_summary",
        "solver_receipt_digest",
        "what_if",
    }
)


def canonical_receipt_payload(response_dict: Mapping[str, object]) -> dict[str, object]:
    """Return the complete canonical payload dict covered by `solver_receipt_digest`."""
    return {k: response_dict[k] for k in sorted(response_dict.keys()) if k not in _NON_RECEIPT_FIELDS}


def compute_receipt_digest(response_dict: Mapping[str, object]) -> str:
    """Compute the domain-separated SHA-256 receipt digest over the full canonical response."""
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
