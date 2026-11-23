"""Fail-closed PII, closed-schema allowlist, IEEE-float, and canonical JSON boundary for `norn.solver.rostering@0.1.0`.

Every ingress payload passes through this boundary before reaching the solver:
- Any marked PII key anywhere in the payload (recursively, case-insensitively and
  normalized across snake_case / camelCase / kebab-case) is refused with
  `NORN-SOLVER-PII-FORBIDDEN`.
- Every JSON object level (perform envelope, top-level request, employee, leave window,
  shift requirement, preference profile) is checked against a strict closed schema
  allowlist; any unallowlisted key fails closed with `NORN-SOLVER-UNKNOWN-FIELD` and
  its exact dotted JSON path.
- Unexpected non-dict / non-list container shapes fail closed with `NORN-SOLVER-SCHEMA`.
- Any IEEE 754 `float` (including JSON decimal literals, `NaN`, `Infinity`) is refused
  with `NORN-SOLVER-FLOAT-FORBIDDEN`.
- All business quantities are parsed into exact integers (minutes, minor currency units)
  or exact `Decimal` values.
"""

from __future__ import annotations

import hashlib
import json
import re
from datetime import UTC, datetime, timedelta
from decimal import Decimal, InvalidOperation
from typing import Any

_CAMEL_BOUNDARY_1 = re.compile(r"([a-z0-9])([A-Z])")
_CAMEL_BOUNDARY_2 = re.compile(r"([A-Z]+)([A-Z][a-z])")
_NON_ALNUM = re.compile(r"[^a-z0-9]+")


def normalize_field_key(key: str) -> str:
    """Normalize a field key to canonical lowercase snake_case for PII matching."""
    step1 = _CAMEL_BOUNDARY_1.sub(r"\1_\2", key)
    step2 = _CAMEL_BOUNDARY_2.sub(r"\1_\2", step1)
    return _NON_ALNUM.sub("_", step2.lower()).strip("_")


FORBIDDEN_PII_FIELDS: frozenset[str] = frozenset(
    {
        # Personal names
        "arabic_name",
        "employee_name",
        "english_name",
        "family_name",
        "first_name",
        "full_name",
        "full_name_ar",
        "full_name_en",
        "given_name",
        "last_name",
        "legal_name",
        "middle_name",
        "name",
        "surname",
        # Contact & address
        "address",
        "cell_phone",
        "email",
        "email_address",
        "emergency_contact",
        "emergency_contact_name",
        "emergency_contact_phone",
        "emergency_phone",
        "home_address",
        "mailing_address",
        "mobile",
        "mobile_no",
        "mobile_number",
        "next_of_kin",
        "personal_email",
        "phone",
        "phone_no",
        "phone_number",
        "residential_address",
        "street_address",
        "telephone",
        "whatsapp",
        "whatsapp_number",
        "work_email",
        # Government, residency, tax & insurance IDs
        "civil_id",
        "emirates_id",
        "gov_id",
        "government_id",
        "id_number",
        "iqama",
        "iqama_no",
        "iqama_number",
        "national_id",
        "national_id_number",
        "nid",
        "passport",
        "passport_no",
        "passport_number",
        "residency_id",
        "resident_id",
        "social_insurance_number",
        "social_security_number",
        "ssn",
        "tax_id",
        "tin",
        "visa_number",
        # Banking & compensation
        "annual_salary",
        "bank_account",
        "bank_account_number",
        "bank_iban",
        "bank_name",
        "base_salary",
        "basic_salary",
        "compensation",
        "gross_salary",
        "hourly_overtime_rate",
        "hourly_rate",
        "hourly_wage",
        "iban",
        "monthly_salary",
        "net_salary",
        "pay_rate",
        "payroll_account",
        "routing_number",
        "salary",
        "swift_code",
        "wage",
        # Demographic & medical
        "birth_date",
        "birthdate",
        "blood_type",
        "citizenship",
        "date_of_birth",
        "diagnosis",
        "disability",
        "dob",
        "ethnicity",
        "gender",
        "health_condition",
        "health_note",
        "marital_status",
        "medical_info",
        "medical_note",
        "medical_notes",
        "medical_record",
        "nationality",
        "place_of_birth",
        "race",
        "religion",
        "sex",
    }
)

# Strict closed schema allowlists for every raw JSON object level.
ALLOWED_PERFORM_ENVELOPE_FIELDS: frozenset[str] = frozenset(
    {
        "alt_profile",
        "input",
        "invocation_id",
    }
)

ALLOWED_SOLVE_REQUEST_FIELDS: frozenset[str] = frozenset(
    {
        "allow_cross_branch_transfers",
        "authorized_transfer_employee_ids",
        "employees",
        "profile",
        "roster_id",
        "shifts",
        "unit_id",
    }
)

ALLOWED_EMPLOYEE_FIELDS: frozenset[str] = frozenset(
    {
        "approved_leave",
        "avoid_shift_ids",
        "cross_trainable_skills",
        "display_label",
        "eligible_unit_ids",
        "employee_id",
        "home_unit_id",
        "max_overtime_weekly_hours",
        "max_overtime_weekly_minutes",
        "max_regular_weekly_hours",
        "max_regular_weekly_minutes",
        "overtime_cost_minor_per_hour",
        "overtime_cost_per_hour",
        "preferred_shift_ids",
        "prior_weekend_shifts",
        "skills",
    }
)

ALLOWED_TIME_WINDOW_FIELDS: frozenset[str] = frozenset(
    {
        "ends_at",
        "starts_at",
    }
)

ALLOWED_SHIFT_FIELDS: frozenset[str] = frozenset(
    {
        "ends_at",
        "is_weekend",
        "required_count",
        "role_code",
        "shift_id",
        "starts_at",
        "unit_id",
    }
)

ALLOWED_PROFILE_FIELDS: frozenset[str] = frozenset(
    {
        "avoid_split_shifts",
        "currency",
        "label",
        "max_consecutive_days",
        "max_daily_hours",
        "max_daily_minutes",
        "max_weekly_hours",
        "max_weekly_minutes",
        "min_rest_hours",
        "min_rest_minutes",
        "profile_id",
        "staff_preference_priority",
        "weekend_fairness_priority",
        "weekly_overtime_budget",
        "weekly_overtime_budget_minor",
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


class UnknownFieldError(SolverContractError):
    """Raised when an unallowlisted field appears at any object level in a solver payload."""

    def __init__(self, field_path: str, allowed_fields: frozenset[str] | None = None) -> None:
        self.field_path = field_path
        self.allowed_fields = allowed_fields
        super().__init__(
            "NORN-SOLVER-UNKNOWN-FIELD",
            f"unknown field {field_path!r} is not allowed in the closed rostering solver schema",
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
        items = sorted(value.items(), key=lambda kv: str(kv[0]))
        for k, _v in items:
            if not isinstance(k, str):
                raise SolverContractError(
                    "NORN-SOLVER-SCHEMA",
                    f"{path} object keys must be strings, got {type(k).__name__}",
                )
            child_path = f"{path}.{k}"
            if normalize_field_key(k) in FORBIDDEN_PII_FIELDS:
                raise PiiViolationError(child_path)
        for k, v in items:
            reject_pii_and_floats(v, f"{path}.{k}")
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


def parse_str(raw: Any, path: str) -> str:
    """Parse a strict string (rejecting non-strings, floats, bools, and containers)."""
    if isinstance(raw, float):
        raise FloatRefusalError(path, raw)
    if not isinstance(raw, str):
        raise SolverContractError(
            "NORN-SOLVER-SCHEMA",
            f"{path} must be a string, got {type(raw).__name__}",
        )
    return raw


def parse_str_list(raw: Any, path: str) -> tuple[str, ...]:
    """Parse a strict JSON array of strings (rejecting bare strings, dicts, and non-string items)."""
    if not isinstance(raw, list):
        raise SolverContractError(
            "NORN-SOLVER-SCHEMA",
            f"{path} must be a list of strings, got {type(raw).__name__}",
        )
    return tuple(parse_str(item, f"{path}[{idx}]") for idx, item in enumerate(raw))


def _validate_object_keys(
    raw: Any,
    allowed_fields: frozenset[str],
    path: str,
) -> dict[str, Any]:
    """Validate that `raw` is a JSON object whose keys all belong to `allowed_fields`."""
    if not isinstance(raw, dict):
        raise SolverContractError(
            "NORN-SOLVER-SCHEMA",
            f"{path} must be an object, got {type(raw).__name__}",
        )
    for k in sorted(raw.keys(), key=str):
        if not isinstance(k, str):
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path} object keys must be strings, got {type(k).__name__}",
            )
        if k not in allowed_fields:
            child_path = f"{path}.{k}"
            if normalize_field_key(k) in FORBIDDEN_PII_FIELDS:
                raise PiiViolationError(child_path)
            raise UnknownFieldError(child_path, allowed_fields)
    return raw


def validate_time_window_payload(
    raw: Any,
    path: str,
    *,
    _skip_pii_scan: bool = False,
) -> None:
    """Validate a raw TimeWindow object against the strict closed schema allowlist."""
    if not _skip_pii_scan:
        reject_pii_and_floats(raw, path)
    obj = _validate_object_keys(raw, ALLOWED_TIME_WINDOW_FIELDS, path)
    for key in ("starts_at", "ends_at"):
        if key in obj and not isinstance(obj[key], (str, datetime)):
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path}.{key} must be ISO-8601 timestamp string or datetime, got {type(obj[key]).__name__}",
            )


def validate_preference_profile_payload(
    raw: Any,
    path: str = "$.profile",
    *,
    _skip_pii_scan: bool = False,
) -> None:
    """Validate a raw PreferenceProfileInput object against the strict closed schema allowlist."""
    if not _skip_pii_scan:
        reject_pii_and_floats(raw, path)
    obj = _validate_object_keys(raw, ALLOWED_PROFILE_FIELDS, path)
    for key in ("profile_id", "label", "currency", "weekend_fairness_priority", "staff_preference_priority"):
        if key in obj:
            parse_str(obj[key], f"{path}.{key}")
    if "avoid_split_shifts" in obj:
        parse_bool(obj["avoid_split_shifts"], f"{path}.avoid_split_shifts")
    for key in (
        "max_consecutive_days",
        "weekly_overtime_budget_minor",
        "min_rest_minutes",
        "max_daily_minutes",
        "max_weekly_minutes",
    ):
        if key in obj:
            min_val = 1 if key == "max_consecutive_days" else 0
            parse_int(obj[key], f"{path}.{key}", minimum=min_val)
    for key in ("weekly_overtime_budget", "min_rest_hours", "max_daily_hours", "max_weekly_hours"):
        if key in obj:
            parse_decimal(obj[key], f"{path}.{key}")


def validate_employee_payload(
    raw: Any,
    path: str,
    *,
    _skip_pii_scan: bool = False,
) -> None:
    """Validate a raw EmployeeInput object against the strict closed schema allowlist."""
    if not _skip_pii_scan:
        reject_pii_and_floats(raw, path)
    obj = _validate_object_keys(raw, ALLOWED_EMPLOYEE_FIELDS, path)
    for key in ("employee_id", "display_label", "home_unit_id"):
        if key in obj:
            parse_str(obj[key], f"{path}.{key}")
    for key in (
        "eligible_unit_ids",
        "skills",
        "cross_trainable_skills",
        "preferred_shift_ids",
        "avoid_shift_ids",
    ):
        if key in obj:
            parse_str_list(obj[key], f"{path}.{key}")
    for key in (
        "prior_weekend_shifts",
        "max_regular_weekly_minutes",
        "max_overtime_weekly_minutes",
        "overtime_cost_minor_per_hour",
    ):
        if key in obj:
            parse_int(obj[key], f"{path}.{key}", minimum=0)
    for key in ("max_regular_weekly_hours", "max_overtime_weekly_hours", "overtime_cost_per_hour"):
        if key in obj:
            parse_decimal(obj[key], f"{path}.{key}")
    if "approved_leave" in obj:
        raw_leave = obj["approved_leave"]
        if not isinstance(raw_leave, list):
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path}.approved_leave must be a list, got {type(raw_leave).__name__}",
            )
        for idx, win in enumerate(raw_leave):
            validate_time_window_payload(win, f"{path}.approved_leave[{idx}]", _skip_pii_scan=True)


def validate_shift_payload(
    raw: Any,
    path: str,
    *,
    _skip_pii_scan: bool = False,
) -> None:
    """Validate a raw ShiftRequirementInput object against the strict closed schema allowlist."""
    if not _skip_pii_scan:
        reject_pii_and_floats(raw, path)
    obj = _validate_object_keys(raw, ALLOWED_SHIFT_FIELDS, path)
    for key in ("shift_id", "unit_id", "role_code"):
        if key in obj:
            parse_str(obj[key], f"{path}.{key}")
    for key in ("starts_at", "ends_at"):
        if key in obj and not isinstance(obj[key], (str, datetime)):
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path}.{key} must be ISO-8601 timestamp string or datetime, got {type(obj[key]).__name__}",
            )
    if "required_count" in obj:
        parse_int(obj["required_count"], f"{path}.required_count", minimum=1)
    if "is_weekend" in obj:
        parse_bool(obj["is_weekend"], f"{path}.is_weekend")


def validate_solve_request_payload(
    raw: Any,
    path: str = "$",
    *,
    _skip_pii_scan: bool = False,
) -> None:
    """Validate a raw SolveRequest payload against the strict closed schema allowlist."""
    if not _skip_pii_scan:
        reject_pii_and_floats(raw, path)
    obj = _validate_object_keys(raw, ALLOWED_SOLVE_REQUEST_FIELDS, path)
    for key in ("roster_id", "unit_id"):
        if key in obj:
            parse_str(obj[key], f"{path}.{key}")
    if "allow_cross_branch_transfers" in obj:
        parse_bool(obj["allow_cross_branch_transfers"], f"{path}.allow_cross_branch_transfers")
    if "authorized_transfer_employee_ids" in obj:
        parse_str_list(obj["authorized_transfer_employee_ids"], f"{path}.authorized_transfer_employee_ids")
    if "profile" in obj:
        validate_preference_profile_payload(obj["profile"], f"{path}.profile", _skip_pii_scan=True)
    if "employees" in obj:
        raw_employees = obj["employees"]
        if not isinstance(raw_employees, list):
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path}.employees must be a list, got {type(raw_employees).__name__}",
            )
        for idx, emp in enumerate(raw_employees):
            validate_employee_payload(emp, f"{path}.employees[{idx}]", _skip_pii_scan=True)
    if "shifts" in obj:
        raw_shifts = obj["shifts"]
        if not isinstance(raw_shifts, list):
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path}.shifts must be a list, got {type(raw_shifts).__name__}",
            )
        for idx, sh in enumerate(raw_shifts):
            validate_shift_payload(sh, f"{path}.shifts[{idx}]", _skip_pii_scan=True)


def validate_invoke_payload(raw: Any) -> tuple[dict[str, Any], str, str | None, dict[str, Any] | None]:
    """Validate a worker `/invoke` payload (perform envelope or direct request + optional `alt_profile`).

    Returns `(raw_request, request_path, invocation_id, raw_alt_profile)`.
    """
    reject_pii_and_floats(raw, "$")
    if not isinstance(raw, dict):
        raise SolverContractError(
            "NORN-SOLVER-SCHEMA",
            f"request payload must be a JSON object, got {type(raw).__name__}",
        )

    if "input" in raw:
        obj = _validate_object_keys(raw, ALLOWED_PERFORM_ENVELOPE_FIELDS, "$")
        raw_request = obj["input"]
        if not isinstance(raw_request, dict):
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"$.input must be an object, got {type(raw_request).__name__}",
            )
        invocation_id: str | None = None
        if "invocation_id" in obj and obj["invocation_id"] is not None:
            invocation_id = parse_str(obj["invocation_id"], "$.invocation_id")
        raw_alt_profile: dict[str, Any] | None = None
        if "alt_profile" in obj and obj["alt_profile"] is not None:
            validate_preference_profile_payload(obj["alt_profile"], "$.alt_profile", _skip_pii_scan=True)
            raw_alt_profile = obj["alt_profile"]
        validate_solve_request_payload(raw_request, "$.input", _skip_pii_scan=True)
        return raw_request, "$.input", invocation_id, raw_alt_profile

    allowed_direct = ALLOWED_SOLVE_REQUEST_FIELDS | frozenset({"alt_profile"})
    obj = _validate_object_keys(raw, allowed_direct, "$")
    raw_alt_profile = None
    if "alt_profile" in obj and obj["alt_profile"] is not None:
        validate_preference_profile_payload(obj["alt_profile"], "$.alt_profile", _skip_pii_scan=True)
        raw_alt_profile = obj["alt_profile"]
    direct_request = {k: v for k, v in obj.items() if k != "alt_profile"}
    validate_solve_request_payload(direct_request, "$", _skip_pii_scan=True)
    return direct_request, "$", None, raw_alt_profile


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
