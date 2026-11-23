"""Fail-closed PII, IEEE-float, and canonical JSON boundary for `norn.solver.rostering@0.1.0`.

Every ingress payload passes through this boundary before reaching the solver:
- Any marked PII key anywhere in the payload (recursively, case-insensitively) is refused
  with `NORN-SOLVER-PII-FORBIDDEN`.
- Any IEEE 754 `float` (including JSON decimal literals, `NaN`, `Infinity`) is refused
  with `NORN-SOLVER-FLOAT-FORBIDDEN`.
- All business quantities are parsed into exact integers (minutes, minor currency units)
  or exact `Decimal` values.
"""

from __future__ import annotations

import hashlib
import json
from datetime import UTC, datetime, timedelta
from decimal import Decimal, InvalidOperation
from typing import Any

FORBIDDEN_PII_FIELDS: frozenset[str] = frozenset(
    {
        "address",
        "bank_account",
        "base_salary",
        "date_of_birth",
        "dob",
        "email",
        "gender",
        "hourly_overtime_rate",
        "medical_note",
        "national_id",
        "nationality",
        "passport_number",
        "phone",
        "religion",
        "salary",
    }
)


class SolverContractError(ValueError):
    """Deterministic contract refusal with stable Norn diagnostic code."""

    def __init__(self, code: str, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"[{code}] {message}")


class PiiViolationError(SolverContractError):
    """Raised when a marked PII key appears anywhere in an unmarked solver payload."""

    def __init__(self, field_path: str) -> None:
        self.field_path = field_path
        super().__init__(
            "NORN-SOLVER-PII-FORBIDDEN",
            f"marked PII field {field_path!r} is forbidden in the unmarked rostering solver slice",
        )


class FloatRefusalError(SolverContractError):
    """Raised when an IEEE floating-point number appears anywhere in a solver payload."""

    def __init__(self, field_path: str, value: object) -> None:
        self.field_path = field_path
        self.rejected_value = value
        super().__init__(
            "NORN-SOLVER-FLOAT-FORBIDDEN",
            f"floating-point value {value!r} at {field_path!r} is refused; pass exact integers or decimal strings",
        )


class PostVerificationError(SolverContractError):
    """Raised when the independent hard-law post-verifier detects a constraint violation."""

    def __init__(self, violations: tuple[str, ...]) -> None:
        self.violations = violations
        joined = "; ".join(violations)
        super().__init__(
            "NORN-SOLVER-POSTVERIFY-FAILED",
            f"independent hard-law post-verification failed ({len(violations)} violation(s)): {joined}",
        )


def reject_pii_and_floats(value: Any, path: str = "$") -> None:
    """Recursively reject any IEEE float or marked PII key in a decoded Python structure."""
    if isinstance(value, float):
        raise FloatRefusalError(path, value)
    if isinstance(value, dict):
        for k, v in value.items():
            key_str = str(k)
            child_path = f"{path}.{key_str}"
            if key_str.lower() in FORBIDDEN_PII_FIELDS:
                raise PiiViolationError(child_path)
            reject_pii_and_floats(v, child_path)
    elif isinstance(value, (list, tuple)):
        for idx, item in enumerate(value):
            reject_pii_and_floats(item, f"{path}[{idx}]")


def reject_floats_only(value: Any, path: str = "$") -> None:
    """Recursively reject any IEEE float before canonical JSON serialization."""
    if isinstance(value, float):
        raise FloatRefusalError(path, value)
    if isinstance(value, dict):
        for k, v in value.items():
            reject_floats_only(v, f"{path}.{k}")
    elif isinstance(value, (list, tuple)):
        for idx, item in enumerate(value):
            reject_floats_only(item, f"{path}[{idx}]")


def _refuse_json_float(raw_token: str) -> Any:
    raise FloatRefusalError("$", raw_token)


def _refuse_json_constant(raw_token: str) -> Any:
    raise FloatRefusalError("$", raw_token)


def parse_strict_json(raw: str | bytes) -> Any:
    """Decode JSON while refusing IEEE floats, NaN/Infinity, and marked PII keys."""
    text = raw.decode("utf-8") if isinstance(raw, bytes) else raw
    try:
        parsed = json.loads(
            text,
            parse_float=_refuse_json_float,
            parse_constant=_refuse_json_constant,
        )
    except SolverContractError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise SolverContractError("NORN-SOLVER-JSON", f"invalid JSON payload: {exc}") from exc
    reject_pii_and_floats(parsed, "$")
    return parsed


def parse_decimal(raw: Any, path: str, *, allow_negative: bool = False) -> Decimal:
    """Parse an exact scale-2 Decimal from Decimal, int, or string; refuse floats and bools."""
    if isinstance(raw, float):
        raise FloatRefusalError(path, raw)
    if isinstance(raw, bool):
        raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} must be a decimal string or int, got bool")
    if isinstance(raw, Decimal):
        val = raw
    elif isinstance(raw, (int, str)):
        try:
            val = Decimal(str(raw))
        except InvalidOperation as exc:
            raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} is not a valid decimal: {raw!r}") from exc
    else:
        raise SolverContractError(
            "NORN-SOLVER-SCHEMA",
            f"{path} must be Decimal, int, or str, got {type(raw).__name__}",
        )
    if not val.is_finite():
        raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} must be finite, got {val!r}")
    if not allow_negative and val < Decimal("0"):
        raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} must be non-negative, got {val}")
    return val.quantize(Decimal("0.01"))


def decimal_to_minor_units(value: Decimal, path: str) -> int:
    """Convert an exact Decimal currency amount (2 decimal places) to integer minor units."""
    scaled = value * Decimal(100)
    if scaled != scaled.to_integral_value():
        raise SolverContractError(
            "NORN-SOLVER-SCHEMA",
            f"{path} must have at most 2 decimal places for exact minor units, got {value}",
        )
    return int(scaled)


def minor_units_to_decimal(minor: int) -> Decimal:
    """Format exact integer minor currency units as a scale-2 Decimal."""
    return (Decimal(minor) / Decimal(100)).quantize(Decimal("0.01"))


def hours_to_minutes(hours: Decimal, path: str) -> int:
    """Convert exact Decimal hours to exact integer minutes (refuses fractional minutes)."""
    scaled = hours * Decimal(60)
    if scaled != scaled.to_integral_value():
        raise SolverContractError(
            "NORN-SOLVER-SCHEMA",
            f"{path} must correspond to an exact whole number of minutes, got {hours}h",
        )
    return int(scaled)


def minutes_to_hours(minutes: int) -> Decimal:
    """Convert exact integer minutes to scale-2 Decimal hours."""
    return (Decimal(minutes) / Decimal(60)).quantize(Decimal("0.01"))


def parse_int(raw: Any, path: str, *, minimum: int = 0) -> int:
    """Parse a strict integer (rejecting bools and floats)."""
    if isinstance(raw, float):
        raise FloatRefusalError(path, raw)
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} must be an integer, got {raw!r}")
    if raw < minimum:
        raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} must be >= {minimum}, got {raw}")
    return raw


def parse_bool(raw: Any, path: str) -> bool:
    """Parse a strict boolean."""
    if not isinstance(raw, bool):
        raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} must be a boolean, got {raw!r}")
    return raw


def parse_utc_timestamp(raw: Any, path: str) -> datetime:
    """Parse an explicit-timezone ISO-8601 timestamp normalized to UTC."""
    if isinstance(raw, datetime):
        dt = raw
    elif isinstance(raw, str):
        text = raw[:-1] + "+00:00" if raw.endswith("Z") else raw
        try:
            dt = datetime.fromisoformat(text)
        except ValueError as exc:
            raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} invalid ISO-8601 timestamp: {raw!r}") from exc
    else:
        raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} must be ISO-8601 timestamp string or datetime")
    if dt.tzinfo is None:
        raise SolverContractError(
            "NORN-SOLVER-SCHEMA",
            f"{path} timestamp must include timezone/UTC offset: {raw!r}",
        )
    if dt.microsecond != 0:
        raise SolverContractError(
            "NORN-SOLVER-INTERVAL",
            f"{path} timestamp must not contain sub-second microseconds: {raw!r}",
        )
    return dt.astimezone(UTC)


def timedelta_exact_seconds(delta: timedelta) -> int:
    """Compute exact integer seconds from a timedelta without invoking float `.total_seconds()`."""
    if delta.microseconds != 0:
        raise SolverContractError(
            "NORN-SOLVER-INTERVAL",
            "time intervals must have zero sub-second microseconds",
        )
    return delta.days * 86_400 + delta.seconds


def timedelta_exact_minutes(delta: timedelta, path: str = "interval") -> int:
    """Compute exact integer minutes from a timedelta without using IEEE float."""
    secs = timedelta_exact_seconds(delta)
    if secs % 60 != 0:
        raise SolverContractError(
            "NORN-SOLVER-INTERVAL",
            f"{path} duration ({secs}s) must be an exact whole number of minutes",
        )
    return secs // 60


def format_utc(dt: datetime) -> str:
    """Format a UTC datetime in canonical ISO-8601 Z form."""
    return dt.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def _json_default(obj: Any) -> str:
    if isinstance(obj, Decimal):
        return format(obj.quantize(Decimal("0.01")), "f")
    if isinstance(obj, datetime):
        return format_utc(obj)
    raise TypeError(f"Non-serializable value of type {type(obj).__name__}")


def canonical_json(obj: Any) -> str:
    """Deterministic UTF-8 canonical JSON (sorted keys, exact Decimal strings, zero floats)."""
    reject_floats_only(obj, "$")
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False, default=_json_default)


def sha256_digest(obj: Any) -> str:
    """Canonical SHA-256 content digest (`sha256:<64 hex>`)."""
    return "sha256:" + hashlib.sha256(canonical_json(obj).encode("utf-8")).hexdigest()


def domain_sha256_digest(domain: bytes, obj: Any) -> str:
    """Domain-separated SHA-256 content digest matching Norn1 canonical hashing."""
    payload = domain + canonical_json(obj).encode("utf-8")
    return "sha256:" + hashlib.sha256(payload).hexdigest()
