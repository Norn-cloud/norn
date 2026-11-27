"""Authenticated Function manifest, invocation, and receipt builder for forecast evaluation.

Implements Norn1 canonical JSON and domain-separated SHA-256 matching
`crates/norn-canonical` and `crates/norn-ir/src/function.rs`, so a forecast
evaluation run produces a verifiable `norn.function/v1` `FunctionReceipt`
bound to:

- the declared evaluator Function manifest (`FORECAST_EVALUATOR_MANIFEST`),
- the exact model version and dataset revision evaluated,
- the tenant/environment scope, and
- the bounded typed evaluation summary (`evaluation_revision`, `metric_name`,
  `metric_value`, `contracts_passed`, `checks_passed`, `checks_failed`,
  `suite_version`).

Pure Python standard library; `assumption_suite` is imported lazily inside
`evaluate_forecast_receipt` so receipt verification and digest helpers run
without scientific-stack dependencies.
"""

from __future__ import annotations

import hashlib
import unicodedata
from decimal import Decimal, InvalidOperation
from typing import Any, Callable, Mapping

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


def _format_decimal(value: Decimal) -> str:
    if not value.is_finite():
        raise ValueError(f"non-finite decimal cannot be canonicalized: {value!r}")
    if value == 0:
        return "0"
    normalized = value.normalize()
    sign, digits, exponent = normalized.as_tuple()
    assert isinstance(exponent, int)
    prefix = "-" if sign else ""
    significant = "".join(str(d) for d in digits).lstrip("0")
    if not significant:
        return "0"
    if exponent >= 0:
        return f"{prefix}{significant}{'0' * exponent}"
    decimal_position = len(significant) + exponent
    if decimal_position <= 0:
        return f"{prefix}0.{'0' * (-decimal_position)}{significant}"
    return f"{prefix}{significant[:decimal_position]}.{significant[decimal_position:]}"


def canonical_json_str(value: Any) -> str:
    """Serialize `value` to Norn1 canonical JSON text.

    Rejects binary `float` values so no digest ever passes through IEEE-754
    rounding; callers must pass `int` or `Decimal`.
    """
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, Decimal):
        return _format_decimal(value)
    if isinstance(value, float):
        raise TypeError("binary float is forbidden in Norn1 canonical JSON; pass Decimal or int")
    if isinstance(value, str):
        return _escape_string(value)
    if isinstance(value, (list, tuple)):
        return "[" + ",".join(canonical_json_str(item) for item in value) + "]"
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
            f"{_escape_string(k)}:{canonical_json_str(v)}" for k, v in entries
        ) + "}"
    raise TypeError(f"unsupported Norn1 JSON value: {type(value).__name__}")


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


def parse_exact_decimal(value: Decimal | int | str, *, max_scale: int = 9) -> Decimal:
    """Parse an exact decimal value (rejecting floats, NaN, Inf, and excess scale)."""
    if isinstance(value, bool) or isinstance(value, float):
        raise TypeError(f"metric_value must be Decimal, int, or decimal text; got {type(value).__name__}")
    try:
        dec = value if isinstance(value, Decimal) else Decimal(str(value).strip())
    except InvalidOperation as exc:
        raise ValueError(f"invalid decimal value: {value!r}") from exc
    if not dec.is_finite():
        raise ValueError(f"non-finite decimal value: {value!r}")
    normalized = dec.normalize() if dec != 0 else Decimal(0)
    exponent = normalized.as_tuple().exponent
    assert isinstance(exponent, int)
    if -exponent > max_scale:
        raise ValueError(f"decimal scale {-exponent} exceeds max_scale={max_scale}: {value!r}")
    return normalized


def _schema_ref(schema_id: str, version: int, document: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "id": schema_id,
        "version": version,
        "digest": norn1_json_digest(document),
    }


def _digest_of_tag(label: str) -> str:
    return f"sha256:{hashlib.sha256(label.encode('utf-8')).hexdigest()}"


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


def forecast_evaluator_manifest() -> dict[str, Any]:
    """Return the immutable `FunctionManifest` for `ml.forecast.evaluate` v1.

    Admitted by `norn_checker::function::admit_function_manifest` under the
    `hermetic_container` profile (`network: false`, `credentials: false`,
    `isolatedScratch: true`, `locale: "C.UTF-8"`, `timezone: "UTC"`,
    `cache: "none"`, `retention: "ledger"`).
    """
    implementation_digest = _digest_of_tag(
        "norn.ml.forecast.evaluate/oci-image/norn_regression.suite/v1"
    )
    runtime_digest = _digest_of_tag("norn.runtime/containerd/1.7/v1")
    closure_entries = [
        {
            "name": "base-python-3.12-slim",
            "kind": "base_image",
            "digest": _digest_of_tag("norn.dep/base-python-3.12-slim/v1"),
        },
        {
            "name": "norn-regression-suite-v1",
            "kind": "library",
            "digest": _digest_of_tag("norn.dep/norn-regression-suite/v1"),
        },
        {
            "name": "statsmodels-scipy-numpy-pandas",
            "kind": "library",
            "digest": _digest_of_tag("norn.dep/statsmodels-scipy-numpy-pandas/v1"),
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
            "id": "containerd",
            "version": "1.7",
            "digest": runtime_digest,
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


def build_evaluation_invocation(
    *,
    tenant: str,
    authority_id: str,
    command_id: str,
    version_id: str,
    model_id: str,
    evaluation_run: str,
    dataset_revision: str,
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
    """Build a schema-valid `FunctionInvocation` pinning evaluator, model, revision, and scope."""
    resolved_manifest = FORECAST_EVALUATOR_MANIFEST if manifest is None else manifest
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
        "blobs": {} if blobs is None else dict(blobs),
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
) -> tuple[dict[str, Any], dict[str, Any], list[Any], dict[str, Any]]:
    """Run `assumption_suite` and bind its outcome to a `FunctionInvocation` + `FunctionReceipt`."""
    from .suite import SUITE_VERSION, assumption_suite

    checks, plots = assumption_suite(frame, target, features, order=order)
    checks_failed = sum(1 for check in checks if check.status == "fail")
    checks_passed = sum(1 for check in checks if check.status == "pass")
    if metric_fn is not None:
        metric_val = parse_exact_decimal(metric_fn(plots), max_scale=9)
    else:
        raw_r2 = plots["r_squared"]
        metric_val = parse_exact_decimal(f"{float(raw_r2):.6f}", max_scale=9)
    contracts_passed = bool(extra_contracts_passed) and checks_failed == 0
    invocation = build_evaluation_invocation(
        tenant=tenant,
        authority_id=authority_id,
        command_id=command_id,
        version_id=version_id,
        model_id=model_id,
        evaluation_run=evaluation_run,
        dataset_revision=dataset_revision,
        metric_name=metric_name,
        target=target,
        features=features,
        order=order,
        fact_pins=fact_pins,
        deadline_millis=deadline_millis,
    )
    receipt = build_evaluation_receipt(
        invocation,
        evaluation_revision=dataset_revision,
        metric_name=metric_name,
        metric_value=metric_val,
        contracts_passed=contracts_passed,
        checks_passed=checks_passed,
        checks_failed=checks_failed,
        suite_version=SUITE_VERSION,
        executor=executor,
        placement=placement,
    )
    return invocation, receipt, checks, plots
