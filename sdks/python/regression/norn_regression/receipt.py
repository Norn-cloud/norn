"""Authenticated Function manifest, invocation, and receipt builder for forecast evaluation.

Implements Norn1 canonical JSON and domain-separated SHA-256 matching
`crates/norn-canonical` and `crates/norn-ir/src/function.rs`, so a forecast
evaluation run produces a verifiable `norn.function/v1` `FunctionReceipt`
bound to:

- the declared evaluator Function manifest (`FORECAST_EVALUATOR_MANIFEST`),
  whose `dependencyClosure`, `implementation.digest`, and `runtime.digest` pin
  the actual on-disk `Dockerfile`, `requirements.txt`, `norn_regression/suite.py`,
  and `python:3.11-slim@sha256:...` base image digest;
- the exact content digest of the evaluated dataset snapshot (`blobs["dataset"]`)
  as well as `model_id`, `version_id`, and `dataset_revision`;
- the tenant/environment scope; and
- the bounded typed evaluation summary (`evaluation_revision`, `metric_name`,
  `metric_value`, `contracts_passed`, `checks_passed`, `checks_failed`,
  `suite_version`).

Pure Python standard library; `assumption_suite` is imported lazily inside
`evaluate_forecast_receipt` so receipt verification and digest helpers run
without scientific-stack dependencies.
"""

from __future__ import annotations

import hashlib
import math
import re
import unicodedata
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any, Callable, Mapping, Sequence

FUNCTION_ABI_V1 = "norn.function-abi/1"
FORECAST_EVALUATOR_FUNCTION_ID = "ml.forecast.evaluate"
FORECAST_EVALUATOR_VERSION = 1

NORN1_DOMAIN_TAG = b"norn.canonical.json/norn1\0"
MANIFEST_DOMAIN = b"norn.function/manifest/v1\0"
EXECUTION_PROFILE_DOMAIN = b"norn.function/execution-profile/v1\0"
DEPENDENCY_CLOSURE_DOMAIN = b"norn.function/dependency-closure/v1\0"
INVOCATION_DOMAIN = b"norn.function/invocation/v1\0"
INPUT_IDENTITY_DOMAIN = b"norn.function/inputs/v1\0"
DEPENDENCY_TOKEN_DOMAIN = b"norn.function/dependency-token/v1\0"
ACTION_KEY_DOMAIN = b"norn.function-action/v1\0"
OUTPUT_MANIFEST_DOMAIN = b"norn.function/output-manifest/v1\0"
RESULT_IDENTITY_DOMAIN = b"norn.function/result/v1\0"
RECEIPT_DOMAIN = b"norn.function/receipt/v1\0"

MAX_RECEIPT_SUMMARY_BYTES = 32 * 1024
# Mirrors `crates/norn-canonical/src/lib.rs`: per-number and per-document bounds.
MAX_CANONICAL_NUMBER_BYTES = 1_048_576
MAX_DOCUMENT_NUMBER_BYTES = 4 * MAX_CANONICAL_NUMBER_BYTES

_DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")

# Real content digests of the evaluator build artifacts in `sdks/python/regression/`:
# - `BASE_IMAGE_DIGEST`: pinned `FROM python:3.11-slim@sha256:...` in `Dockerfile`
# - `DOCKERFILE_DIGEST`: SHA-256 of `sdks/python/regression/Dockerfile`
# - `REQUIREMENTS_DIGEST`: SHA-256 of `sdks/python/regression/requirements.txt`
# - `SUITE_SOURCE_DIGEST`: SHA-256 of `sdks/python/regression/norn_regression/suite.py`
BASE_IMAGE_DIGEST = "sha256:07d4824036af339fa1b6ef769cc99d9c10a141769538b52b3b597c84f7a7b535"
DOCKERFILE_DIGEST = "sha256:48a768534f887b2272aaad42e884f9fb6c3ed20042b96cb1b0c58ff7c0adbbc6"
REQUIREMENTS_DIGEST = "sha256:76f8b5d4d5fc10d508b3a372246548a6738fea83a652308da173b41f4f915adf"
SUITE_SOURCE_DIGEST = "sha256:15361cf814d137f03885fdca7d8e118f2dbc3a0fdae6d8911ca6c43269b5a491"

_CONTROL_ESCAPES = {
    '"': '\\"',
    "\\": "\\\\",
    "\b": "\\b",
    "\f": "\\f",
    "\n": "\\n",
    "\r": "\\r",
    "\t": "\\t",
}


def _escape_string(value: str) -> str:
    normalized = unicodedata.normalize("NFC", value)
    out = ['"']
    for ch in normalized:
        escaped = _CONTROL_ESCAPES.get(ch)
        if escaped is not None:
            out.append(escaped)
        elif ord(ch) < 0x20:
            out.append(f"\\u{ord(ch):04x}")
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def _exact_decimal_parts(value: Decimal) -> tuple[bool, str, int]:
    """Return `(negative, stripped_significant_digits, adjusted_exponent)` without rounding.

    Never calls `Decimal.normalize()`, which rounds to the active context's
    precision (28 digits by default).
    """
    if not value.is_finite():
        raise ValueError(f"non-finite decimal cannot be canonicalized: {value!r}")
    sign, digits, exponent = value.as_tuple()
    assert isinstance(exponent, int)
    start = 0
    while start < len(digits) and digits[start] == 0:
        start += 1
    if start == len(digits):
        return False, "", 0
    end = len(digits)
    while end > start and digits[end - 1] == 0:
        end -= 1
        exponent += 1
    significant = "".join(str(d) for d in digits[start:end])
    return bool(sign), significant, exponent


def _charge_number_bytes(output_len: int, budget: list[int]) -> None:
    next_total = budget[0] + output_len
    if output_len > MAX_CANONICAL_NUMBER_BYTES or next_total > MAX_DOCUMENT_NUMBER_BYTES:
        raise ValueError(
            f"canonical number expansion ({output_len} bytes, total {next_total}) exceeds Norn1 limit"
        )
    budget[0] = next_total


def _format_decimal(value: Decimal, budget: list[int]) -> str:
    negative, significant, exponent = _exact_decimal_parts(value)
    if not significant:
        _charge_number_bytes(1, budget)
        return "0"
    decimal_position = len(significant) + exponent
    sign_len = 1 if negative else 0
    if decimal_position <= 0:
        zero_count = -decimal_position
        output_len = sign_len + 2 + zero_count + len(significant)
        _charge_number_bytes(output_len, budget)
        prefix = "-" if negative else ""
        return f"{prefix}0.{'0' * zero_count}{significant}"
    if decimal_position < len(significant):
        output_len = sign_len + len(significant) + 1
        _charge_number_bytes(output_len, budget)
        prefix = "-" if negative else ""
        return f"{prefix}{significant[:decimal_position]}.{significant[decimal_position:]}"
    zero_count = decimal_position - len(significant)
    output_len = sign_len + decimal_position
    _charge_number_bytes(output_len, budget)
    prefix = "-" if negative else ""
    return f"{prefix}{significant}{'0' * zero_count}"


def _canonical_json_str(value: Any, budget: list[int]) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        text = str(value)
        _charge_number_bytes(len(text), budget)
        return text
    if isinstance(value, Decimal):
        return _format_decimal(value, budget)
    if isinstance(value, float):
        raise TypeError("binary float is forbidden in Norn1 canonical JSON; pass Decimal or int")
    if isinstance(value, str):
        return _escape_string(value)
    if isinstance(value, (list, tuple)):
        return "[" + ",".join(_canonical_json_str(item, budget) for item in value) + "]"
    if isinstance(value, Mapping):
        entries: list[tuple[str, Any]] = []
        for key, item in value.items():
            if not isinstance(key, str):
                raise TypeError(f"JSON object key must be str, got {type(key).__name__}")
            entries.append((unicodedata.normalize("NFC", key), item))
        entries.sort(key=lambda pair: pair[0])
        for index in range(1, len(entries)):
            if entries[index - 1][0] == entries[index][0]:
                raise ValueError(f"duplicate canonical object key: {entries[index][0]!r}")
        return "{" + ",".join(
            f"{_escape_string(k)}:{_canonical_json_str(v, budget)}" for k, v in entries
        ) + "}"
    raise TypeError(f"unsupported Norn1 JSON value: {type(value).__name__}")


def canonical_json_str(value: Any) -> str:
    """Serialize `value` to Norn1 canonical JSON text.

    Rejects binary `float` values so no digest ever passes through IEEE-754
    rounding; callers must pass `int` or `Decimal`.
    """
    return _canonical_json_str(value, [0])


def canonical_json(value: Any) -> bytes:
    """Serialize `value` to Norn1 canonical UTF-8 JSON bytes."""
    return canonical_json_str(value).encode("utf-8")


def norn1_json_digest(value: Any) -> str:
    """`Digest::of_json_document` (`sha256(NORN1_DOMAIN_TAG || canonical_json)`)."""
    digest = hashlib.sha256(NORN1_DOMAIN_TAG + canonical_json(value)).hexdigest()
    return f"sha256:{digest}"


def domain_hash(domain: bytes, value: Any) -> str:
    """Domain-separated SHA-256 over Norn1 canonical JSON (`sha256:<64 hex>`)."""
    digest = hashlib.sha256(domain + canonical_json(value)).hexdigest()
    return f"sha256:{digest}"


def _require_digest(value: Any, label: str) -> str:
    if not isinstance(value, str) or not _DIGEST_RE.fullmatch(value):
        raise ValueError(f"{label} must be a well-formed sha256 digest, got {value!r}")
    return value


def parse_exact_decimal(value: Decimal | int | str, *, max_scale: int = 9) -> Decimal:
    """Parse an exact decimal value (rejecting floats, NaN, Inf, excess scale, or context rounding)."""
    if isinstance(value, bool) or isinstance(value, float):
        raise TypeError(f"metric_value must be Decimal, int, or decimal text; got {type(value).__name__}")
    try:
        dec = value if isinstance(value, Decimal) else Decimal(str(value).strip())
    except InvalidOperation as exc:
        raise ValueError(f"invalid decimal value: {value!r}") from exc
    negative, significant, exponent = _exact_decimal_parts(dec)
    if not significant:
        return Decimal(0)
    scale = max(0, -exponent)
    if scale > max_scale:
        raise ValueError(f"decimal scale {scale} exceeds max_scale={max_scale}: {value!r}")
    # Validate Norn1 canonical expansion limit without mutating precision.
    _format_decimal(dec, [0])
    # Reconstruct exact reduced Decimal without context rounding.
    sign_int = 1 if negative else 0
    return Decimal((sign_int, tuple(int(ch) for ch in significant), exponent))


def _schema_ref(schema_id: str, version: int, document: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "id": schema_id,
        "version": version,
        "digest": norn1_json_digest(document),
    }


EVALUATION_INPUT_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": [
        "model_id",
        "version_id",
        "evaluation_run",
        "dataset_revision",
        "metric_name",
        "target",
    ],
    "properties": {
        "model_id": {"type": "string"},
        "version_id": {"type": "string"},
        "evaluation_run": {"type": "string"},
        "dataset_revision": {"type": "string"},
        "metric_name": {"type": "string"},
        "target": {"type": "string"},
        "features": {"type": "string"},
        "order": {"type": ["string", "null"]},
    },
}

EVALUATION_OUTPUT_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": [
        "evaluation_revision",
        "metric_name",
        "metric_value",
        "contracts_passed",
        "checks_passed",
        "checks_failed",
        "suite_version",
    ],
    "properties": {
        "evaluation_revision": {"type": "string"},
        "metric_name": {"type": "string"},
        "metric_value": {"type": "number", "nornType": {"kind": "decimal", "scale": 9}},
        "contracts_passed": {"type": "boolean"},
        "checks_passed": {"type": "integer"},
        "checks_failed": {"type": "integer"},
        "suite_version": {"type": "string"},
    },
}

EVALUATION_ERROR_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["message"],
    "properties": {
        "message": {"type": "string"},
    },
}


def verify_evaluator_artifact_pins(regression_dir: Path | None = None) -> None:
    """Verify pinned digests match the actual `Dockerfile`, `requirements.txt`, and `suite.py` on disk."""
    root = Path(__file__).resolve().parents[1] if regression_dir is None else Path(regression_dir)
    dockerfile_bytes = (root / "Dockerfile").read_bytes()
    actual_dockerfile = f"sha256:{hashlib.sha256(dockerfile_bytes).hexdigest()}"
    if actual_dockerfile != DOCKERFILE_DIGEST:
        raise ValueError(
            f"Dockerfile digest drifted: expected {DOCKERFILE_DIGEST}, got {actual_dockerfile}"
        )
    if f"python:3.11-slim@{BASE_IMAGE_DIGEST}" not in dockerfile_bytes.decode("utf-8"):
        raise ValueError(f"Dockerfile does not pin python:3.11-slim@{BASE_IMAGE_DIGEST}")
    actual_reqs = f"sha256:{hashlib.sha256((root / 'requirements.txt').read_bytes()).hexdigest()}"
    if actual_reqs != REQUIREMENTS_DIGEST:
        raise ValueError(
            f"requirements.txt digest drifted: expected {REQUIREMENTS_DIGEST}, got {actual_reqs}"
        )
    actual_suite = f"sha256:{hashlib.sha256((root / 'norn_regression' / 'suite.py').read_bytes()).hexdigest()}"
    if actual_suite != SUITE_SOURCE_DIGEST:
        raise ValueError(
            f"suite.py digest drifted: expected {SUITE_SOURCE_DIGEST}, got {actual_suite}"
        )


def forecast_evaluator_manifest() -> dict[str, Any]:
    """Return the immutable `FunctionManifest` for `ml.forecast.evaluate` v1.

    Admitted by `norn_checker::function::admit_function_manifest` under the
    `hermetic_container` profile (`network: false`, `credentials: false`,
    `isolatedScratch: true`, `locale: "C.UTF-8"`, `timezone: "UTC"`,
    `cache: "none"`, `retention: "ledger"`). Every digest pins real build
    artifacts (`Dockerfile`, `requirements.txt`, `norn_regression/suite.py`,
    and the `python:3.11-slim@sha256:...` base image digest).
    """
    implementation_digest = norn1_json_digest(
        {
            "baseImage": BASE_IMAGE_DIGEST,
            "dockerfile": DOCKERFILE_DIGEST,
            "requirements": REQUIREMENTS_DIGEST,
            "suiteSource": SUITE_SOURCE_DIGEST,
        }
    )
    closure_entries = [
        {
            "name": "base-python-3.11-slim",
            "kind": "base_image",
            "digest": BASE_IMAGE_DIGEST,
        },
        {
            "name": "norn-regression-dockerfile",
            "kind": "asset",
            "digest": DOCKERFILE_DIGEST,
        },
        {
            "name": "norn-regression-requirements",
            "kind": "library",
            "digest": REQUIREMENTS_DIGEST,
        },
        {
            "name": "norn-regression-suite",
            "kind": "library",
            "digest": SUITE_SOURCE_DIGEST,
        },
    ]
    return {
        "abi": FUNCTION_ABI_V1,
        "functionId": FORECAST_EVALUATOR_FUNCTION_ID,
        "version": FORECAST_EVALUATOR_VERSION,
        "inputSchema": _schema_ref("ml.forecast.evaluate.input", 1, EVALUATION_INPUT_SCHEMA),
        "outputSchema": _schema_ref("ml.forecast.evaluate.output", 1, EVALUATION_OUTPUT_SCHEMA),
        "errorSchema": _schema_ref("ml.forecast.evaluate.error", 1, EVALUATION_ERROR_SCHEMA),
        "implementation": {
            "format": "oci_image",
            "digest": implementation_digest,
            "entryPoint": "norn_regression.receipt:evaluate_forecast_receipt",
            "locator": f"ghcr.io/norn-cloud/norn-forecast-evaluator@{implementation_digest}",
        },
        "dependencyClosure": {
            "digest": domain_hash(DEPENDENCY_CLOSURE_DOMAIN, closure_entries),
            "entries": closure_entries,
        },
        "runtime": {
            "id": "python-container",
            "version": "3.11-slim",
            "digest": BASE_IMAGE_DIGEST,
        },
        "platform": {
            "os": "linux",
            "arch": "amd64",
        },
        "capabilities": [],
        "determinism": {
            "hermetic_container": {
                "network": False,
                "credentials": False,
                "isolatedScratch": True,
                "locale": "C.UTF-8",
                "timezone": "UTC",
                "goldenRuns": [],
                "qualified": False,
            }
        },
        "bounds": {
            "maxCpuMillis": 60_000,
            "maxWallMillis": 120_000,
            "maxMemoryBytes": 512 * 1024 * 1024,
            "maxInputBytes": 16 * 1024 * 1024,
            "maxDecodedBytes": 64 * 1024 * 1024,
            "maxOutputBytes": 1024 * 1024,
        },
        "cache": "none",
        "retention": "ledger",
        "steps": [],
    }


def manifest_digest(manifest: Mapping[str, Any]) -> str:
    """`FunctionManifest::content_digest`."""
    return domain_hash(MANIFEST_DOMAIN, manifest)


def execution_profile_digest(manifest: Mapping[str, Any]) -> str:
    """`FunctionManifest::execution_profile_digest`."""
    return domain_hash(
        EXECUTION_PROFILE_DOMAIN,
        {
            "runtime": manifest["runtime"],
            "platform": manifest["platform"],
            "determinism": manifest["determinism"],
            "capabilities": manifest["capabilities"],
            "bounds": manifest["bounds"],
        },
    )


def dependency_token_digest(dependency: Mapping[str, Any]) -> str:
    """`RelevantStateToken::digest`."""
    return domain_hash(DEPENDENCY_TOKEN_DOMAIN, dependency)


def invocation_digest(invocation: Mapping[str, Any]) -> str:
    """`FunctionInvocation::content_digest`."""
    return domain_hash(INVOCATION_DOMAIN, invocation)


def input_identity(invocation: Mapping[str, Any]) -> str:
    """`FunctionInvocation::input_identity`."""
    return domain_hash(
        INPUT_IDENTITY_DOMAIN,
        {
            "inputs": invocation["inputs"],
            "blobs": {name: blob["digest"] for name, blob in invocation["blobs"].items()},
        },
    )


def output_manifest_digest(outputs: list[Mapping[str, Any]], summary: Mapping[str, Any]) -> str:
    """`output_manifest_digest(outputs, summary)`."""
    return domain_hash(
        OUTPUT_MANIFEST_DOMAIN,
        {
            "outputs": outputs,
            "summary": summary,
        },
    )


def receipt_digest(receipt: Mapping[str, Any]) -> str:
    """`FunctionReceipt::content_digest`."""
    return domain_hash(RECEIPT_DOMAIN, receipt)


def result_identity(receipt: Mapping[str, Any]) -> str:
    """`FunctionReceipt::result_identity`."""
    return domain_hash(
        RESULT_IDENTITY_DOMAIN,
        {
            "commandId": receipt["commandId"],
            "run": receipt["run"],
            "manifest": receipt["manifest"],
            "executionProfile": receipt["executionProfile"],
            "inputIdentity": receipt["inputIdentity"],
            "dependencyToken": receipt["dependencyToken"],
            "outcome": receipt["outcome"],
        },
    )


FORECAST_EVALUATOR_MANIFEST: dict[str, Any] = forecast_evaluator_manifest()
FORECAST_EVALUATOR_MANIFEST_DIGEST: str = manifest_digest(FORECAST_EVALUATOR_MANIFEST)
FORECAST_EVALUATOR_PROFILE_DIGEST: str = execution_profile_digest(FORECAST_EVALUATOR_MANIFEST)


def _normalize_cell_value(value: Any) -> Any:
    if value is None or isinstance(value, (bool, int, Decimal, str)):
        return value
    if isinstance(value, float):
        if math.isnan(value):
            return None
        if not math.isfinite(value):
            raise ValueError(f"non-finite float in evaluated dataset snapshot: {value!r}")
        return Decimal(repr(value))
    # Handle numpy scalar types without importing numpy.
    if hasattr(value, "item") and callable(value.item):
        return _normalize_cell_value(value.item())
    return str(value)


def dataset_snapshot_blob(
    frame: Any,
    columns: Sequence[str],
    *,
    dataset_revision: str | None = None,
) -> dict[str, Any]:
    """Compute a `BlobRef` content digest over the exact evaluated dataset rows.

    Sorts rows by `id` when present (matching `suite.py`) so identical logical
    snapshots in different row orders share one content digest while any value
    difference produces a distinct `inputIdentity`.
    """
    if hasattr(frame, "columns") and hasattr(frame, "to_dict"):
        frame_cols = list(frame.columns)
        records: list[Mapping[str, Any]] = frame.to_dict(orient="records")
    elif isinstance(frame, Sequence) and not isinstance(frame, (str, bytes)):
        records = list(frame)
        frame_cols = list(records[0].keys()) if records else []
    else:
        raise TypeError(f"unsupported frame type for snapshot pinning: {type(frame).__name__}")

    ordered_cols: list[str] = []
    if "id" in frame_cols:
        ordered_cols.append("id")
    for col in columns:
        if col not in ordered_cols:
            ordered_cols.append(col)

    missing = [col for col in ordered_cols if col not in frame_cols]
    if missing:
        raise ValueError(f"evaluated dataset snapshot is missing required columns: {missing}")

    rows: list[list[Any]] = []
    for record in records:
        rows.append([_normalize_cell_value(record[col]) for col in ordered_cols])
    if "id" in ordered_cols:
        rows.sort(key=lambda row: "" if row[0] is None else str(row[0]))

    raw = canonical_json({"columns": ordered_cols, "rows": rows})
    return {
        "digest": f"sha256:{hashlib.sha256(raw).hexdigest()}",
        "version": dataset_revision,
        "sizeBytes": len(raw),
    }


def build_evaluation_invocation(
    *,
    tenant: str,
    authority_id: str,
    command_id: str,
    version_id: str,
    model_id: str,
    evaluation_run: str,
    dataset_revision: str,
    dataset_blob: Mapping[str, Any],
    metric_name: str,
    target: str,
    features: list[str],
    order: str | None = None,
    authority_kind: str = "service",
    generation: int = 1,
    provenance_commit: int = 1,
    fact_pins: list[dict[str, Any]] | None = None,
    blobs: dict[str, dict[str, Any]] | None = None,
    deadline_millis: int = 1_000_000,
    max_attempts: int = 2,
    max_cost_units: int = 100,
    fencing_token: str | None = None,
    cancellation_generation: int = 0,
    manifest: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Build a schema-valid `FunctionInvocation` pinning evaluator, dataset snapshot, model, and scope."""
    resolved_manifest = FORECAST_EVALUATOR_MANIFEST if manifest is None else manifest
    dataset_digest = _require_digest(dataset_blob.get("digest"), "dataset_blob.digest")
    size_bytes = dataset_blob.get("sizeBytes")
    if isinstance(size_bytes, bool) or not isinstance(size_bytes, int) or size_bytes < 0:
        raise ValueError(f"dataset_blob.sizeBytes must be a non-negative int, got {size_bytes!r}")
    all_blobs: dict[str, dict[str, Any]] = {} if blobs is None else dict(blobs)
    all_blobs["dataset"] = {
        "digest": dataset_digest,
        "version": dataset_blob.get("version", dataset_revision),
        "sizeBytes": size_bytes,
    }
    inputs: dict[str, Any] = {
        "dataset_revision": dataset_revision,
        "evaluation_run": evaluation_run,
        "features": ",".join(features),
        "metric_name": metric_name,
        "model_id": model_id,
        "order": order,
        "target": target,
        "version_id": version_id,
    }
    pins = [] if fact_pins is None else list(fact_pins)
    freshness = "relevant_state" if pins else "append_only"
    return {
        "tenant": tenant,
        "world": "main",
        "authority": {"kind": authority_kind, "id": authority_id},
        "commandId": command_id,
        "run": {"id": version_id, "generation": generation},
        "manifest": manifest_digest(resolved_manifest),
        "inputs": inputs,
        "blobs": all_blobs,
        "provenanceCommit": provenance_commit,
        "dependency": {
            "facts": pins,
            "predicates": [],
            "collections": [],
        },
        "freshness": freshness,
        "destination": None,
        "deadlineMillis": deadline_millis,
        "retry": {"maxAttempts": max_attempts, "maxCostUnits": max_cost_units},
        "fencing": {
            "token": fencing_token or f"fence-{command_id}",
            "cancellationGeneration": cancellation_generation,
        },
    }


def build_evaluation_receipt(
    invocation: Mapping[str, Any],
    *,
    evaluation_revision: str,
    metric_name: str,
    metric_value: Decimal | int | str,
    contracts_passed: bool,
    checks_passed: int,
    checks_failed: int,
    suite_version: str = "1",
    executor: str = "pool.ml-evaluator",
    placement: str = "ml-evaluator/node-1",
    attempt: int = 1,
    cpu_millis: int = 10,
    wall_millis: int = 25,
    peak_memory_bytes: int = 16 * 1024 * 1024,
    outputs: list[dict[str, Any]] | None = None,
    manifest: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Build an authenticated `FunctionReceipt` bound to `invocation` and its typed summary."""
    resolved_manifest = FORECAST_EVALUATOR_MANIFEST if manifest is None else manifest
    expected_manifest_digest = manifest_digest(resolved_manifest)
    if invocation["manifest"] != expected_manifest_digest:
        raise ValueError(
            f"invocation manifest {invocation['manifest']} does not match {expected_manifest_digest}"
        )
    if "dataset" not in invocation.get("blobs", {}):
        raise ValueError("invocation must pin blobs['dataset'] content digest")
    _require_digest(invocation["blobs"]["dataset"].get("digest"), "invocation.blobs.dataset.digest")
    if not isinstance(contracts_passed, bool):
        raise TypeError("contracts_passed must be bool")
    exact_metric = parse_exact_decimal(metric_value, max_scale=9)
    summary: dict[str, Any] = {
        "evaluation_revision": evaluation_revision,
        "metric_name": metric_name,
        "metric_value": exact_metric,
        "contracts_passed": contracts_passed,
        "checks_passed": int(checks_passed),
        "checks_failed": int(checks_failed),
        "suite_version": suite_version,
    }
    summary_bytes = len(canonical_json(summary))
    if summary_bytes > MAX_RECEIPT_SUMMARY_BYTES:
        raise ValueError(
            f"receipt summary ({summary_bytes} bytes) exceeds {MAX_RECEIPT_SUMMARY_BYTES} bytes"
        )
    resolved_outputs: list[dict[str, Any]] = [] if outputs is None else list(outputs)
    output_bytes = sum(int(item["sizeBytes"]) for item in resolved_outputs) + summary_bytes
    return {
        "commandId": invocation["commandId"],
        "run": dict(invocation["run"]),
        "attempt": attempt,
        "dispatch": invocation_digest(invocation),
        "manifest": expected_manifest_digest,
        "executionProfile": execution_profile_digest(resolved_manifest),
        "inputIdentity": input_identity(invocation),
        "dependencyToken": dependency_token_digest(invocation["dependency"]),
        "outcome": {
            "success": {
                "outputManifest": output_manifest_digest(resolved_outputs, summary),
                "outputs": resolved_outputs,
                "summary": summary,
            }
        },
        "provenance": {
            "executor": executor,
            "placement": placement,
            "runtime": resolved_manifest["runtime"]["digest"],
        },
        "resources": {
            "cpuMillis": cpu_millis,
            "wallMillis": wall_millis,
            "peakMemoryBytes": peak_memory_bytes,
            "outputBytes": output_bytes,
            "fuel": None,
        },
        "cache": {"miss": {}},
    }


def evaluate_forecast_receipt(
    frame: Any,
    target: str,
    features: list[str],
    *,
    tenant: str,
    authority_id: str,
    command_id: str,
    version_id: str,
    model_id: str,
    evaluation_run: str,
    dataset_revision: str,
    metric_name: str = "r_squared",
    metric_fn: Callable[[dict[str, Any]], Decimal | int | str] | None = None,
    order: str | None = None,
    extra_contracts_passed: bool = True,
    executor: str = "pool.ml-evaluator",
    placement: str = "ml-evaluator/node-1",
    fact_pins: list[dict[str, Any]] | None = None,
    deadline_millis: int = 1_000_000,
    expected_dataset_digest: str | None = None,
    dispatched_invocation: Mapping[str, Any] | None = None,
    suite_runner: Callable[..., tuple[list[Any], dict[str, Any]]] | None = None,
    suite_version: str | None = None,
) -> tuple[dict[str, Any], dict[str, Any], list[Any], dict[str, Any]]:
    """Run `assumption_suite` and bind its outcome to a snapshot-pinned `FunctionInvocation` + `FunctionReceipt`."""
    columns = [target, *features] + ([order] if order is not None and order not in (target, *features) else [])
    actual_blob = dataset_snapshot_blob(frame, columns, dataset_revision=dataset_revision)
    if expected_dataset_digest is not None and actual_blob["digest"] != expected_dataset_digest:
        raise ValueError(
            f"evaluated dataset snapshot {actual_blob['digest']} does not match expected {expected_dataset_digest}"
        )
    if dispatched_invocation is not None:
        if dispatched_invocation.get("manifest") != FORECAST_EVALUATOR_MANIFEST_DIGEST:
            raise ValueError("dispatched invocation manifest does not match forecast evaluator")
        pinned_blob = dispatched_invocation.get("blobs", {}).get("dataset")
        if not isinstance(pinned_blob, Mapping) or pinned_blob.get("digest") != actual_blob["digest"]:
            raise ValueError(
                f"evaluated dataset snapshot {actual_blob['digest']} does not match dispatched blob pin {pinned_blob!r}"
            )
        if pinned_blob.get("sizeBytes") != actual_blob["sizeBytes"]:
            raise ValueError("evaluated dataset snapshot size does not match dispatched blob pin")
        if dispatched_invocation.get("inputs", {}).get("dataset_revision") != dataset_revision:
            raise ValueError("dataset_revision does not match dispatched invocation")
        invocation: dict[str, Any] = dict(dispatched_invocation)
    else:
        invocation = build_evaluation_invocation(
            tenant=tenant,
            authority_id=authority_id,
            command_id=command_id,
            version_id=version_id,
            model_id=model_id,
            evaluation_run=evaluation_run,
            dataset_revision=dataset_revision,
            dataset_blob=actual_blob,
            metric_name=metric_name,
            target=target,
            features=features,
            order=order,
            fact_pins=fact_pins,
            deadline_millis=deadline_millis,
        )

    if suite_runner is None:
        from .suite import SUITE_VERSION, assumption_suite

        runner = assumption_suite
        resolved_suite_version = SUITE_VERSION if suite_version is None else suite_version
    else:
        runner = suite_runner
        resolved_suite_version = "1" if suite_version is None else suite_version

    checks, plots = runner(frame, target, features, order=order)
    checks_failed = sum(1 for check in checks if check.status == "fail")
    checks_passed = sum(1 for check in checks if check.status == "pass")
    if metric_fn is not None:
        metric_val = parse_exact_decimal(metric_fn(plots), max_scale=9)
    else:
        if "r_squared" not in plots:
            raise ValueError("assumption_suite plots missing required 'r_squared' metric")
        raw_r2 = float(plots["r_squared"])
        if not math.isfinite(raw_r2):
            raise ValueError(f"non-finite r_squared metric: {raw_r2!r}")
        metric_val = parse_exact_decimal(f"{raw_r2:.6f}", max_scale=9)
    contracts_passed = bool(extra_contracts_passed) and checks_failed == 0
    receipt = build_evaluation_receipt(
        invocation,
        evaluation_revision=dataset_revision,
        metric_name=metric_name,
        metric_value=metric_val,
        contracts_passed=contracts_passed,
        checks_passed=checks_passed,
        checks_failed=checks_failed,
        suite_version=resolved_suite_version,
        executor=executor,
        placement=placement,
    )
    return invocation, receipt, checks, plots
