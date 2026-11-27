"""Authenticated Function manifest, invocation, and receipt builder for forecast evaluation.

Implements Norn1 canonical JSON and domain-separated SHA-256 matching
`crates/norn-canonical` and `crates/norn-ir/src/function.rs`, so a forecast
evaluation run produces a verifiable `norn.function/v1` `FunctionReceipt`
bound to:

- the declared evaluator Function manifest (`FORECAST_EVALUATOR_MANIFEST`),
  whose `dependencyClosure`, `implementation.digest`, and `runtime.digest` pin
  every file copied into `Dockerfile.evaluator` (`Dockerfile.evaluator`,
  `norn_regression/__init__.py`, `norn_regression/receipt.py`,
  `norn_regression/evaluator_cli.py`, and the `python:3.11-slim@sha256:...`
  base image digest) via `evaluator_pins.json`;
- the exact content digest, size, and version of BOTH the evaluated canonical
  dataset snapshot (`blobs["dataset"]`, `norn.snapshot/v1`) and the registered
  model artifact (`blobs["model"]`, `norn.linear-model/v1`);
- the exact evaluation settings (`model_id`, `version_id`, `evaluation_run`,
  `dataset_revision`, `metric_name`, `target`, `features`, `order`) and
  tenant/environment scope; and
- the bounded typed evaluation summary (`evaluation_revision`, `metric_name`,
  `metric_value`, `contracts_passed`, `checks_passed`, `checks_failed`,
  `suite_version`) computed strictly by scoring the pinned registered model
  artifact against the decoded canonical snapshot bytes.

Pure Python standard library (zero external dependencies).
"""

from __future__ import annotations

import hashlib
import json
import math
import re
import unicodedata
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
from fractions import Fraction
from pathlib import Path
from typing import Any, Mapping, Sequence

FUNCTION_ABI_V1 = "norn.function-abi/1"
FORECAST_EVALUATOR_FUNCTION_ID = "ml.forecast.evaluate"
FORECAST_EVALUATOR_VERSION = 1
EVALUATOR_SUITE_VERSION = "1"
SNAPSHOT_FORMAT_V1 = "norn.snapshot/v1"
MODEL_ARTIFACT_FORMAT_V1 = "norn.linear-model/v1"
SUPPORTED_METRICS = frozenset({"wape", "mae", "r_squared"})

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
# Closed identifier grammar so comma-joined `features` is unambiguous and
# cannot smuggle commas, whitespace, or control characters.
_COLUMN_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_.-]*$")
# Upper bound on bit length of an integer whose decimal representation fits
# within `MAX_CANONICAL_NUMBER_BYTES` (log2(10) ~= 3.321928).
_MAX_INT_BITS = int(MAX_CANONICAL_NUMBER_BYTES * 3.322) + 4

# Every file copied into `sdks/python/regression/Dockerfile.evaluator` (sorted by closure name).
_PINNED_ARTIFACT_SPECS: tuple[tuple[str, str, str, str], ...] = (
    ("norn-evaluator-cli", "library", "evaluatorCliSource", "norn_regression/evaluator_cli.py"),
    ("norn-evaluator-dockerfile", "asset", "dockerfile", "Dockerfile.evaluator"),
    ("norn-evaluator-init", "library", "initSource", "norn_regression/__init__.py"),
    ("norn-evaluator-receipt", "library", "receiptSource", "norn_regression/receipt.py"),
)

_CONTROL_ESCAPES = {
    '"': '\\"',
    "\\": "\\\\",
    "\b": "\\b",
    "\f": "\\f",
    "\n": "\\n",
    "\r": "\\r",
    "\t": "\\t",
}


@dataclass(frozen=True)
class EvaluationCheck:
    """One deterministic contract or threshold check over the registered model artifact."""

    name: str
    status: str  # "pass" or "fail"
    detail: str


def _require_digest(value: Any, label: str) -> str:
    if not isinstance(value, str) or not _DIGEST_RE.fullmatch(value):
        raise ValueError(f"{label} must be a well-formed sha256 digest, got {value!r}")
    return value


def _validate_column_name(name: str, label: str) -> str:
    if not isinstance(name, str) or not _COLUMN_NAME_RE.fullmatch(name):
        raise ValueError(
            f"{label} must match {_COLUMN_NAME_RE.pattern!r} (no commas or whitespace), got {name!r}"
        )
    return name


def _validate_eval_columns(
    target: str,
    features: Sequence[str],
    order: str | None,
) -> tuple[str, list[str], str | None]:
    valid_target = _validate_column_name(target, "target")
    if not features:
        raise ValueError("features must contain at least one feature column")
    valid_features: list[str] = []
    seen: set[str] = set()
    for feat in features:
        valid_feat = _validate_column_name(feat, "feature")
        if valid_feat == valid_target:
            raise ValueError(f"feature {valid_feat!r} must not equal target")
        if valid_feat in seen:
            raise ValueError(f"duplicate feature column: {valid_feat!r}")
        seen.add(valid_feat)
        valid_features.append(valid_feat)
    valid_order = None if order is None else _validate_column_name(order, "order")
    return valid_target, valid_features, valid_order


def _load_evaluator_pins() -> dict[str, str]:
    pins_path = Path(__file__).resolve().parent / "evaluator_pins.json"
    raw = json.loads(pins_path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        raise ValueError("evaluator_pins.json must be a JSON object")
    required_keys = {"baseImage", *(key for _, _, key, _ in _PINNED_ARTIFACT_SPECS)}
    missing = sorted(required_keys - set(raw.keys()))
    if missing:
        raise ValueError(f"evaluator_pins.json missing required digests: {missing}")
    return {key: _require_digest(raw[key], f"evaluator_pins.{key}") for key in sorted(required_keys)}


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


def _reduce_decimal(value: Decimal) -> Decimal:
    """Return an exact reduced `Decimal` with trailing fractional zeros stripped, without context rounding."""
    negative, significant, exponent = _exact_decimal_parts(value)
    if not significant:
        return Decimal(0)
    return Decimal((1 if negative else 0, tuple(int(ch) for ch in significant), exponent))


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


def _int_to_exact_decimal(value: int) -> Decimal:
    """Convert an arbitrary Python `int` to an exact `Decimal` without `str(int)` digit-limit crashes."""
    if abs(value).bit_length() > _MAX_INT_BITS:
        raise ValueError(
            f"canonical integer bit length ({abs(value).bit_length()}) exceeds Norn1 limit"
        )
    return Decimal(value)


def _canonical_json_str(value: Any, budget: list[int]) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return _format_decimal(_int_to_exact_decimal(value), budget)
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


def parse_exact_decimal(value: Decimal | int | str, *, max_scale: int = 9) -> Decimal:
    """Parse an exact decimal value (rejecting floats, NaN, Inf, excess scale, or context rounding)."""
    if isinstance(value, bool) or isinstance(value, float):
        raise TypeError(f"metric_value must be Decimal, int, or decimal text; got {type(value).__name__}")
    try:
        if isinstance(value, Decimal):
            dec = value
        elif isinstance(value, int):
            dec = _int_to_exact_decimal(value)
        else:
            dec = Decimal(str(value).strip())
    except InvalidOperation as exc:
        raise ValueError(f"invalid decimal value: {value!r}") from exc
    negative, significant, exponent = _exact_decimal_parts(dec)
    if not significant:
        return Decimal(0)
    scale = max(0, -exponent)
    if scale > max_scale:
        raise ValueError(f"decimal scale {scale} exceeds max_scale={max_scale}: {value!r}")
    _format_decimal(dec, [0])
    return Decimal((1 if negative else 0, tuple(int(ch) for ch in significant), exponent))


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
        "dataset_revision",
        "evaluation_run",
        "features",
        "metric_name",
        "model_id",
        "order",
        "target",
        "version_id",
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
    """Verify every pinned digest in `evaluator_pins.json` and the exact `Dockerfile.evaluator` recipe."""
    root = Path(__file__).resolve().parents[1] if regression_dir is None else Path(regression_dir)
    pins = _load_evaluator_pins()
    dockerfile_text = (root / "Dockerfile.evaluator").read_text(encoding="utf-8")

    expected_from = f"python:3.11-slim@{pins['baseImage']}"
    from_lines: list[str] = []
    actual_copies: set[tuple[str, str]] = set()
    entrypoint_lines: list[str] = []
    for line in dockerfile_text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        upper = stripped.upper()
        if upper.startswith("FROM "):
            from_lines.append(stripped)
        elif upper.startswith("COPY "):
            parts = stripped.split()
            if len(parts) != 3 or parts[1].startswith("--"):
                raise ValueError(f"Dockerfile.evaluator COPY must be exact `COPY <src> <dst>`: {stripped!r}")
            pair = (parts[1], parts[2])
            if pair in actual_copies:
                raise ValueError(f"Dockerfile.evaluator duplicate COPY instruction: {stripped!r}")
            actual_copies.add(pair)
        elif upper.startswith("ENTRYPOINT "):
            entrypoint_lines.append(stripped)

    if len(from_lines) != 1 or from_lines[0].split()[1] != expected_from:
        raise ValueError(f"Dockerfile.evaluator must declare exactly one `FROM {expected_from}`")
    if entrypoint_lines != ['ENTRYPOINT ["python", "-m", "norn_regression.evaluator_cli"]']:
        raise ValueError(
            "Dockerfile.evaluator must declare exactly ENTRYPOINT [\"python\", \"-m\", \"norn_regression.evaluator_cli\"]"
        )
    expected_copies = {
        ("sdks/python/regression/norn_regression/__init__.py", "/app/norn_regression/__init__.py"),
        ("sdks/python/regression/norn_regression/receipt.py", "/app/norn_regression/receipt.py"),
        ("sdks/python/regression/norn_regression/evaluator_cli.py", "/app/norn_regression/evaluator_cli.py"),
        ("sdks/python/regression/norn_regression/evaluator_pins.json", "/app/norn_regression/evaluator_pins.json"),
    }
    if actual_copies != expected_copies:
        raise ValueError(
            f"Dockerfile.evaluator COPY set mismatch: expected {sorted(expected_copies)}, got {sorted(actual_copies)}"
        )

    for _, _, key, rel_path in _PINNED_ARTIFACT_SPECS:
        actual = f"sha256:{hashlib.sha256((root / rel_path).read_bytes()).hexdigest()}"
        if actual != pins[key]:
            raise ValueError(
                f"{rel_path} digest drifted: expected {pins[key]}, got {actual}"
            )


def forecast_evaluator_manifest() -> dict[str, Any]:
    """Return the immutable `FunctionManifest` for `ml.forecast.evaluate` v1.

    Admitted by `norn_checker::function::admit_function_manifest` under the
    `hermetic_container` profile (`network: false`, `credentials: false`,
    `isolatedScratch: true`, `locale: "C.UTF-8"`, `timezone: "UTC"`,
    `cache: "none"`, `retention: "ledger"`). Every file copied into
    `Dockerfile.evaluator` is pinned in `evaluator_pins.json`; `locator` is
    `None` until a release pipeline publishes the built OCI image manifest.
    """
    pins = _load_evaluator_pins()
    implementation_digest = norn1_json_digest(pins)
    closure_entries = [
        {
            "name": "base-python-3.11-slim",
            "kind": "base_image",
            "digest": pins["baseImage"],
        },
        *[
            {
                "name": name,
                "kind": kind,
                "digest": pins[key],
            }
            for name, kind, key, _ in _PINNED_ARTIFACT_SPECS
        ],
    ]
    closure_entries.sort(key=lambda entry: entry["name"])
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
            "entryPoint": "python -m norn_regression.evaluator_cli",
            "locator": None,
        },
        "dependencyClosure": {
            "digest": domain_hash(DEPENDENCY_CLOSURE_DOMAIN, closure_entries),
            "entries": closure_entries,
        },
        "runtime": {
            "id": "python-container",
            "version": "3.11-slim",
            "digest": pins["baseImage"],
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


# ---------------------------------------------------------------------------
# Shared canonical dataset snapshot format (`norn.snapshot/v1`)
# ---------------------------------------------------------------------------


def _encode_typed_cell(value: Any) -> list[Any]:
    """Encode one dataset cell in canonical NFC/reduced typed form so hashed == decoded."""
    if value is None:
        return ["null", None]
    if isinstance(value, bool):
        return ["bool", value]
    if isinstance(value, int):
        return ["int", value]
    if isinstance(value, Decimal):
        return ["decimal", _reduce_decimal(value)]
    if isinstance(value, float):
        if math.isnan(value):
            return ["null", None]
        if not math.isfinite(value):
            raise ValueError(f"non-finite float in evaluated dataset snapshot: {value!r}")
        return ["float", repr(value)]
    if isinstance(value, str):
        return ["str", unicodedata.normalize("NFC", value)]
    if hasattr(value, "item") and callable(value.item):
        return _encode_typed_cell(value.item())
    return ["str", unicodedata.normalize("NFC", str(value))]


def _decode_typed_cell(cell: Any) -> Any:
    if not isinstance(cell, list) or len(cell) != 2 or not isinstance(cell[0], str):
        raise ValueError(f"malformed snapshot cell: {cell!r}")
    tag, payload = cell
    if tag == "null":
        if payload is not None:
            raise ValueError(f"null cell must have null payload, got {payload!r}")
        return None
    if tag == "bool":
        if not isinstance(payload, bool):
            raise ValueError(f"bool cell must carry bool, got {payload!r}")
        return payload
    if tag == "int":
        if isinstance(payload, bool) or not isinstance(payload, int):
            raise ValueError(f"int cell must carry int, got {payload!r}")
        return payload
    if tag == "decimal":
        if isinstance(payload, bool) or not isinstance(payload, (int, Decimal)):
            raise ValueError(f"decimal cell must carry exact number, got {payload!r}")
        return _reduce_decimal(Decimal(payload))
    if tag == "float":
        if not isinstance(payload, str):
            raise ValueError(f"float cell must carry string repr, got {payload!r}")
        parsed = float(payload)
        if not math.isfinite(parsed):
            raise ValueError(f"non-finite float cell: {payload!r}")
        return parsed
    if tag == "str":
        if not isinstance(payload, str):
            raise ValueError(f"str cell must carry string, got {payload!r}")
        return unicodedata.normalize("NFC", payload)
    raise ValueError(f"unknown snapshot cell tag: {tag!r}")


def encode_dataset_snapshot(frame: Any, columns: Sequence[str]) -> bytes:
    """Encode a DataFrame or sequence of row mappings into canonical `norn.snapshot/v1` bytes."""
    if hasattr(frame, "columns") and hasattr(frame, "to_dict"):
        frame_cols = list(frame.columns)
        records: list[Mapping[str, Any]] = frame.to_dict(orient="records")
    elif isinstance(frame, Sequence) and not isinstance(frame, (str, bytes)):
        records = list(frame)
        frame_cols = list(records[0].keys()) if records else []
    else:
        raise TypeError(f"unsupported frame type for snapshot encoding: {type(frame).__name__}")

    ordered_cols: list[str] = []
    if "id" in frame_cols:
        ordered_cols.append("id")
    for col in columns:
        valid_col = _validate_column_name(col, "snapshot column")
        if valid_col not in ordered_cols:
            ordered_cols.append(valid_col)

    missing = [col for col in ordered_cols if col not in frame_cols]
    if missing:
        raise ValueError(f"evaluated dataset snapshot is missing required columns: {missing}")

    typed_rows: list[list[list[Any]]] = [
        [_encode_typed_cell(record[col]) for col in ordered_cols]
        for record in records
    ]
    if "id" in ordered_cols:
        typed_rows.sort(
            key=lambda row: (
                "" if row[0][0] == "null" else str(_decode_typed_cell(row[0])),
                row[0][0],
            )
        )
    return canonical_json(
        {
            "format": SNAPSHOT_FORMAT_V1,
            "columns": ordered_cols,
            "rows": typed_rows,
        }
    )


def decode_dataset_snapshot(raw_bytes: bytes) -> tuple[list[str], list[dict[str, Any]]]:
    """Decode `norn.snapshot/v1` bytes, refusing any non-canonical encoding."""
    if not isinstance(raw_bytes, (bytes, bytearray)):
        raise TypeError("snapshot payload must be bytes")
    raw = bytes(raw_bytes)
    doc = json.loads(raw.decode("utf-8"), parse_float=Decimal)
    if not isinstance(doc, dict) or set(doc.keys()) != {"format", "columns", "rows"}:
        raise ValueError("dataset snapshot must contain exactly format, columns, and rows")
    if doc.get("format") != SNAPSHOT_FORMAT_V1:
        raise ValueError(f"dataset snapshot must declare format {SNAPSHOT_FORMAT_V1!r}")
    cols = doc.get("columns")
    rows = doc.get("rows")
    if not isinstance(cols, list) or not all(isinstance(c, str) for c in cols):
        raise ValueError("dataset snapshot columns must be a list of strings")
    for col in cols:
        _validate_column_name(col, "snapshot column")
    if len(set(cols)) != len(cols):
        raise ValueError("dataset snapshot contains duplicate column names")
    if not isinstance(rows, list):
        raise ValueError("dataset snapshot rows must be a list")

    decoded_rows: list[dict[str, Any]] = []
    canonical_rows: list[list[list[Any]]] = []
    for row in rows:
        if not isinstance(row, list) or len(row) != len(cols):
            raise ValueError("dataset snapshot row length does not match columns")
        decoded_row: dict[str, Any] = {}
        canon_row: list[list[Any]] = []
        for col, cell in zip(cols, row, strict=True):
            val = _decode_typed_cell(cell)
            decoded_row[col] = val
            canon_row.append(_encode_typed_cell(val))
        decoded_rows.append(decoded_row)
        canonical_rows.append(canon_row)

    if "id" in cols:
        if cols[0] != "id":
            raise ValueError("dataset snapshot with 'id' column must place 'id' first")
        expected_order = sorted(
            canonical_rows,
            key=lambda r: (
                "" if r[0][0] == "null" else str(_decode_typed_cell(r[0])),
                r[0][0],
            ),
        )
        if canonical_rows != expected_order:
            raise ValueError("dataset snapshot rows are not canonically sorted by 'id'")

    expected_bytes = canonical_json(
        {
            "format": SNAPSHOT_FORMAT_V1,
            "columns": cols,
            "rows": canonical_rows,
        }
    )
    if raw != expected_bytes:
        raise ValueError("dataset snapshot bytes are not Norn1 canonical")
    return cols, decoded_rows


def dataset_snapshot_blob(
    source: Any,
    columns: Sequence[str],
    *,
    dataset_revision: str | None = None,
) -> dict[str, Any]:
    """Compute a `BlobRef` over canonical `norn.snapshot/v1` bytes (or encode a frame first)."""
    if isinstance(source, (bytes, bytearray)):
        raw = bytes(source)
        cols, _ = decode_dataset_snapshot(raw)
        missing = [c for c in columns if c not in cols]
        if missing:
            raise ValueError(f"dataset snapshot bytes missing required columns: {missing}")
    else:
        raw = encode_dataset_snapshot(source, columns)
    return {
        "digest": f"sha256:{hashlib.sha256(raw).hexdigest()}",
        "version": dataset_revision,
        "sizeBytes": len(raw),
    }


# ---------------------------------------------------------------------------
# Registered model artifact format (`norn.linear-model/v1`) and exact scoring
# ---------------------------------------------------------------------------


def encode_model_artifact(
    *,
    model_id: str,
    target: str,
    features: Sequence[str],
    intercept: Decimal | int | str,
    coefficients: Mapping[str, Decimal | int | str],
    contracts: Mapping[str, Decimal | int | str | None] | None = None,
) -> bytes:
    """Serialize a declarative `norn.linear-model/v1` artifact to canonical JSON bytes."""
    valid_target, valid_features, _ = _validate_eval_columns(target, features, None)
    if set(coefficients.keys()) != set(valid_features):
        raise ValueError("model artifact coefficients must match declared features exactly")
    norm_coeffs = {
        feat: parse_exact_decimal(coefficients[feat], max_scale=18)
        for feat in valid_features
    }
    raw_contracts = {} if contracts is None else dict(contracts)
    allowed_contracts = {"minPrediction", "maxPrediction", "maxWape", "maxMae", "minRSquared"}
    unknown = sorted(set(raw_contracts.keys()) - allowed_contracts)
    if unknown:
        raise ValueError(f"unsupported model artifact contract keys: {unknown}")
    norm_contracts = {
        key: (None if raw_contracts[key] is None else parse_exact_decimal(raw_contracts[key], max_scale=18))
        for key in sorted(raw_contracts.keys())
    }
    return canonical_json(
        {
            "format": MODEL_ARTIFACT_FORMAT_V1,
            "modelId": model_id,
            "target": valid_target,
            "features": valid_features,
            "intercept": parse_exact_decimal(intercept, max_scale=18),
            "coefficients": norm_coeffs,
            "contracts": norm_contracts,
        }
    )


def decode_model_artifact(raw_bytes: bytes) -> dict[str, Any]:
    """Decode and validate raw `norn.linear-model/v1` model artifact bytes."""
    if not isinstance(raw_bytes, (bytes, bytearray)):
        raise TypeError("model_artifact_bytes must be bytes")
    doc = json.loads(raw_bytes.decode("utf-8"), parse_float=Decimal)
    if not isinstance(doc, dict) or doc.get("format") != MODEL_ARTIFACT_FORMAT_V1:
        raise ValueError(f"model artifact must declare format {MODEL_ARTIFACT_FORMAT_V1!r}")
    model_id = doc.get("modelId")
    if not isinstance(model_id, str) or not model_id.strip():
        raise ValueError("model artifact missing non-empty modelId")
    valid_target, valid_features, _ = _validate_eval_columns(
        doc.get("target", ""),
        doc.get("features", []),
        None,
    )
    raw_coeffs = doc.get("coefficients")
    if not isinstance(raw_coeffs, dict) or set(raw_coeffs.keys()) != set(valid_features):
        raise ValueError("model artifact coefficients must match declared features")
    coefficients = {
        feat: parse_exact_decimal(raw_coeffs[feat], max_scale=18)
        for feat in valid_features
    }
    intercept = parse_exact_decimal(doc.get("intercept"), max_scale=18)
    raw_contracts = doc.get("contracts", {})
    if not isinstance(raw_contracts, dict):
        raise ValueError("model artifact contracts must be an object")
    allowed_contracts = {"minPrediction", "maxPrediction", "maxWape", "maxMae", "minRSquared"}
    unknown = sorted(set(raw_contracts.keys()) - allowed_contracts)
    if unknown:
        raise ValueError(f"unsupported model artifact contract keys: {unknown}")
    contracts = {
        key: (None if val is None else parse_exact_decimal(val, max_scale=18))
        for key, val in raw_contracts.items()
    }
    return {
        "format": MODEL_ARTIFACT_FORMAT_V1,
        "modelId": model_id,
        "target": valid_target,
        "features": valid_features,
        "intercept": intercept,
        "coefficients": coefficients,
        "contracts": contracts,
    }


def model_artifact_blob(artifact_bytes: bytes, *, version_id: str | None = None) -> dict[str, Any]:
    """Compute a `BlobRef` content digest and size over raw model artifact bytes."""
    if not isinstance(artifact_bytes, (bytes, bytearray)):
        raise TypeError("artifact_bytes must be bytes")
    raw = bytes(artifact_bytes)
    return {
        "digest": f"sha256:{hashlib.sha256(raw).hexdigest()}",
        "version": version_id,
        "sizeBytes": len(raw),
    }


def _to_exact_eval_decimal(value: Any, label: str) -> Decimal:
    if value is None or isinstance(value, bool):
        raise ValueError(f"missing or non-numeric value in column {label!r}: {value!r}")
    if isinstance(value, Decimal):
        return _reduce_decimal(value)
    if isinstance(value, int):
        return _int_to_exact_decimal(value)
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueError(f"non-finite float in column {label!r}: {value!r}")
        return _reduce_decimal(Decimal(repr(value)))
    if isinstance(value, str):
        try:
            return _reduce_decimal(Decimal(value.strip()))
        except InvalidOperation as exc:
            raise ValueError(f"non-numeric string in column {label!r}: {value!r}") from exc
    raise ValueError(f"unsupported numeric value in column {label!r}: {value!r}")


def _decimal_to_exact_fraction(value: Decimal) -> Fraction:
    """Convert any finite `Decimal` to an exact `Fraction` without ambient context rounding."""
    negative, significant, exponent = _exact_decimal_parts(value)
    if not significant:
        return Fraction(0)
    integer = int(significant)
    if negative:
        integer = -integer
    if exponent >= 0:
        return Fraction(integer * (10**exponent), 1)
    return Fraction(integer, 10 ** (-exponent))


def _fraction_to_decimal_scale9(value: Fraction) -> Decimal:
    """Round an exact `Fraction` to scale <= 9 (half-even) without touching ambient Decimal context."""
    negative = value < 0
    num = -value.numerator if negative else value.numerator
    den = value.denominator
    scaled = num * 1_000_000_000
    quotient, remainder = divmod(scaled, den)
    twice_rem = remainder * 2
    if twice_rem > den or (twice_rem == den and quotient % 2 == 1):
        quotient += 1
    if quotient == 0:
        return Decimal(0)
    exact_q = _int_to_exact_decimal(quotient)
    _, digits_str, exp = _exact_decimal_parts(exact_q)
    return _reduce_decimal(
        Decimal((1 if negative else 0, tuple(int(ch) for ch in digits_str), exp - 9))
    )


def _score_model_artifact(
    model: Mapping[str, Any],
    rows: list[dict[str, Any]],
    *,
    target: str,
    features: list[str],
    metric_name: str,
) -> tuple[Decimal, list[EvaluationCheck], dict[str, Any]]:
    """Score the registered model artifact with exact rational arithmetic and evaluate its contracts."""
    if not rows:
        raise ValueError("evaluated dataset snapshot must contain at least one row")
    intercept = _decimal_to_exact_fraction(model["intercept"])
    coefficients = {
        feat: _decimal_to_exact_fraction(coef)
        for feat, coef in model["coefficients"].items()
    }
    contracts = {
        key: (None if val is None else _decimal_to_exact_fraction(val))
        for key, val in model["contracts"].items()
    }

    actuals: list[Fraction] = []
    predictions: list[Fraction] = []
    abs_errors: list[Fraction] = []
    sq_errors: list[Fraction] = []

    for row in rows:
        y = _decimal_to_exact_fraction(_to_exact_eval_decimal(row[target], target))
        y_hat = intercept
        for feat in features:
            x_val = _decimal_to_exact_fraction(_to_exact_eval_decimal(row[feat], feat))
            y_hat += coefficients[feat] * x_val
        err = y - y_hat
        actuals.append(y)
        predictions.append(y_hat)
        abs_errors.append(abs(err))
        sq_errors.append(err * err)

    n = Fraction(len(rows), 1)
    sum_abs_err = sum(abs_errors, Fraction(0))
    sum_abs_y = sum((abs(y) for y in actuals), Fraction(0))
    mae = sum_abs_err / n
    wape = (sum_abs_err / sum_abs_y) if sum_abs_y > 0 else None
    y_mean = sum(actuals, Fraction(0)) / n
    ss_res = sum(sq_errors, Fraction(0))
    ss_tot = sum(((y - y_mean) * (y - y_mean) for y in actuals), Fraction(0))
    r_squared = (Fraction(1) - (ss_res / ss_tot)) if ss_tot > 0 else None

    if metric_name == "wape":
        if wape is None:
            raise ValueError("cannot compute wape when sum(|target|) == 0")
        raw_metric = wape
    elif metric_name == "mae":
        raw_metric = mae
    elif metric_name == "r_squared":
        if r_squared is None:
            raise ValueError("cannot compute r_squared on constant target")
        raw_metric = r_squared
    else:
        raise ValueError(f"unsupported metric_name {metric_name!r}; expected one of {sorted(SUPPORTED_METRICS)}")

    metric_value = _fraction_to_decimal_scale9(raw_metric)

    checks: list[EvaluationCheck] = []
    min_pred = contracts.get("minPrediction")
    if min_pred is not None:
        worst = min(predictions)
        passed = worst >= min_pred
        checks.append(
            EvaluationCheck(
                name="minPrediction",
                status="pass" if passed else "fail",
                detail=f"min(prediction)={worst} vs minPrediction={min_pred}",
            )
        )
    max_pred = contracts.get("maxPrediction")
    if max_pred is not None:
        worst = max(predictions)
        passed = worst <= max_pred
        checks.append(
            EvaluationCheck(
                name="maxPrediction",
                status="pass" if passed else "fail",
                detail=f"max(prediction)={worst} vs maxPrediction={max_pred}",
            )
        )
    max_wape = contracts.get("maxWape")
    if max_wape is not None:
        passed = wape is not None and wape <= max_wape
        checks.append(
            EvaluationCheck(
                name="maxWape",
                status="pass" if passed else "fail",
                detail=f"wape={wape} vs maxWape={max_wape}",
            )
        )
    max_mae = contracts.get("maxMae")
    if max_mae is not None:
        passed = mae <= max_mae
        checks.append(
            EvaluationCheck(
                name="maxMae",
                status="pass" if passed else "fail",
                detail=f"mae={mae} vs maxMae={max_mae}",
            )
        )
    min_r2 = contracts.get("minRSquared")
    if min_r2 is not None:
        passed = r_squared is not None and r_squared >= min_r2
        checks.append(
            EvaluationCheck(
                name="minRSquared",
                status="pass" if passed else "fail",
                detail=f"r_squared={r_squared} vs minRSquared={min_r2}",
            )
        )
    if not checks:
        checks.append(
            EvaluationCheck(
                name="finite_predictions",
                status="pass",
                detail=f"scored {len(rows)} rows with exact rational predictions",
            )
        )

    diagnostics = {
        "n": len(rows),
        "predictions": predictions,
        "residuals": [y - y_hat for y, y_hat in zip(actuals, predictions, strict=True)],
        "mae": mae,
        "wape": wape,
        "r_squared": r_squared,
    }
    return metric_value, checks, diagnostics


# ---------------------------------------------------------------------------
# Invocation, receipt, and end-to-end evaluator entry point
# ---------------------------------------------------------------------------


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
    model_blob: Mapping[str, Any],
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
    """Build a schema-valid `FunctionInvocation` pinning evaluator, dataset snapshot, model artifact, and scope."""
    valid_target, valid_features, valid_order = _validate_eval_columns(target, features, order)
    if metric_name not in SUPPORTED_METRICS:
        raise ValueError(f"unsupported metric_name {metric_name!r}; expected one of {sorted(SUPPORTED_METRICS)}")
    resolved_manifest = FORECAST_EVALUATOR_MANIFEST if manifest is None else manifest

    dataset_digest = _require_digest(dataset_blob.get("digest"), "dataset_blob.digest")
    dataset_size = dataset_blob.get("sizeBytes")
    if isinstance(dataset_size, bool) or not isinstance(dataset_size, int) or dataset_size < 0:
        raise ValueError(f"dataset_blob.sizeBytes must be a non-negative int, got {dataset_size!r}")
    dataset_version = dataset_blob.get("version", dataset_revision)
    if dataset_version != dataset_revision:
        raise ValueError(
            f"dataset_blob.version {dataset_version!r} must match dataset_revision {dataset_revision!r}"
        )

    model_digest = _require_digest(model_blob.get("digest"), "model_blob.digest")
    model_size = model_blob.get("sizeBytes")
    if isinstance(model_size, bool) or not isinstance(model_size, int) or model_size < 0:
        raise ValueError(f"model_blob.sizeBytes must be a non-negative int, got {model_size!r}")
    model_version = model_blob.get("version", version_id)
    if model_version != version_id:
        raise ValueError(f"model_blob.version {model_version!r} must match version_id {version_id!r}")

    all_blobs: dict[str, dict[str, Any]] = {} if blobs is None else dict(blobs)
    all_blobs["dataset"] = {
        "digest": dataset_digest,
        "version": dataset_revision,
        "sizeBytes": dataset_size,
    }
    all_blobs["model"] = {
        "digest": model_digest,
        "version": version_id,
        "sizeBytes": model_size,
    }
    inputs: dict[str, Any] = {
        "dataset_revision": dataset_revision,
        "evaluation_run": evaluation_run,
        "features": ",".join(valid_features),
        "metric_name": metric_name,
        "model_id": model_id,
        "order": valid_order,
        "target": valid_target,
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
    suite_version: str = EVALUATOR_SUITE_VERSION,
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
    blobs = invocation.get("blobs", {})
    if "dataset" not in blobs or "model" not in blobs:
        raise ValueError("invocation must pin both blobs['dataset'] and blobs['model'] content digests")
    _require_digest(blobs["dataset"].get("digest"), "invocation.blobs.dataset.digest")
    _require_digest(blobs["model"].get("digest"), "invocation.blobs.model.digest")
    inputs = invocation.get("inputs", {})
    if inputs.get("dataset_revision") != evaluation_revision:
        raise ValueError(
            f"evaluation_revision {evaluation_revision!r} does not match invocation dataset_revision {inputs.get('dataset_revision')!r}"
        )
    if inputs.get("metric_name") != metric_name:
        raise ValueError(
            f"metric_name {metric_name!r} does not match invocation metric_name {inputs.get('metric_name')!r}"
        )
    if blobs["dataset"].get("version") != evaluation_revision:
        raise ValueError(
            f"blobs.dataset.version {blobs['dataset'].get('version')!r} does not match evaluation_revision {evaluation_revision!r}"
        )
    if blobs["model"].get("version") != inputs.get("version_id"):
        raise ValueError(
            f"blobs.model.version {blobs['model'].get('version')!r} does not match version_id {inputs.get('version_id')!r}"
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
    snapshot_or_frame: Any,
    model_artifact_bytes: bytes,
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
    metric_name: str = "wape",
    order: str | None = None,
    authority_kind: str = "service",
    generation: int = 1,
    executor: str = "pool.ml-evaluator",
    placement: str = "ml-evaluator/node-1",
    fact_pins: list[dict[str, Any]] | None = None,
    deadline_millis: int = 1_000_000,
    expected_dataset_digest: str | None = None,
    expected_model_digest: str | None = None,
    dispatched_invocation: Mapping[str, Any] | None = None,
) -> tuple[dict[str, Any], dict[str, Any], list[EvaluationCheck], dict[str, Any]]:
    """Score the pinned registered model artifact on the hashed snapshot and emit a `FunctionReceipt`."""
    valid_target, valid_features, valid_order = _validate_eval_columns(target, features, order)
    if metric_name not in SUPPORTED_METRICS:
        raise ValueError(
            f"unsupported metric_name {metric_name!r}; expected one of {sorted(SUPPORTED_METRICS)}"
        )
    if isinstance(generation, bool) or not isinstance(generation, int) or generation <= 0:
        raise ValueError(f"generation must be a positive int, got {generation!r}")

    columns = [valid_target, *valid_features] + (
        [valid_order] if valid_order is not None and valid_order not in (valid_target, *valid_features) else []
    )
    if isinstance(snapshot_or_frame, (bytes, bytearray)):
        snapshot_bytes = bytes(snapshot_or_frame)
    else:
        snapshot_bytes = encode_dataset_snapshot(snapshot_or_frame, columns)

    actual_dataset_blob = dataset_snapshot_blob(snapshot_bytes, columns, dataset_revision=dataset_revision)
    actual_model_blob = model_artifact_blob(model_artifact_bytes, version_id=version_id)

    if expected_dataset_digest is not None and actual_dataset_blob["digest"] != expected_dataset_digest:
        raise ValueError(
            f"evaluated dataset snapshot {actual_dataset_blob['digest']} does not match expected {expected_dataset_digest}"
        )
    if expected_model_digest is not None and actual_model_blob["digest"] != expected_model_digest:
        raise ValueError(
            f"registered model artifact {actual_model_blob['digest']} does not match expected {expected_model_digest}"
        )

    if dispatched_invocation is not None:
        if dispatched_invocation.get("manifest") != FORECAST_EVALUATOR_MANIFEST_DIGEST:
            raise ValueError("dispatched invocation manifest does not match forecast evaluator")
        if dispatched_invocation.get("world") != "main":
            raise ValueError(
                f"dispatched invocation world must be 'main', got {dispatched_invocation.get('world')!r}"
            )
        if dispatched_invocation.get("tenant") != tenant:
            raise ValueError("tenant does not match dispatched invocation")
        if dispatched_invocation.get("commandId") != command_id:
            raise ValueError("command_id does not match dispatched invocation")
        dispatched_run = dispatched_invocation.get("run")
        if (
            not isinstance(dispatched_run, Mapping)
            or dispatched_run.get("id") != version_id
            or isinstance(dispatched_run.get("generation"), bool)
            or dispatched_run.get("generation") != generation
        ):
            raise ValueError(
                f"run does not match dispatched invocation: expected id={version_id!r}, generation={generation!r}, got {dispatched_run!r}"
            )
        dispatched_authority = dispatched_invocation.get("authority")
        if (
            not isinstance(dispatched_authority, Mapping)
            or dispatched_authority.get("kind") != authority_kind
            or dispatched_authority.get("id") != authority_id
        ):
            raise ValueError(
                f"authority does not match dispatched invocation: expected kind={authority_kind!r}, id={authority_id!r}, got {dispatched_authority!r}"
            )

        pinned_dataset = dispatched_invocation.get("blobs", {}).get("dataset")
        if not isinstance(pinned_dataset, Mapping) or pinned_dataset.get("digest") != actual_dataset_blob["digest"]:
            raise ValueError(
                f"evaluated dataset snapshot {actual_dataset_blob['digest']} does not match dispatched blob pin {pinned_dataset!r}"
            )
        if pinned_dataset.get("sizeBytes") != actual_dataset_blob["sizeBytes"]:
            raise ValueError("evaluated dataset snapshot size does not match dispatched blob pin")
        if pinned_dataset.get("version") != dataset_revision:
            raise ValueError(
                f"dispatched blobs.dataset.version {pinned_dataset.get('version')!r} does not match dataset_revision {dataset_revision!r}"
            )

        pinned_model = dispatched_invocation.get("blobs", {}).get("model")
        if not isinstance(pinned_model, Mapping) or pinned_model.get("digest") != actual_model_blob["digest"]:
            raise ValueError(
                f"registered model artifact {actual_model_blob['digest']} does not match dispatched model blob pin {pinned_model!r}"
            )
        if pinned_model.get("sizeBytes") != actual_model_blob["sizeBytes"]:
            raise ValueError("registered model artifact size does not match dispatched model blob pin")
        if pinned_model.get("version") != version_id:
            raise ValueError(
                f"dispatched blobs.model.version {pinned_model.get('version')!r} does not match version_id {version_id!r}"
            )

        dispatched_inputs = dispatched_invocation.get("inputs")
        expected_inputs = {
            "dataset_revision": dataset_revision,
            "evaluation_run": evaluation_run,
            "features": ",".join(valid_features),
            "metric_name": metric_name,
            "model_id": model_id,
            "order": valid_order,
            "target": valid_target,
            "version_id": version_id,
        }
        if not isinstance(dispatched_inputs, Mapping) or set(dispatched_inputs.keys()) != set(expected_inputs.keys()):
            raise ValueError(
                f"dispatched invocation inputs must declare exactly {sorted(expected_inputs.keys())}"
            )
        for key, expected_val in expected_inputs.items():
            if dispatched_inputs[key] != expected_val:
                raise ValueError(
                    f"evaluation setting {key!r}={expected_val!r} does not match dispatched invocation {dispatched_inputs[key]!r}"
                )
        invocation: dict[str, Any] = dict(dispatched_invocation)
    else:
        invocation = build_evaluation_invocation(
            tenant=tenant,
            authority_id=authority_id,
            authority_kind=authority_kind,
            generation=generation,
            command_id=command_id,
            version_id=version_id,
            model_id=model_id,
            evaluation_run=evaluation_run,
            dataset_revision=dataset_revision,
            dataset_blob=actual_dataset_blob,
            model_blob=actual_model_blob,
            metric_name=metric_name,
            target=valid_target,
            features=valid_features,
            order=valid_order,
            fact_pins=fact_pins,
            deadline_millis=deadline_millis,
        )

    # Decode strictly from the exact hashed canonical bytes (never unhashed inputs).
    _, decoded_rows = decode_dataset_snapshot(snapshot_bytes)
    model = decode_model_artifact(model_artifact_bytes)
    if model["modelId"] != model_id:
        raise ValueError(
            f"model artifact modelId {model['modelId']!r} does not match invocation model_id {model_id!r}"
        )
    if model["target"] != valid_target or model["features"] != valid_features:
        raise ValueError(
            "model artifact target/features do not match invocation target/features"
        )

    metric_value, checks, diagnostics = _score_model_artifact(
        model,
        decoded_rows,
        target=valid_target,
        features=valid_features,
        metric_name=metric_name,
    )
    checks_failed = sum(1 for check in checks if check.status == "fail")
    checks_passed = sum(1 for check in checks if check.status == "pass")
    contracts_passed = checks_failed == 0
    receipt = build_evaluation_receipt(
        invocation,
        evaluation_revision=dataset_revision,
        metric_name=metric_name,
        metric_value=metric_value,
        contracts_passed=contracts_passed,
        checks_passed=checks_passed,
        checks_failed=checks_failed,
        suite_version=EVALUATOR_SUITE_VERSION,
        executor=executor,
        placement=placement,
    )
    return invocation, receipt, checks, diagnostics
