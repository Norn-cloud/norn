"""Comprehensive test suite for the OR-Tools CP-SAT rostering solver (`norn.solver.rostering@0.1.0`)."""

from __future__ import annotations

import copy
import json
import re
import socket
import threading
import time
import unicodedata
import urllib.error
import urllib.request
from datetime import UTC, datetime, timedelta
from decimal import Decimal
from pathlib import Path
from typing import Any
from zoneinfo import ZoneInfo

import pytest
from ortools.sat.python import cp_model

from norn_rostering_solver import (
    ALLOWED_EMPLOYEE_FIELDS,
    ALLOWED_PERFORM_ENVELOPE_FIELDS,
    ALLOWED_PROFILE_FIELDS,
    ALLOWED_SHIFT_FIELDS,
    ALLOWED_SOLVE_REQUEST_FIELDS,
    ALLOWED_TIME_WINDOW_FIELDS,
    CONTRACT_ID,
    FORBIDDEN_PII_FIELDS,
    FUNCTION_ID,
    FUNCTION_VERSION,
    MAX_ASSIGNMENT_VARS,
    MAX_CONFLICT_CONSTRAINTS,
    MAX_EMPLOYEES,
    MAX_HORIZON_DAYS,
    MAX_LEAVE_BLOCKS,
    MAX_SHIFTS,
    Assignment,
    DuplicateKeyError,
    EmployeeInput,
    FloatRefusalError,
    LimitExceededError,
    ModelInvalidError,
    PiiViolationError,
    PostVerificationError,
    PreferenceProfileInput,
    Priority,
    ShiftRequirementInput,
    SolverContractError,
    SolveRequest,
    TimeWindow,
    UnknownFieldError,
    build_function_manifest,
    canonical_json,
    compare_what_if,
    compute_comparison_digest,
    compute_manifest_digest,
    create_worker_server,
    handle_invoke_payload,
    normalize_field_key,
    package_source_digest,
    parse_strict_json,
    solve_roster,
    split_interval_by_iso_week,
    split_interval_by_local_day,
    verify_comparison_digest,
    verify_or_raise,
    verify_receipt_digest,
    verify_roster,
    verify_statutory_laws,
)
from norn_rostering_solver.boundary import domain_sha256_digest


def _utc(year: int, month: int, day: int, hour: int, minute: int = 0) -> datetime:
    return datetime(year, month, day, hour, minute, tzinfo=UTC)


def _cairo_profile(
    *,
    profile_id: str = "prof_cairo_balanced",
    budget: str = "1500.00",
    weekend_fairness: Priority = "high",
    staff_pref: Priority = "medium",
    min_rest: str = "11.00",
    max_daily: str = "10.00",
    max_weekly: str = "48.00",
    max_consec: int = 6,
    avoid_split: bool = True,
) -> PreferenceProfileInput:
    return PreferenceProfileInput(
        profile_id=profile_id,
        currency="EGP",
        weekly_overtime_budget=Decimal(budget),
        min_rest_hours=Decimal(min_rest),
        max_daily_hours=Decimal(max_daily),
        max_weekly_hours=Decimal(max_weekly),
        max_consecutive_days=max_consec,
        weekend_fairness_priority=weekend_fairness,
        staff_preference_priority=staff_pref,
        avoid_split_shifts=avoid_split,
    )


def _valid_raw_payload() -> dict[str, Any]:
    return {
        "roster_id": "ros_2026_w48",
        "unit_id": "branch_maadi",
        "iana_timezone": "Africa/Cairo",
        "profile": {
            "profile_id": "prof_1",
            "currency": "EGP",
            "weekly_overtime_budget": "500.00",
            "min_rest_hours": "11.00",
            "max_daily_hours": "10.00",
            "max_weekly_hours": "48.00",
            "max_consecutive_days": 6,
            "weekend_fairness_priority": "high",
            "staff_preference_priority": "medium",
            "avoid_split_shifts": True,
        },
        "employees": [
            {
                "employee_id": "emp_01",
                "home_unit_id": "branch_maadi",
                "skills": ["cashier"],
                "max_regular_weekly_hours": "40.00",
                "max_overtime_weekly_hours": "8.00",
                "overtime_cost_per_hour": "120.00",
            }
        ],
        "shifts": [
            {
                "shift_id": "sh_sat_am",
                "unit_id": "branch_maadi",
                "role_code": "cashier",
                "starts_at": "2026-11-28T06:00:00Z",
                "ends_at": "2026-11-28T14:00:00Z",
                "required_count": 1,
                "is_weekend": True,
            }
        ],
    }


# ---------------------------------------------------------------------------
# 1. Fail-Closed PII, Duplicate-Key, Closed-Schema & IEEE Float Boundary Tests
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("pii_field", "val"),
    [
        ("national_id", "29901010100001"),
        ("phone", "+201000000000"),
        ("email", "staff@example.eg"),
        ("base_salary", "9500.00"),
        ("hourly_overtime_rate", "150.00"),
        ("medical_note", "sprained ankle"),
        ("gender", "female"),
        ("dob", "1995-04-12"),
        ("bank_account", "EG380019000500000000263180002"),
        ("NATIONAL_ID", "29901010100001"),
    ],
)
def test_pii_fields_fail_closed_recursively(pii_field: str, val: str) -> None:
    payload = _valid_raw_payload()
    payload["employees"][0]["nested_meta"] = [{"inner": {pii_field: val}}]
    with pytest.raises(PiiViolationError) as exc_info:
        SolveRequest.from_dict(payload)
    assert exc_info.value.code == "NORN-SOLVER-PII-FORBIDDEN"


def test_duplicate_json_keys_and_shadowed_pii_fail_closed() -> None:
    # 1. Plain duplicate non-PII key fails closed with NORN-SOLVER-DUPLICATE-KEY
    dup_json = '{"roster_id":"r1","roster_id":"r2"}'
    with pytest.raises(DuplicateKeyError) as exc_dup:
        parse_strict_json(dup_json)
    assert exc_dup.value.code == "NORN-SOLVER-DUPLICATE-KEY"

    # 2. Duplicate key alongside PII fails closed with NORN-SOLVER-PII-FORBIDDEN
    dup_with_pii = '{"employee_ref":"e1","name":"Alice","employee_ref":"e2"}'
    with pytest.raises(PiiViolationError) as exc_pii_sib:
        parse_strict_json(dup_with_pii)
    assert exc_pii_sib.value.code == "NORN-SOLVER-PII-FORBIDDEN"

    # 3. Overwritten subtree hiding PII fails closed with NORN-SOLVER-PII-FORBIDDEN
    shadowed_pii = '{"meta":{"national_id":"29901010100001"},"meta":{}}'
    with pytest.raises(PiiViolationError) as exc_shadow:
        parse_strict_json(shadowed_pii)
    assert exc_shadow.value.code == "NORN-SOLVER-PII-FORBIDDEN"

    # 4. Duplicate PII key fails closed with NORN-SOLVER-PII-FORBIDDEN
    dup_pii = '{"name":"Alice","name":"Bob"}'
    with pytest.raises(PiiViolationError) as exc_dup_pii:
        parse_strict_json(dup_pii)
    assert exc_dup_pii.value.code == "NORN-SOLVER-PII-FORBIDDEN"

    # 5. Over HTTP invoke: duplicate key -> 400 NORN-SOLVER-DUPLICATE-KEY
    status, body = handle_invoke_payload(dup_json.encode("utf-8"))
    assert status == 400
    assert body["code"] == "NORN-SOLVER-DUPLICATE-KEY"
    assert body["status"] == "refused"


def test_free_text_labels_rejected_by_closed_schema() -> None:
    assert "display_label" not in ALLOWED_EMPLOYEE_FIELDS
    assert "display_label" not in ALLOWED_SHIFT_FIELDS
    assert "label" not in ALLOWED_PROFILE_FIELDS

    payload_emp = _valid_raw_payload()
    payload_emp["employees"][0]["display_label"] = "Alice (Cashier)"
    with pytest.raises(UnknownFieldError) as exc_emp:
        SolveRequest.from_dict(payload_emp)
    assert exc_emp.value.code == "NORN-SOLVER-UNKNOWN-FIELD"
    assert exc_emp.value.field_path == "$.employees[0].display_label"

    payload_prof = _valid_raw_payload()
    payload_prof["profile"]["label"] = "Cairo Retail"
    with pytest.raises(UnknownFieldError) as exc_prof:
        SolveRequest.from_dict(payload_prof)
    assert exc_prof.value.code == "NORN-SOLVER-UNKNOWN-FIELD"
    assert exc_prof.value.field_path == "$.profile.label"


def test_float_inputs_refused_in_dict_and_strict_json() -> None:
    payload = _valid_raw_payload()
    payload["profile"]["min_rest_hours"] = 11.5
    with pytest.raises(FloatRefusalError) as exc_info:
        SolveRequest.from_dict(payload)
    assert exc_info.value.code == "NORN-SOLVER-FLOAT-FORBIDDEN"

    raw_json_with_float = '{"roster_id":"r1","value":10.5}'
    with pytest.raises(FloatRefusalError) as exc_json:
        parse_strict_json(raw_json_with_float)
    assert exc_json.value.code == "NORN-SOLVER-FLOAT-FORBIDDEN"

    raw_json_with_nan = '{"roster_id":"r1","value":NaN}'
    with pytest.raises(FloatRefusalError) as exc_nan:
        parse_strict_json(raw_json_with_nan)
    assert exc_nan.value.code == "NORN-SOLVER-FLOAT-FORBIDDEN"


def test_pii_denylist_disjoint_from_all_closed_schema_allowlists() -> None:
    all_allowed = (
        ALLOWED_PERFORM_ENVELOPE_FIELDS
        | ALLOWED_SOLVE_REQUEST_FIELDS
        | ALLOWED_EMPLOYEE_FIELDS
        | ALLOWED_TIME_WINDOW_FIELDS
        | ALLOWED_SHIFT_FIELDS
        | ALLOWED_PROFILE_FIELDS
    )
    normalized_allowed = {normalize_field_key(k) for k in all_allowed}
    assert normalized_allowed.isdisjoint(FORBIDDEN_PII_FIELDS)


@pytest.mark.parametrize(
    ("pii_field", "val"),
    [
        ("name", "SYNTH_TOKEN_NAME"),
        ("national_id", "SYNTH_NID_0001"),
        ("phone", "SYNTH_PHONE_0001"),
        ("salary", "10000.00"),
        ("medical_note", "SYNTH_NOTE_0001"),
        ("mobile_no", "SYNTH_MOBILE_0001"),
        ("mobileNo", "SYNTH_MOBILE_0002"),
        ("full_name_ar", "SYNTH_NAME_AR_0001"),
        ("fullNameAr", "SYNTH_NAME_AR_0002"),
        ("iqama", "SYNTH_IQAMA_0001"),
        ("IQAMA_NO", "SYNTH_IQAMA_0002"),
        ("bank_iban", "SYNTH_IBAN_0001"),
        ("bankIBAN", "SYNTH_IBAN_0002"),
        ("passport_number", "SYNTH_PASS_0001"),
        ("passport-number", "SYNTH_PASS_0002"),
        ("emergency_contact", "SYNTH_EMERG_0001"),
        ("home_address", "SYNTH_ADDR_0001"),
        ("homeAddress", "SYNTH_ADDR_0002"),
    ],
)
def test_adversarial_pii_keys_refused_with_exact_path(pii_field: str, val: str) -> None:
    payload = _valid_raw_payload()
    payload["employees"][0][pii_field] = val
    with pytest.raises(PiiViolationError) as exc_info:
        SolveRequest.from_dict(payload)
    assert exc_info.value.code == "NORN-SOLVER-PII-FORBIDDEN"
    assert exc_info.value.field_path == f"$.employees[0].{pii_field}"
    assert f"$.employees[0].{pii_field}" in exc_info.value.message


@pytest.mark.parametrize(
    ("mutator_kind", "expected_path"),
    [
        ("top_level", "$.unexpected_top_key"),
        ("employee", "$.employees[0].unlisted_employee_attr"),
        ("shift", "$.shifts[0].unlisted_shift_attr"),
        ("leave_block", "$.employees[0].approved_leave[0].unlisted_leave_attr"),
        ("profile", "$.profile.unlisted_profile_weight"),
    ],
)
def test_closed_schema_rejects_unknown_keys_at_every_object_level(
    mutator_kind: str,
    expected_path: str,
) -> None:
    payload = _valid_raw_payload()
    payload["employees"][0]["approved_leave"] = [
        {"starts_at": "2026-11-29T00:00:00Z", "ends_at": "2026-11-29T08:00:00Z"}
    ]
    if mutator_kind == "top_level":
        payload["unexpected_top_key"] = "opaque_val"
    elif mutator_kind == "employee":
        payload["employees"][0]["unlisted_employee_attr"] = "opaque_val"
    elif mutator_kind == "shift":
        payload["shifts"][0]["unlisted_shift_attr"] = "opaque_val"
    elif mutator_kind == "leave_block":
        payload["employees"][0]["approved_leave"][0]["unlisted_leave_attr"] = "opaque_val"
    elif mutator_kind == "profile":
        payload["profile"]["unlisted_profile_weight"] = 42

    with pytest.raises(UnknownFieldError) as exc_info:
        SolveRequest.from_dict(payload)
    assert exc_info.value.code == "NORN-SOLVER-UNKNOWN-FIELD"
    assert exc_info.value.field_path == expected_path
    assert expected_path in exc_info.value.message


def test_closed_schema_rejects_unknown_keys_in_worker_envelopes_and_alt_profile() -> None:
    env_extra: dict[str, Any] = {
        "invocation_id": "inv_adv_1",
        "input": _valid_raw_payload(),
        "shadow_header": "opaque",
    }
    status, body = handle_invoke_payload(canonical_json(env_extra).encode("utf-8"))
    assert status == 422
    assert body["code"] == "NORN-SOLVER-UNKNOWN-FIELD"
    assert "$.shadow_header" in str(body["error"])

    env_nested: dict[str, Any] = {
        "invocation_id": "inv_adv_2",
        "input": _valid_raw_payload(),
    }
    env_nested["input"]["employees"][0]["custom_badge_code"] = "B-01"
    status, body = handle_invoke_payload(canonical_json(env_nested).encode("utf-8"))
    assert status == 422
    assert body["code"] == "NORN-SOLVER-UNKNOWN-FIELD"
    assert "$.input.employees[0].custom_badge_code" in str(body["error"])

    direct_alt = _valid_raw_payload()
    direct_alt["alt_profile"] = {
        **direct_alt["profile"],
        "profile_id": "prof_alt",
        "unlisted_override": True,
    }
    status, body = handle_invoke_payload(canonical_json(direct_alt).encode("utf-8"))
    assert status == 422
    assert body["code"] == "NORN-SOLVER-UNKNOWN-FIELD"
    assert "$.alt_profile.unlisted_override" in str(body["error"])

    pii_req = _valid_raw_payload()
    pii_req["employees"][0]["iqama"] = "SYNTH_IQAMA_999"
    status, body = handle_invoke_payload(canonical_json(pii_req).encode("utf-8"))
    assert status == 422
    assert body["code"] == "NORN-SOLVER-PII-FORBIDDEN"
    assert "$.employees[0].iqama" in str(body["error"])


@pytest.mark.parametrize(
    "bad_payload_factory",
    [
        lambda: ["not", "an", "object"],
        lambda: "not-an-object",
        lambda: 42,
        lambda: {**_valid_raw_payload(), "profile": ["not", "a", "dict"]},
        lambda: {**_valid_raw_payload(), "employees": {"emp_01": {}}},
        lambda: {**_valid_raw_payload(), "employees": ["not-a-dict"]},
        lambda: {**_valid_raw_payload(), "shifts": {"sh_1": {}}},
        lambda: {**_valid_raw_payload(), "shifts": [123]},
        lambda: {**_valid_raw_payload(), "authorized_transfer_employee_ids": "emp_01"},
        lambda: {**_valid_raw_payload(), "authorized_transfer_employee_ids": [123]},
        lambda: {
            **_valid_raw_payload(),
            "employees": [{**_valid_raw_payload()["employees"][0], "skills": "cashier"}],
        },
        lambda: {
            **_valid_raw_payload(),
            "employees": [{**_valid_raw_payload()["employees"][0], "skills": [123]}],
        },
        lambda: {
            **_valid_raw_payload(),
            "employees": [{**_valid_raw_payload()["employees"][0], "approved_leave": {"starts_at": "2026-11-29T00:00:00Z"}}],
        },
        lambda: {
            **_valid_raw_payload(),
            "employees": [{**_valid_raw_payload()["employees"][0], "approved_leave": [["2026-11-29T00:00:00Z"]]}],
        },
        lambda: {
            **_valid_raw_payload(),
            "employees": [{**_valid_raw_payload()["employees"][0], "approved_leave": [{"starts_at": "2026-11-29T00:00:00Z"}]}],
        },
        lambda: {**_valid_raw_payload(), "roster_id": ["not-a-string"]},
        lambda: {**_valid_raw_payload(), "iana_timezone": "Mars/Olympus_Mons"},
    ],
)
def test_unexpected_container_shapes_fail_closed_cleanly(bad_payload_factory: Any) -> None:
    bad_payload = bad_payload_factory()
    with pytest.raises(SolverContractError) as exc_info:
        SolveRequest.from_dict(bad_payload)
    assert exc_info.value.code == "NORN-SOLVER-SCHEMA"


def test_worker_rejects_malformed_envelope_containers_cleanly() -> None:
    for bad_envelope in (
        ["list-root"],
        {"input": ["not-a-dict"]},
        {"input": _valid_raw_payload(), "invocation_id": {"nested": "bad"}},
        {"input": _valid_raw_payload(), "alt_profile": ["not-a-dict"]},
    ):
        status, body = handle_invoke_payload(canonical_json(bad_envelope).encode("utf-8"))
        assert status == 422
        assert body["code"] == "NORN-SOLVER-SCHEMA"
        assert body["status"] == "refused"


def test_valid_full_allowlist_payload_solves_deterministically_and_passes_verification() -> None:
    payload: dict[str, Any] = {
        "roster_id": "ros_allowlist_full",
        "unit_id": "branch_maadi",
        "iana_timezone": "Africa/Cairo",
        "allow_cross_branch_transfers": False,
        "authorized_transfer_employee_ids": [],
        "profile": {
            "profile_id": "prof_allowlist",
            "currency": "EGP",
            "weekly_overtime_budget": "500.00",
            "min_rest_hours": "11.00",
            "max_daily_hours": "10.00",
            "max_weekly_hours": "48.00",
            "max_consecutive_days": 6,
            "weekend_fairness_priority": "high",
            "staff_preference_priority": "medium",
            "avoid_split_shifts": True,
        },
        "employees": [
            {
                "employee_id": "emp_token_01",
                "home_unit_id": "branch_maadi",
                "eligible_unit_ids": ["branch_maadi"],
                "skills": ["cashier"],
                "cross_trainable_skills": ["barista"],
                "max_regular_weekly_hours": "40.00",
                "max_overtime_weekly_hours": "8.00",
                "overtime_cost_per_hour": "120.00",
                "approved_leave": [
                    {
                        "starts_at": "2026-11-29T00:00:00Z",
                        "ends_at": "2026-11-29T12:00:00Z",
                    }
                ],
                "preferred_shift_ids": ["sh_sat_am"],
                "avoid_shift_ids": [],
                "prior_weekend_shifts": 1,
            }
        ],
        "shifts": [
            {
                "shift_id": "sh_sat_am",
                "unit_id": "branch_maadi",
                "role_code": "cashier",
                "starts_at": "2026-11-28T06:00:00Z",
                "ends_at": "2026-11-28T14:00:00Z",
                "required_count": 1,
                "is_weekend": True,
            }
        ],
    }

    req = SolveRequest.from_dict(payload)
    res1 = solve_roster(req)
    res2 = solve_roster(SolveRequest.from_dict(parse_strict_json(canonical_json(payload))))

    assert res1.status == "optimal"
    assert res1.evidence_label == "exhaustive-finite"
    assert res1.verification.checked and res1.verification.passed and res1.verification.violation_count == 0
    assert len(res1.assignments) == 1
    assert res1.assignments[0].employee_id == "emp_token_01"
    assert res1.solver_receipt_digest == res2.solver_receipt_digest
    assert canonical_json(res1.to_dict()) == canonical_json(res2.to_dict())
    assert verify_receipt_digest(res1.to_dict()) is True


# ---------------------------------------------------------------------------
# 2. Exact Minutes / Money Parsing & Full-Payload Receipt Integrity
# ---------------------------------------------------------------------------


def test_exact_integer_minutes_preserved_without_lossy_hour_rounding() -> None:
    # 470 min (7h50m), 50 min rest, 550 min daily, 1000 min weekly — none divisible by 3!
    payload: dict[str, Any] = {
        "roster_id": "ros_exact_min",
        "unit_id": "branch_maadi",
        "profile": {
            "profile_id": "prof_exact_min",
            "currency": "EGP",
            "weekly_overtime_budget_minor": 50_000,
            "min_rest_minutes": 50,
            "max_daily_minutes": 550,
            "max_weekly_minutes": 1_000,
            "max_consecutive_days": 6,
        },
        "employees": [
            {
                "employee_id": "emp_exact",
                "home_unit_id": "branch_maadi",
                "skills": ["cashier"],
                "max_regular_weekly_minutes": 470,
                "max_overtime_weekly_minutes": 50,
                "overtime_cost_minor_per_hour": 12_000,
            }
        ],
        "shifts": [
            {
                "shift_id": "sh_490m",
                "unit_id": "branch_maadi",
                "role_code": "cashier",
                "starts_at": "2026-11-28T06:00:00Z",
                "ends_at": "2026-11-28T14:10:00Z",  # 490 minutes = 470 regular + 20 overtime!
                "required_count": 1,
            }
        ],
    }

    req = SolveRequest.from_dict(payload)
    assert req.employees[0].max_regular_weekly_minutes == 470
    assert req.employees[0].max_overtime_weekly_minutes == 50
    assert req.profile.min_rest_minutes == 50
    assert req.profile.max_daily_minutes == 550

    res = solve_roster(req)
    assert res.status == "optimal"
    assert res.metrics is not None
    assert res.metrics.total_regular_minutes == 470
    assert res.metrics.total_overtime_minutes == 20
    # 20 min OT at 120.00 EGP/h = exactly 40.00 EGP (4000 minor units)
    assert res.metrics.total_overtime_cost_minor == 4_000
    assert res.metrics.total_overtime_cost == Decimal("40.00")
    assert verify_receipt_digest(res.to_dict()) is True

    # Round-tripping serialized employee/profile dicts preserves exact minutes losslessly.
    rt_dict: dict[str, Any] = {
        "roster_id": req.roster_id,
        "unit_id": req.unit_id,
        "iana_timezone": req.iana_timezone,
        "profile": req.profile.to_dict(),
        "employees": [e.to_dict() for e in req.employees],
        "shifts": [{k: v for k, v in s.to_dict().items() if k != "duration_minutes"} for s in req.shifts],
    }
    req_rt = SolveRequest.from_dict(rt_dict)
    assert req_rt.employees[0].max_regular_weekly_minutes == 470
    assert req_rt.profile.min_rest_minutes == 50


@pytest.mark.parametrize(
    ("field_path", "bad_val"),
    [
        (("profile", "weekly_overtime_budget"), "1.00000000000000000000000000001"),  # exceeds 28-digit Decimal prec
        (("profile", "weekly_overtime_budget"), "0.001"),  # over-scale money (> 2 decimals)
        (("profile", "weekly_overtime_budget"), "1e-30"),  # tiny over-scale exponent
        (("profile", "weekly_overtime_budget"), "NaN"),  # non-finite string
        (("profile", "weekly_overtime_budget"), "Infinity"),  # non-finite string
        (("profile", "weekly_overtime_budget"), "-Infinity"),  # non-finite string
        (("profile", "weekly_overtime_budget"), "500.125"),  # over-scale money (> 2 decimals)
        (("employees", 0, "overtime_cost_per_hour"), "120.999"),  # over-scale money
        (("profile", "weekly_overtime_budget"), "not-a-number"),  # invalid decimal
        (("profile", "weekly_overtime_budget"), "9" * 40),  # huge decimal
        (("profile", "weekly_overtime_budget"), "1e999999999"),  # huge exponent
        (("profile", "min_rest_hours"), "1.125"),  # 67.5 minutes (non-whole minute)
        (("profile", "min_rest_hours"), "1.00000000000000000000000000001"),  # non-whole minute past prec 28
    ],
)
def test_bad_or_overscale_decimal_strings_fail_closed_with_400(
    field_path: tuple[str | int, ...],
    bad_val: str,
) -> None:
    payload = _valid_raw_payload()
    cur: Any = payload
    for seg in field_path[:-1]:
        cur = cur[seg]
    cur[field_path[-1]] = bad_val

    status, body = handle_invoke_payload(canonical_json(payload).encode("utf-8"))
    assert status == 400
    assert body["code"] == "NORN-SOLVER-DECIMAL"
    assert body["status"] == "refused"


def test_full_payload_receipt_tamper_detection() -> None:
    # 1. Feasible/optimal receipt tamper tests (metrics, assignments, status, verification, perform_summary)
    req = SolveRequest.from_dict(_valid_raw_payload())
    res = solve_roster(req)
    valid_doc = res.to_dict()
    assert verify_receipt_digest(valid_doc) is True

    tampered_metrics = copy.deepcopy(valid_doc)
    assert isinstance(tampered_metrics["metrics"], dict)
    tampered_metrics["metrics"]["total_overtime_cost"] = "9999.00"
    assert verify_receipt_digest(tampered_metrics) is False

    tampered_score = copy.deepcopy(valid_doc)
    assert isinstance(tampered_score["metrics"], dict)
    tampered_score["metrics"]["objective_score"] = 123_456
    assert verify_receipt_digest(tampered_score) is False

    tampered_assignments = copy.deepcopy(valid_doc)
    assert isinstance(tampered_assignments["assignments"], list)
    tampered_assignments["assignments"][0]["employee_id"] = "emp_tampered"
    assert verify_receipt_digest(tampered_assignments) is False

    tampered_status = copy.deepcopy(valid_doc)
    tampered_status["status"] = "feasible"
    assert verify_receipt_digest(tampered_status) is False

    tampered_verification = copy.deepcopy(valid_doc)
    assert isinstance(tampered_verification["verification"], dict)
    tampered_verification["verification"]["passed"] = False
    assert verify_receipt_digest(tampered_verification) is False

    tampered_summary = copy.deepcopy(valid_doc)
    assert isinstance(tampered_summary["perform_summary"], dict)
    tampered_summary["perform_summary"]["assignmentCount"] = 99
    assert verify_receipt_digest(tampered_summary) is False

    # 2. Infeasible receipt tamper test (`unsat_summary` and `repair_optimality`)
    unsat_raw = _valid_raw_payload()
    unsat_raw["shifts"][0]["required_count"] = 2
    unsat_res = solve_roster(SolveRequest.from_dict(unsat_raw))
    unsat_doc = unsat_res.to_dict()
    assert verify_receipt_digest(unsat_doc) is True

    tampered_unsat = copy.deepcopy(unsat_doc)
    tampered_unsat["unsat_summary"] = "tampered summary text"
    assert verify_receipt_digest(tampered_unsat) is False

    tampered_opt = copy.deepcopy(unsat_doc)
    tampered_opt["repair_optimality"] = "feasible_slack"
    assert verify_receipt_digest(tampered_opt) is False


# ---------------------------------------------------------------------------
# 3. Exact Overnight Shift & ISO-Week Boundary Accounting Tests
# ---------------------------------------------------------------------------


def test_overnight_shift_counts_both_calendar_days_and_splits_daily_hours() -> None:
    tz = ZoneInfo("UTC")
    overnight = ShiftRequirementInput(
        "sh_sat_night",
        "branch_maadi",
        "cashier",
        _utc(2026, 11, 28, 22),  # Sat 22:00 UTC
        _utc(2026, 11, 29, 6),   # Sun 06:00 UTC
        1,
    )
    assert overnight.daily_minutes(tz) == {
        datetime(2026, 11, 28, tzinfo=UTC).date(): 120,
        datetime(2026, 11, 29, tzinfo=UTC).date(): 360,
    }

    solo = (
        EmployeeInput(
            employee_id="emp_solo",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("60.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )

    # Mon..Fri day shifts (5 days: Nov 23..27) + Sat 22:00 -> Sun 06:00 spans Nov 28 AND Nov 29 => 7 consecutive days!
    base = datetime(2026, 11, 23, 8, 0, tzinfo=UTC)
    shifts_5_plus_overnight = tuple(
        ShiftRequirementInput(
            f"sh_d{d}",
            "branch_maadi",
            "cashier",
            base + timedelta(days=d),
            base + timedelta(days=d, hours=6),
            1,
        )
        for d in range(5)
    ) + (overnight,)

    req_streak = SolveRequest(
        "ros_overnight_streak",
        "branch_maadi",
        solo,
        shifts_5_plus_overnight,
        _cairo_profile(max_weekly="60.00", max_daily="10.00", max_consec=6),
        iana_timezone="UTC",
    )
    res_streak = solve_roster(req_streak)
    assert res_streak.status == "infeasible"

    # Independent verifier also catches the 7-day streak caused by the overnight shift!
    forced_assignments = tuple(
        Assignment(
            sh.shift_id,
            "emp_solo",
            sh.unit_id,
            sh.role_code,
            sh.starts_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
            sh.ends_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
            sh.duration_minutes,
            sh.duration_hours,
        )
        for sh in shifts_5_plus_overnight
    )
    ver = verify_roster(req_streak, forced_assignments)
    assert not ver.passed
    assert "HR-SH-STAT:max_consecutive_days_exceeded" in {v.code for v in ver.violations}

    # Daily cap split across midnight: Sat 20:00->Sun 04:00 (4h on Sun) + Sun 16:00->Sun 23:00 (7h on Sun) = 11h on Sun > 10h!
    sh_overnight_4h_sun = ShiftRequirementInput("sh_ov", "branch_maadi", "cashier", _utc(2026, 11, 28, 20), _utc(2026, 11, 29, 4), 1)
    sh_sun_evening_7h = ShiftRequirementInput("sh_sun_ev", "branch_maadi", "cashier", _utc(2026, 11, 29, 16), _utc(2026, 11, 29, 23), 1)
    req_daily_split = SolveRequest(
        "ros_overnight_daily",
        "branch_maadi",
        solo,
        (sh_overnight_4h_sun, sh_sun_evening_7h),
        _cairo_profile(min_rest="11.00", max_daily="10.00", max_weekly="48.00", max_consec=6),
        iana_timezone="UTC",
    )
    assert solve_roster(req_daily_split).status == "infeasible"


def test_sunday_to_monday_iso_week_boundary_split_utc_and_cairo() -> None:
    # Sun Nov 29 22:00Z -> Mon Nov 30 06:00Z:
    # - In UTC: 120 min in 2026-W48 (Sun) + 360 min in 2026-W49 (Mon)
    # - In Africa/Cairo (UTC+2): Mon Nov 30 00:00+02:00 -> Mon Nov 30 08:00+02:00 = ALL 480 min in 2026-W49!
    sun_mon_shift = ShiftRequirementInput(
        "sh_sun_mon",
        "branch_maadi",
        "cashier",
        _utc(2026, 11, 29, 22),
        _utc(2026, 11, 30, 6),
        1,
    )
    assert split_interval_by_iso_week(sun_mon_shift.starts_at, sun_mon_shift.ends_at, ZoneInfo("UTC")) == {
        (2026, 48): 120,
        (2026, 49): 360,
    }
    assert split_interval_by_iso_week(sun_mon_shift.starts_at, sun_mon_shift.ends_at, ZoneInfo("Africa/Cairo")) == {
        (2026, 49): 480,
    }

    # Shift starting Sun Nov 29 21:00Z (= 23:00 Cairo) -> Mon Nov 30 05:00Z (= 07:00 Cairo):
    # - In Africa/Cairo: 60 min in W48 + 420 min in W49!
    cairo_boundary = ShiftRequirementInput(
        "sh_cairo_wk_cross",
        "branch_maadi",
        "cashier",
        _utc(2026, 11, 29, 21),
        _utc(2026, 11, 30, 5),
        1,
    )
    assert split_interval_by_iso_week(cairo_boundary.starts_at, cairo_boundary.ends_at, ZoneInfo("Africa/Cairo")) == {
        (2026, 48): 60,
        (2026, 49): 420,
    }

    # Build 46h of work Mon..Fri in W48 (5 × 9h = 45h + 1h? Even cleaner: 4 × 10h + 1 × 6h = 46h in W48, 5 days Mon..Fri).
    # Then add Sun 22:00Z -> Mon 06:00Z (2h in W48, 6h in W49 under UTC):
    # Total in W48 = 46h + 2h = 48h <= 48h cap -> OPTIMAL!
    # Consecutive days = Mon..Fri (5 days), Sat OFF, Sun..Mon (2 days) -> max streak 5 <= 6!
    w48_shifts_46h = (
        ShiftRequirementInput("sh_mon", "branch_maadi", "cashier", _utc(2026, 11, 23, 8), _utc(2026, 11, 23, 18), 1),  # 10h
        ShiftRequirementInput("sh_tue", "branch_maadi", "cashier", _utc(2026, 11, 24, 8), _utc(2026, 11, 24, 18), 1),  # 10h
        ShiftRequirementInput("sh_wed", "branch_maadi", "cashier", _utc(2026, 11, 25, 8), _utc(2026, 11, 25, 18), 1),  # 10h
        ShiftRequirementInput("sh_thu", "branch_maadi", "cashier", _utc(2026, 11, 26, 8), _utc(2026, 11, 26, 18), 1),  # 10h
        ShiftRequirementInput("sh_fri", "branch_maadi", "cashier", _utc(2026, 11, 27, 8), _utc(2026, 11, 27, 14), 1),  # 6h
    )
    emp = (
        EmployeeInput(
            employee_id="emp_wk",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )

    req_fits_48h = SolveRequest(
        "ros_wk_fits",
        "branch_maadi",
        emp,
        (*w48_shifts_46h, sun_mon_shift),
        _cairo_profile(max_weekly="48.00", max_daily="10.00", max_consec=6, budget="2000.00"),
        iana_timezone="UTC",
    )
    res_fits = solve_roster(req_fits_48h)
    assert res_fits.status == "optimal"
    assert res_fits.verification.passed is True
    assert res_fits.metrics is not None
    # W48: 48h = 40h regular + 8h OT; W49: 6h = 6h regular + 0h OT => total 46h regular + 8h OT!
    assert res_fits.metrics.total_regular_minutes == 46 * 60
    assert res_fits.metrics.total_overtime_minutes == 8 * 60

    # Now add 1 more hour on Friday (47h in W48 + 2h from Sun 22:00Z->Mon 06:00Z = 49h > 48h in W48 under UTC -> INFEASIBLE!).
    # Yet under Africa/Cairo (where Sun 22:00Z is Mon 00:00+02:00), W48 has 47h <= 48h and W49 has 8h -> OPTIMAL!
    w48_shifts_47h = (
        *w48_shifts_46h[:4],
        ShiftRequirementInput("sh_fri", "branch_maadi", "cashier", _utc(2026, 11, 27, 8), _utc(2026, 11, 27, 15), 1),  # 7h
    )
    req_exceeds_utc = SolveRequest(
        "ros_wk_exceeds_utc",
        "branch_maadi",
        emp,
        (*w48_shifts_47h, sun_mon_shift),
        _cairo_profile(max_weekly="48.00", max_daily="10.00", max_consec=6, budget="2000.00"),
        iana_timezone="UTC",
    )
    res_exceeds_utc = solve_roster(req_exceeds_utc)
    assert res_exceeds_utc.status == "infeasible"
    # Overtime repair MUST NOT violate the 48h statutory weekly cap — only `relax_coverage` is legal!
    assert all(r.kind == "relax_coverage" for r in res_exceeds_utc.repair_suggestions)

    req_fits_cairo = SolveRequest(
        "ros_wk_fits_cairo",
        "branch_maadi",
        emp,
        (*w48_shifts_47h, sun_mon_shift),
        _cairo_profile(max_weekly="48.00", max_daily="10.00", max_consec=6, budget="2000.00"),
        iana_timezone="Africa/Cairo",
    )
    res_fits_cairo = solve_roster(req_fits_cairo)
    assert res_fits_cairo.status == "optimal"
    assert res_fits_cairo.verification.passed is True


def test_overtime_repair_never_relaxes_statutory_weekly_cap() -> None:
    # 6 shifts of 9h = 54h in one ISO week; statutory `profile.max_weekly_hours = 48.00`.
    # Even though overtime repair is enabled, statutory 48h ceiling MUST NEVER be violated!
    base_day = datetime(2026, 11, 23, 8, 0, tzinfo=UTC)
    six_9h_shifts = tuple(
        ShiftRequirementInput(
            f"sh_9h_{d}",
            "branch_maadi",
            "cashier",
            base_day + timedelta(days=d),
            base_day + timedelta(days=d, hours=9),
            1,
        )
        for d in range(6)
    )
    solo = (
        EmployeeInput(
            employee_id="emp_solo",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )
    req = SolveRequest(
        "ros_stat_weekly_cap",
        "branch_maadi",
        solo,
        six_9h_shifts,
        _cairo_profile(max_weekly="48.00", max_daily="10.00", max_consec=6, budget="5000.00"),
    )
    res = solve_roster(req)
    assert res.status == "infeasible"
    assert res.repair_suggestions
    # Must NEVER suggest `approve_overtime` above 48h statutory cap!
    assert all(r.kind == "relax_coverage" for r in res.repair_suggestions)
    assert all(r.kind != "approve_overtime" for r in res.minimal_repairs)

    # Independent `verify_statutory_laws` MUST also reject 54h > 48h!
    illegal_54h = tuple(
        Assignment(
            sh.shift_id,
            "emp_solo",
            sh.unit_id,
            sh.role_code,
            sh.starts_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
            sh.ends_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
            sh.duration_minutes,
            sh.duration_hours,
        )
        for sh in six_9h_shifts
    )
    stat_ver = verify_statutory_laws(req, illegal_54h)
    assert not stat_ver.passed
    assert "HR-SH-STAT:max_weekly_hours_exceeded" in {v.code for v in stat_ver.violations}


def test_america_st_johns_dst_midnight_rollback_151_vs_150_cap() -> None:
    # America/St_Johns 2009-11-01 DST fallback rolls 00:01 NDT (-02:30) back to 23:01 NST (-03:30) on 2009-10-31!
    # Shift [2009-11-01T00:58:00Z, 2009-11-01T04:30:00Z) = 212 elapsed UTC minutes:
    #   - 00:58Z..02:30Z = 2009-10-31 22:28..24:00 (-02:30) ->  92 min on 2009-10-31
    #   - 02:30Z..02:31Z = 2009-11-01 00:00..00:01 (-02:30) ->   1 min on 2009-11-01
    #   - 02:31Z..03:30Z = 2009-10-31 23:01..24:00 (-03:30) ->  59 min on 2009-10-31 (rollback!)
    #   - 03:30Z..04:30Z = 2009-11-01 00:00..01:00 (-03:30) ->  60 min on 2009-11-01
    # Exact local totals: 151 min on 2009-10-31, 61 min on 2009-11-01.
    tz = ZoneInfo("America/St_Johns")
    shift = ShiftRequirementInput(
        "sh_st_johns_rollback",
        "branch_nl",
        "cashier",
        _utc(2009, 11, 1, 0, 58),
        _utc(2009, 11, 1, 4, 30),
        1,
    )
    split = split_interval_by_local_day(shift.starts_at, shift.ends_at, tz)
    assert split == {
        datetime(2009, 10, 31, tzinfo=UTC).date(): 151,
        datetime(2009, 11, 1, tzinfo=UTC).date(): 61,
    }
    assert sum(split.values()) == shift.duration_minutes == 212

    emp = (
        EmployeeInput(
            employee_id="emp_nl_1",
            home_unit_id="branch_nl",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )

    # Under 150-minute daily cap: MUST be refused (`infeasible`) AND independent verifier MUST reject!
    profile_150 = PreferenceProfileInput(
        profile_id="prof_150",
        currency="CAD",
        weekly_overtime_budget_minor=0,
        min_rest_minutes=660,
        max_daily_minutes=150,
        max_weekly_minutes=2880,
        max_consecutive_days=6,
    )
    req_150 = SolveRequest("ros_nl_150", "branch_nl", emp, (shift,), profile_150, iana_timezone="America/St_Johns")
    res_150 = solve_roster(req_150)
    assert res_150.status == "infeasible"

    forced = (
        Assignment(
            shift.shift_id,
            "emp_nl_1",
            shift.unit_id,
            shift.role_code,
            "2009-11-01T00:58:00Z",
            "2009-11-01T04:30:00Z",
            shift.duration_minutes,
            shift.duration_hours,
        ),
    )
    ver_150 = verify_roster(req_150, forced)
    assert not ver_150.passed
    assert "HR-SH-STAT:max_daily_hours_exceeded" in {v.code for v in ver_150.violations}

    # Under 151-minute daily cap: MUST succeed (`optimal`) AND independent verifier MUST pass!
    profile_151 = PreferenceProfileInput(
        profile_id="prof_151",
        currency="CAD",
        weekly_overtime_budget_minor=0,
        min_rest_minutes=660,
        max_daily_minutes=151,
        max_weekly_minutes=2880,
        max_consecutive_days=6,
    )
    req_151 = SolveRequest("ros_nl_151", "branch_nl", emp, (shift,), profile_151, iana_timezone="America/St_Johns")
    res_151 = solve_roster(req_151)
    assert res_151.status == "optimal"
    assert res_151.verification.passed is True
    assert verify_roster(req_151, res_151.assignments).passed is True


# ---------------------------------------------------------------------------
# 4. Honest Solver Statuses & Repair Optimality Tests
# ---------------------------------------------------------------------------


def test_honest_unknown_feasible_and_model_invalid_statuses(monkeypatch: pytest.MonkeyPatch) -> None:
    req = SolveRequest.from_dict(_valid_raw_payload())

    # 1. UNKNOWN -> status="unknown", evidence_label="unknown", NO false infeasibility, NO repair run!
    repair_called = False

    def _fail_if_repair_called(*_args: Any, **_kwargs: Any) -> Any:
        nonlocal repair_called
        repair_called = True
        raise AssertionError("UNSAT repair must NEVER be called when primary status is UNKNOWN")

    monkeypatch.setattr("norn_rostering_solver.solver._find_unsat_repairs", _fail_if_repair_called)
    monkeypatch.setattr(cp_model.CpSolver, "solve", lambda self, model, callback=None: cp_model.UNKNOWN)

    res_unk = solve_roster(req)
    assert res_unk.status == "unknown"
    assert res_unk.evidence_label == "unknown"
    assert res_unk.assignments == ()
    assert res_unk.metrics is None
    assert res_unk.repair_suggestions == ()
    assert res_unk.alternative_repairs == ()
    assert res_unk.unsat_summary is None
    assert res_unk.repair_optimality is None
    assert repair_called is False
    assert verify_receipt_digest(res_unk.to_dict()) is True

    # 2. MODEL_INVALID -> raises ModelInvalidError (NORN-SOLVER-MODEL-INVALID) & HTTP 500 status="error"
    monkeypatch.setattr(cp_model.CpSolver, "solve", lambda self, model, callback=None: cp_model.MODEL_INVALID)
    with pytest.raises(ModelInvalidError) as exc_inv:
        solve_roster(req)
    assert exc_inv.value.code == "NORN-SOLVER-MODEL-INVALID"

    status, body = handle_invoke_payload(canonical_json(_valid_raw_payload()).encode("utf-8"))
    assert status == 500
    assert body["code"] == "NORN-SOLVER-MODEL-INVALID"
    assert body["status"] == "error"


def test_honest_feasible_primary_and_feasible_slack_repair(monkeypatch: pytest.MonkeyPatch) -> None:
    # 1. Primary returns FEASIBLE (non-proven-optimal) -> status="feasible", evidence_label="bounded"
    real_solve = cp_model.CpSolver.solve

    def _primary_as_feasible(self: cp_model.CpSolver, model: cp_model.CpModel, callback: Any = None) -> Any:
        res = real_solve(self, model, callback)
        return cp_model.FEASIBLE if res == cp_model.OPTIMAL else res

    req = SolveRequest.from_dict(_valid_raw_payload())
    monkeypatch.setattr(cp_model.CpSolver, "solve", _primary_as_feasible)
    res_feas = solve_roster(req)
    assert res_feas.status == "feasible"
    assert res_feas.evidence_label == "bounded"
    assert res_feas.verification.passed is True
    assert verify_receipt_digest(res_feas.to_dict()) is True

    # 2. Repair returns FEASIBLE (not OPTIMAL) -> repair_optimality="feasible_slack", is_minimum_slack=False!
    unsat_payload = _valid_raw_payload()
    unsat_payload["shifts"][0]["required_count"] = 2
    unsat_req = SolveRequest.from_dict(unsat_payload)
    res_unsat_feas = solve_roster(unsat_req)
    assert res_unsat_feas.status == "infeasible"
    assert res_unsat_feas.evidence_label == "exhaustive-finite"
    assert res_unsat_feas.repair_optimality == "feasible_slack"
    assert res_unsat_feas.unsat_summary is not None
    assert "not proven minimum-slack" in res_unsat_feas.unsat_summary
    assert len(res_unsat_feas.repair_suggestions) == 1
    assert res_unsat_feas.repair_suggestions[0].optimality == "feasible_slack"
    assert res_unsat_feas.repair_suggestions[0].is_minimum_slack is False
    assert verify_receipt_digest(res_unsat_feas.to_dict()) is True


# ---------------------------------------------------------------------------
# 5. Bounded Cardinality, Invalid UTF-8, Concurrency, Timeout & Output Cap
# ---------------------------------------------------------------------------


def test_cardinality_limits_enforced_before_model_construction() -> None:
    # 1. Exceed MAX_EMPLOYEES
    too_many_emps = _valid_raw_payload()
    too_many_emps["employees"] = [
        {
            "employee_id": f"emp_{i:04d}",
            "home_unit_id": "branch_maadi",
            "skills": ["cashier"],
            "max_regular_weekly_minutes": 2400,
        }
        for i in range(MAX_EMPLOYEES + 1)
    ]
    with pytest.raises(LimitExceededError) as exc_e:
        SolveRequest.from_dict(too_many_emps)
    assert exc_e.value.code == "NORN-SOLVER-LIMIT-EXCEEDED"

    status, body = handle_invoke_payload(canonical_json(too_many_emps).encode("utf-8"))
    assert status == 422
    assert body["code"] == "NORN-SOLVER-LIMIT-EXCEEDED"

    # 2. Exceed MAX_SHIFTS
    too_many_shifts = _valid_raw_payload()
    too_many_shifts["shifts"] = [
        {
            "shift_id": f"sh_{i:04d}",
            "unit_id": "branch_maadi",
            "role_code": "cashier",
            "starts_at": "2026-11-28T06:00:00Z",
            "ends_at": "2026-11-28T14:00:00Z",
            "required_count": 1,
        }
        for i in range(MAX_SHIFTS + 1)
    ]
    with pytest.raises(LimitExceededError) as exc_s:
        SolveRequest.from_dict(too_many_shifts)
    assert exc_s.value.code == "NORN-SOLVER-LIMIT-EXCEEDED"

    # 3. Exceed MAX_HORIZON_DAYS
    too_wide_horizon = _valid_raw_payload()
    too_wide_horizon["shifts"] = [
        {
            "shift_id": "sh_start",
            "unit_id": "branch_maadi",
            "role_code": "cashier",
            "starts_at": "2026-01-01T08:00:00Z",
            "ends_at": "2026-01-01T16:00:00Z",
            "required_count": 1,
        },
        {
            "shift_id": "sh_end",
            "unit_id": "branch_maadi",
            "role_code": "cashier",
            "starts_at": (datetime(2026, 1, 1, 8, 0, tzinfo=UTC) + timedelta(days=MAX_HORIZON_DAYS)).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "ends_at": (datetime(2026, 1, 1, 16, 0, tzinfo=UTC) + timedelta(days=MAX_HORIZON_DAYS)).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "required_count": 1,
        },
    ]
    with pytest.raises(LimitExceededError) as exc_h:
        SolveRequest.from_dict(too_wide_horizon)
    assert exc_h.value.code == "NORN-SOLVER-LIMIT-EXCEEDED"

    # 4. Exceed MAX_LEAVE_BLOCKS
    too_many_leaves = _valid_raw_payload()
    too_many_leaves["employees"][0]["approved_leave"] = [
        {
            "starts_at": "2026-11-29T00:00:00Z",
            "ends_at": "2026-11-29T01:00:00Z",
        }
        for _ in range(MAX_LEAVE_BLOCKS + 1)
    ]
    with pytest.raises(LimitExceededError) as exc_l:
        SolveRequest.from_dict(too_many_leaves)
    assert exc_l.value.code == "NORN-SOLVER-LIMIT-EXCEEDED"

    # 5. Exceed MAX_ASSIGNMENT_VARS (`len(employees) * len(shifts) > 10_000`)
    too_many_vars = _valid_raw_payload()
    too_many_vars["employees"] = [
        {
            "employee_id": f"emp_v_{i:04d}",
            "home_unit_id": "branch_maadi",
            "skills": ["cashier"],
            "max_regular_weekly_minutes": 2400,
        }
        for i in range(200)
    ]
    too_many_vars["shifts"] = [
        {
            "shift_id": f"sh_v_{i:04d}",
            "unit_id": "branch_maadi",
            "role_code": "cashier",
            "starts_at": "2026-11-28T06:00:00Z",
            "ends_at": "2026-11-28T14:00:00Z",
            "required_count": 1,
        }
        for i in range(60)
    ]
    assert MAX_ASSIGNMENT_VARS < 200 * 60
    with pytest.raises(LimitExceededError) as exc_v:
        SolveRequest.from_dict(too_many_vars)
    assert exc_v.value.code == "NORN-SOLVER-LIMIT-EXCEEDED"

    # 6. All-overlapping worst-case payload exceeding MAX_CONFLICT_CONSTRAINTS (50,000) fails closed cleanly!
    # 40 employees × 250 identical overlapping shifts: 40*250 = 10,000 <= MAX_ASSIGNMENT_VARS,
    # but C(250, 2) * 40 = 1,245,000 > MAX_CONFLICT_CONSTRAINTS. Sweep-line aborts immediately!
    overlapping_emps = tuple(
        EmployeeInput(
            employee_id=f"emp_ov_{i:03d}",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_minutes=2400,
        )
        for i in range(40)
    )
    overlapping_shifts = tuple(
        ShiftRequirementInput(
            shift_id=f"sh_ov_{i:03d}",
            unit_id="branch_maadi",
            role_code="cashier",
            starts_at=_utc(2026, 11, 28, 6),
            ends_at=_utc(2026, 11, 28, 14),
            required_count=1,
        )
        for i in range(250)
    )
    assert len(overlapping_emps) * len(overlapping_shifts) <= MAX_ASSIGNMENT_VARS
    assert MAX_CONFLICT_CONSTRAINTS < (250 * 249 // 2) * 40
    req_conflict_bomb = SolveRequest(
        "ros_conflict_bomb",
        "branch_maadi",
        overlapping_emps,
        overlapping_shifts,
        _cairo_profile(),
    )
    with pytest.raises(LimitExceededError) as exc_c:
        solve_roster(req_conflict_bomb)
    assert exc_c.value.code == "NORN-SOLVER-LIMIT-EXCEEDED"


def test_invalid_utf8_and_response_byte_cap_fail_closed() -> None:
    # 1. Invalid UTF-8 bytes -> 400 NORN-SOLVER-JSON (never 500 traceback!)
    status, body = handle_invoke_payload(b"\xff\xfe\x80\x81")
    assert status == 400
    assert body["code"] == "NORN-SOLVER-JSON"
    assert body["status"] == "refused"

    # 2. Output response byte cap -> 500 NORN-SOLVER-RESPONSE-TOO-LARGE
    status, body = handle_invoke_payload(
        canonical_json(_valid_raw_payload()).encode("utf-8"),
        max_response_bytes=64,
    )
    assert status == 500
    assert body["code"] == "NORN-SOLVER-RESPONSE-TOO-LARGE"
    assert body["status"] == "refused"


def test_http_worker_concurrency_semaphore_and_socket_timeout() -> None:
    server = create_worker_server(
        "127.0.0.1",
        0,
        max_concurrent_solves=1,
        socket_timeout=0.15,
        acquire_timeout=0.0,
    )
    port = int(server.server_address[1])
    invoke_path = f"/v1/functions/{FUNCTION_ID}/invoke"
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    try:
        # 1. Concurrency semaphore exhaustion -> 503 NORN-SOLVER-BUSY BEFORE reading request body!
        assert server.concurrency_semaphore.acquire(blocking=False) is True
        try:
            # Send ONLY headers claiming a large Content-Length and ZERO body bytes:
            # if the worker read the body before acquiring the semaphore, this would stall and 408 timeout;
            # instead it must immediately refuse with 503 NORN-SOLVER-BUSY!
            with socket.create_connection(("127.0.0.1", port), timeout=2.0) as sock:
                sock.sendall(
                    (
                        f"POST {invoke_path} HTTP/1.1\r\n"
                        f"Host: 127.0.0.1:{port}\r\n"
                        "Content-Type: application/json\r\n"
                        "Content-Length: 100000\r\n"
                        "Connection: close\r\n"
                        "\r\n"
                    ).encode("ascii")
                )
                chunks_busy: list[bytes] = []
                while True:
                    data = sock.recv(4096)
                    if not data:
                        break
                    chunks_busy.append(data)
                raw_busy = b"".join(chunks_busy).decode("utf-8")
                assert "503" in raw_busy.splitlines()[0]
                parsed_busy = json.loads(raw_busy.split("\r\n\r\n", 1)[1])
                assert parsed_busy["code"] == "NORN-SOLVER-BUSY"
                assert parsed_busy["status"] == "refused"
        finally:
            server.concurrency_semaphore.release()

        # 2. Socket read timeout on slow/stalled body -> 408 NORN-SOLVER-TIMEOUT
        with socket.create_connection(("127.0.0.1", port), timeout=2.0) as sock:
            sock.sendall(
                (
                    f"POST {invoke_path} HTTP/1.1\r\n"
                    f"Host: 127.0.0.1:{port}\r\n"
                    "Content-Type: application/json\r\n"
                    "Content-Length: 500\r\n"
                    "Connection: close\r\n"
                    "\r\n"
                    '{"partial":'
                ).encode("ascii")
            )
            chunks: list[bytes] = []
            while True:
                data = sock.recv(4096)
                if not data:
                    break
                chunks.append(data)
            raw_resp = b"".join(chunks).decode("utf-8")
            assert "408" in raw_resp.splitlines()[0]
            body_text = raw_resp.split("\r\n\r\n", 1)[1]
            parsed_timeout = json.loads(body_text)
            assert parsed_timeout["code"] == "NORN-SOLVER-TIMEOUT"
            assert parsed_timeout["status"] == "refused"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


# ---------------------------------------------------------------------------
# 6. Empty-Roster Edge Cases
# ---------------------------------------------------------------------------


def test_empty_rosters_zero_shifts_and_zero_employees() -> None:
    profile = _cairo_profile()
    emp = EmployeeInput(
        employee_id="emp_1",
        home_unit_id="branch_maadi",
        skills=("cashier",),
        max_regular_weekly_hours=Decimal("40.00"),
        max_overtime_weekly_hours=Decimal("8.00"),
        overtime_cost_per_hour=Decimal("100.00"),
    )
    shift = ShiftRequirementInput(
        "sh_1",
        "branch_maadi",
        "cashier",
        _utc(2026, 11, 28, 6),
        _utc(2026, 11, 28, 14),
        2,
    )

    # 1. 0 shifts, 1 employee -> optimal empty schedule
    res_no_shifts = solve_roster(SolveRequest("ros_0sh", "branch_maadi", (emp,), (), profile))
    assert res_no_shifts.status == "optimal"
    assert res_no_shifts.evidence_label == "exhaustive-finite"
    assert res_no_shifts.assignments == ()
    assert res_no_shifts.metrics is not None
    assert res_no_shifts.metrics.total_shifts_filled == 0
    assert res_no_shifts.verification.passed is True
    assert verify_receipt_digest(res_no_shifts.to_dict()) is True

    # 2. 0 shifts, 0 employees -> optimal empty schedule
    res_all_empty = solve_roster(SolveRequest("ros_empty", "branch_maadi", (), (), profile))
    assert res_all_empty.status == "optimal"
    assert res_all_empty.assignments == ()
    assert res_all_empty.metrics is not None
    assert res_all_empty.metrics.total_shifts_filled == 0
    assert res_all_empty.verification.passed is True
    assert verify_receipt_digest(res_all_empty.to_dict()) is True

    # 3. 1 shift requiring 2 staff, 0 employees -> infeasible with minimum-slack relax_coverage (-2)
    res_no_emps = solve_roster(SolveRequest("ros_0emp", "branch_maadi", (), (shift,), profile))
    assert res_no_emps.status == "infeasible"
    assert res_no_emps.repair_optimality == "minimum_slack"
    assert len(res_no_emps.repair_suggestions) == 1
    assert res_no_emps.repair_suggestions[0].kind == "relax_coverage"
    assert res_no_emps.repair_suggestions[0].coverage_shortfall == 2
    assert res_no_emps.repair_suggestions[0].diagnostic_code == "HR-SH-04:repair_coverage"
    assert verify_receipt_digest(res_no_emps.to_dict()) is True


# ---------------------------------------------------------------------------
# 7. Determinism & Hard-Law Enforcement Tests
# ---------------------------------------------------------------------------


def test_deterministic_repeated_and_permuted_solves() -> None:
    shifts = (
        ShiftRequirementInput("sh_fri_pm", "branch_maadi", "cashier", _utc(2026, 11, 27, 14), _utc(2026, 11, 27, 22), 1, is_weekend=True),
        ShiftRequirementInput("sh_sat_am", "branch_maadi", "cashier", _utc(2026, 11, 28, 6), _utc(2026, 11, 28, 14), 1, is_weekend=True),
        ShiftRequirementInput("sh_sat_pm", "branch_maadi", "cashier", _utc(2026, 11, 28, 14), _utc(2026, 11, 28, 22), 1, is_weekend=True),
    )
    employees = (
        EmployeeInput(
            employee_id="emp_a",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
            preferred_shift_ids=("sh_fri_pm",),
        ),
        EmployeeInput(
            employee_id="emp_b",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
            approved_leave=(TimeWindow(_utc(2026, 11, 28, 0), _utc(2026, 11, 28, 14)),),
        ),
        EmployeeInput(
            employee_id="emp_c",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )
    req1 = SolveRequest("ros_w48", "branch_maadi", employees, shifts, _cairo_profile())
    req2 = SolveRequest("ros_w48", "branch_maadi", tuple(reversed(employees)), tuple(reversed(shifts)), _cairo_profile())

    r1 = solve_roster(req1)
    r2 = solve_roster(req1)
    r3 = solve_roster(req2)

    assert r1.contract == CONTRACT_ID
    assert r1.backend == "ortools_cpsat"
    assert r1.status == "optimal"
    assert r1.evidence_label == "exhaustive-finite"
    assert r1.verification.checked and r1.verification.passed and r1.verification.violation_count == 0
    assert r1.solver_receipt_digest.startswith("sha256:")
    assert len(r1.solver_receipt_digest) == len("sha256:") + 64
    assert canonical_json(r1.to_dict()) == canonical_json(r2.to_dict())
    assert canonical_json(r1.to_dict()) == canonical_json(r3.to_dict())
    assert r1.solver_receipt_digest == r2.solver_receipt_digest == r3.solver_receipt_digest
    assert verify_receipt_digest(r1.to_dict()) is True

    by_shift = {a.shift_id: a.employee_id for a in r1.assignments}
    assert by_shift["sh_fri_pm"] == "emp_a"
    assert by_shift["sh_sat_am"] == "emp_c"
    assert by_shift["sh_sat_pm"] == "emp_b"
    assert r1.metrics is not None
    assert r1.metrics.max_weekend_spread == 0


def test_hard_laws_max_consecutive_days_and_max_daily_and_weekly_hours() -> None:
    base_day = datetime(2026, 11, 23, 8, 0, tzinfo=UTC)
    seven_shifts = tuple(
        ShiftRequirementInput(
            shift_id=f"sh_d{d}",
            unit_id="branch_maadi",
            role_code="cashier",
            starts_at=base_day + timedelta(days=d),
            ends_at=base_day + timedelta(days=d, hours=8),
            required_count=1,
            is_weekend=(d in (4, 5)),
        )
        for d in range(7)
    )
    solo_emp = (
        EmployeeInput(
            employee_id="emp_solo",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("48.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )
    req_7days = SolveRequest(
        "ros_7d",
        "branch_maadi",
        solo_emp,
        seven_shifts,
        _cairo_profile(max_weekly="60.00", max_consec=6),
    )
    res_7d = solve_roster(req_7days)
    assert res_7d.status == "infeasible"

    two_same_day = (
        ShiftRequirementInput("sh_m", "branch_maadi", "cashier", _utc(2026, 11, 23, 6), _utc(2026, 11, 23, 12), 1),
        ShiftRequirementInput("sh_e", "branch_maadi", "cashier", _utc(2026, 11, 23, 12), _utc(2026, 11, 23, 18), 1),
    )
    req_daily = SolveRequest(
        "ros_daily",
        "branch_maadi",
        solo_emp,
        two_same_day,
        _cairo_profile(min_rest="0.00", max_daily="10.00"),
    )
    assert solve_roster(req_daily).status == "infeasible"

    six_9h_shifts = tuple(
        ShiftRequirementInput(
            shift_id=f"sh_9h_{d}",
            unit_id="branch_maadi",
            role_code="cashier",
            starts_at=base_day + timedelta(days=d),
            ends_at=base_day + timedelta(days=d, hours=9),
            required_count=1,
        )
        for d in range(6)
    )
    req_weekly = SolveRequest(
        "ros_weekly",
        "branch_maadi",
        solo_emp,
        six_9h_shifts,
        _cairo_profile(max_weekly="48.00", max_daily="10.00", max_consec=6),
    )
    assert solve_roster(req_weekly).status == "infeasible"


# ---------------------------------------------------------------------------
# 8. Independent Hard-Law Post-Verifier Negative Tests (ORACLE-CHECKLIST aligned)
# ---------------------------------------------------------------------------


def test_independent_post_verifier_rejects_every_hard_law_violation() -> None:
    s1 = ShiftRequirementInput("sh_1", "branch_maadi", "cashier", _utc(2026, 11, 23, 8), _utc(2026, 11, 23, 16), 1)
    s2 = ShiftRequirementInput("sh_2", "branch_maadi", "cashier", _utc(2026, 11, 23, 14), _utc(2026, 11, 23, 22), 1)
    s3 = ShiftRequirementInput("sh_3", "branch_maadi", "barista", _utc(2026, 11, 24, 0), _utc(2026, 11, 24, 8), 1)

    emp_maadi = EmployeeInput(
        employee_id="emp_1",
        home_unit_id="branch_maadi",
        skills=("cashier",),
        max_regular_weekly_hours=Decimal("8.00"),
        max_overtime_weekly_hours=Decimal("0.00"),
        overtime_cost_per_hour=Decimal("100.00"),
        approved_leave=(TimeWindow(_utc(2026, 11, 23, 15), _utc(2026, 11, 23, 20)),),
    )
    emp_zamalek = EmployeeInput(
        employee_id="emp_2",
        home_unit_id="branch_zamalek",
        skills=("cashier",),
        max_regular_weekly_hours=Decimal("40.00"),
        max_overtime_weekly_hours=Decimal("0.00"),
        overtime_cost_per_hour=Decimal("100.00"),
    )
    req = SolveRequest(
        "ros_ver",
        "branch_maadi",
        (emp_maadi, emp_zamalek),
        (s1, s2, s3),
        _cairo_profile(min_rest="11.00", max_daily="10.00", max_weekly="8.00", max_consec=1, budget="0.00"),
    )

    bad_assignments = (
        Assignment("sh_1", "emp_1", "branch_maadi", "cashier", "2026-11-23T08:00:00Z", "2026-11-23T16:00:00Z", 480, Decimal("8.00")),
        Assignment("sh_2", "emp_1", "branch_maadi", "cashier", "2026-11-23T14:00:00Z", "2026-11-23T22:00:00Z", 480, Decimal("8.00")),
        Assignment("sh_3", "emp_1", "branch_maadi", "barista", "2026-11-24T00:00:00Z", "2026-11-24T08:00:00Z", 480, Decimal("8.00")),
        Assignment("sh_1", "emp_2", "branch_maadi", "cashier", "2026-11-23T08:00:00Z", "2026-11-23T16:00:00Z", 480, Decimal("8.00"), is_cross_branch=True),
    )

    res = verify_roster(req, bad_assignments)
    assert not res.passed
    codes = {v.code for v in res.violations}
    # Oracle-aligned HR-SH-01..03 codes
    assert "HR-SH-01:overlapping_shifts" in codes
    assert "HR-SH-02:approved_leave_overlap" in codes
    assert "HR-SH-03:min_rest_violated" in codes
    # Coverage, eligibility, statutory caps & overtime budget
    assert "HR-SH-COV:coverage_mismatch" in codes
    assert "HR-SH-ELIG:role_ineligible" in codes
    assert "HR-SH-ELIG:unit_ineligible" in codes
    assert "HR-SH-STAT:max_daily_hours_exceeded" in codes
    assert "HR-SH-STAT:max_weekly_hours_exceeded" in codes
    assert "HR-SH-STAT:max_consecutive_days_exceeded" in codes
    assert "HR-SH-BUDGET:overtime_budget_exceeded" in codes

    with pytest.raises(PostVerificationError) as exc_info:
        verify_or_raise(req, bad_assignments)
    assert exc_info.value.code == "NORN-SOLVER-POSTVERIFY-FAILED"


# ---------------------------------------------------------------------------
# 9. Realistic 25-Staff × 50-Shift Cairo Multi-Branch Benchmark (< 2s)
# ---------------------------------------------------------------------------


def _build_cairo_25x50_benchmark() -> SolveRequest:
    """25 staff × 50 shifts across 3 Cairo branches (Maadi, Zamalek, New Cairo) under Egypt labor law."""
    branches = ("branch_maadi", "branch_zamalek", "branch_new_cairo")
    employees: list[EmployeeInput] = []

    roster_spec: list[tuple[str, tuple[str, ...], int]] = [
        ("branch_maadi", ("cashier",), 0),
        ("branch_maadi", ("cashier", "barista"), 1),
        ("branch_maadi", ("cashier",), 2),
        ("branch_maadi", ("cashier",), 1),
        ("branch_maadi", ("barista",), 0),
        ("branch_maadi", ("barista",), 2),
        ("branch_maadi", ("barista", "cashier"), 1),
        ("branch_maadi", ("shift_lead", "cashier"), 1),
        ("branch_maadi", ("shift_lead", "barista"), 0),
        ("branch_zamalek", ("cashier",), 1),
        ("branch_zamalek", ("cashier", "barista"), 0),
        ("branch_zamalek", ("cashier",), 2),
        ("branch_zamalek", ("barista",), 1),
        ("branch_zamalek", ("barista",), 0),
        ("branch_zamalek", ("barista", "cashier"), 2),
        ("branch_zamalek", ("shift_lead", "cashier"), 0),
        ("branch_zamalek", ("shift_lead", "barista"), 1),
        ("branch_new_cairo", ("cashier",), 0),
        ("branch_new_cairo", ("cashier", "barista"), 2),
        ("branch_new_cairo", ("cashier",), 1),
        ("branch_new_cairo", ("barista",), 0),
        ("branch_new_cairo", ("barista",), 1),
        ("branch_new_cairo", ("barista", "cashier"), 0),
        ("branch_new_cairo", ("shift_lead", "cashier"), 2),
        ("branch_new_cairo", ("shift_lead", "barista"), 1),
    ]
    assert len(roster_spec) == 25

    week_start = datetime(2026, 11, 23, 0, 0, tzinfo=UTC)
    for idx, (home, skills, prior_wk) in enumerate(roster_spec, start=1):
        leave: tuple[TimeWindow, ...] = ()
        if idx == 1:
            leave = (TimeWindow(week_start + timedelta(days=2), week_start + timedelta(days=3)),)
        elif idx == 10:
            leave = (TimeWindow(week_start + timedelta(days=4), week_start + timedelta(days=5)),)
        elif idx == 18:
            leave = (TimeWindow(week_start + timedelta(days=5), week_start + timedelta(days=6)),)
        elif idx == 22:
            leave = (TimeWindow(week_start + timedelta(days=1), week_start + timedelta(days=2)),)

        employees.append(
            EmployeeInput(
                employee_id=f"emp_cairo_{idx:02d}",
                home_unit_id=home,
                eligible_unit_ids=branches,
                skills=skills,
                max_regular_weekly_hours=Decimal("40.00"),
                max_overtime_weekly_hours=Decimal("8.00"),
                overtime_cost_per_hour=Decimal("120.00"),
                approved_leave=leave,
                prior_weekend_shifts=prior_wk,
            )
        )

    shifts: list[ShiftRequirementInput] = []
    shift_seq = 0
    for day_offset in range(7):
        is_wk = day_offset in (4, 5)
        day_dt = week_start + timedelta(days=day_offset)
        for branch in branches:
            shift_seq += 1
            shifts.append(
                ShiftRequirementInput(
                    shift_id=f"sh_{shift_seq:02d}_{branch}_d{day_offset}_am_cashier",
                    unit_id=branch,
                    role_code="cashier",
                    starts_at=day_dt + timedelta(hours=6),
                    ends_at=day_dt + timedelta(hours=14),
                    required_count=2 if (branch == "branch_maadi" and is_wk) else 1,
                    is_weekend=is_wk,
                )
            )
            shift_seq += 1
            shifts.append(
                ShiftRequirementInput(
                    shift_id=f"sh_{shift_seq:02d}_{branch}_d{day_offset}_pm_barista",
                    unit_id=branch,
                    role_code="barista",
                    starts_at=day_dt + timedelta(hours=14),
                    ends_at=day_dt + timedelta(hours=22),
                    required_count=1,
                    is_weekend=is_wk,
                )
            )

    peak_lead_slots: list[tuple[str, int, int, int, bool]] = [
        ("branch_maadi", 3, 12, 20, False),
        ("branch_zamalek", 3, 12, 20, False),
        ("branch_maadi", 4, 10, 18, True),
        ("branch_zamalek", 4, 10, 18, True),
        ("branch_new_cairo", 4, 10, 18, True),
        ("branch_maadi", 5, 10, 18, True),
        ("branch_zamalek", 5, 10, 18, True),
        ("branch_new_cairo", 5, 10, 18, True),
    ]
    for branch, day_offset, start_h, end_h, is_wk in peak_lead_slots:
        shift_seq += 1
        day_dt = week_start + timedelta(days=day_offset)
        shifts.append(
            ShiftRequirementInput(
                shift_id=f"sh_{shift_seq:02d}_{branch}_d{day_offset}_lead",
                unit_id=branch,
                role_code="shift_lead",
                starts_at=day_dt + timedelta(hours=start_h),
                ends_at=day_dt + timedelta(hours=end_h),
                required_count=1,
                is_weekend=is_wk,
            )
        )

    assert len(shifts) == 50

    return SolveRequest(
        roster_id="ros_cairo_2026_w48",
        unit_id="cairo_region",
        employees=tuple(employees),
        shifts=tuple(shifts),
        profile=_cairo_profile(
            profile_id="prof_cairo_eg_statutory",
            budget="3000.00",
            min_rest="11.00",
            max_daily="10.00",
            max_weekly="48.00",
            max_consec=6,
        ),
        iana_timezone="Africa/Cairo",
    )


def test_cairo_25_staff_50_shift_multibranch_benchmark_under_2s() -> None:
    req = _build_cairo_25x50_benchmark()

    t0 = time.perf_counter()
    res1 = solve_roster(req)
    elapsed_s = time.perf_counter() - t0

    assert res1.backend == "ortools_cpsat"
    assert res1.status in ("optimal", "feasible")
    assert elapsed_s < 2.0, f"25×50 Cairo benchmark took {elapsed_s:.3f}s (budget < 2.0s)"
    assert res1.verification.checked and res1.verification.passed and res1.verification.violation_count == 0
    assert verify_receipt_digest(res1.to_dict()) is True

    ver = verify_or_raise(req, res1.assignments)
    assert ver.passed

    expected_slots = sum(s.required_count for s in req.shifts)
    assert len(res1.assignments) == expected_slots
    assert res1.metrics is not None
    assert res1.metrics.total_shifts_filled == expected_slots

    res2 = solve_roster(req)
    assert res1.solver_receipt_digest == res2.solver_receipt_digest
    assert canonical_json(res1.to_dict()) == canonical_json(res2.to_dict())


# ---------------------------------------------------------------------------
# 10. Multi-Repair Slack-Variable UNSAT Diagnostics Tests
# ---------------------------------------------------------------------------


def test_multi_repair_unsat_simultaneous_coverage_shortfall_and_overtime_constraint() -> None:
    shifts = (
        ShiftRequirementInput("sh_sat_cashier", "branch_maadi", "cashier", _utc(2026, 11, 28, 6), _utc(2026, 11, 28, 14), 2, is_weekend=True),
        ShiftRequirementInput("sh_fri_barista", "branch_maadi", "barista", _utc(2026, 11, 27, 6), _utc(2026, 11, 27, 14), 1, is_weekend=True),
        ShiftRequirementInput("sh_sat_barista", "branch_maadi", "barista", _utc(2026, 11, 28, 14), _utc(2026, 11, 28, 22), 1, is_weekend=True),
    )
    employees = (
        EmployeeInput(
            employee_id="emp_cashier_1",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
        EmployeeInput(
            employee_id="emp_barista_1",
            home_unit_id="branch_maadi",
            skills=("barista",),
            max_regular_weekly_hours=Decimal("8.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("125.00"),
        ),
    )
    req = SolveRequest("ros_multi_unsat", "branch_maadi", employees, shifts, _cairo_profile(budget="0.00"))
    res = solve_roster(req)

    assert res.status == "infeasible"
    assert res.evidence_label == "exhaustive-finite"
    assert res.repair_optimality == "minimum_slack"
    assert res.assignments == ()
    assert res.unsat_summary is not None
    assert verify_receipt_digest(res.to_dict()) is True

    kinds = {r.kind for r in res.repair_suggestions}
    assert kinds == {"relax_coverage", "approve_overtime"}

    cov_repair = next(r for r in res.repair_suggestions if r.kind == "relax_coverage")
    assert cov_repair.target_shift_id == "sh_sat_cashier"
    assert cov_repair.coverage_shortfall == 1
    assert cov_repair.requires_approval is True
    assert cov_repair.is_minimum_slack is True
    assert cov_repair.diagnostic_code == "HR-SH-04:repair_coverage"

    ot_repair = next(r for r in res.repair_suggestions if r.kind == "approve_overtime")
    assert ot_repair.employee_id == "emp_barista_1"
    assert ot_repair.delta_minutes == 480
    assert ot_repair.delta_hours == Decimal("8.00")
    assert ot_repair.cost_delta == Decimal("1000.00")
    assert ot_repair.cost_delta_minor == 100_000
    assert ot_repair.requires_approval is True
    assert ot_repair.is_minimum_slack is True
    assert ot_repair.diagnostic_code == "HR-SH-04:repair_overtime"
    assert "emp_barista_1" in ot_repair.hint

    res_repeat = solve_roster(req)
    assert res.solver_receipt_digest == res_repeat.solver_receipt_digest


def test_unsat_single_shortage_returns_transfer_and_alternatives() -> None:
    shifts = (
        ShiftRequirementInput("sh_fri_am", "branch_maadi", "cashier", _utc(2026, 11, 27, 6), _utc(2026, 11, 27, 14), 1, is_weekend=True),
        ShiftRequirementInput("sh_sat_pm", "branch_maadi", "cashier", _utc(2026, 11, 28, 14), _utc(2026, 11, 28, 22), 2, is_weekend=True),
    )
    employees = (
        EmployeeInput(
            employee_id="emp_a",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("8.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
        EmployeeInput(
            employee_id="emp_b",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("8.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("110.00"),
        ),
        EmployeeInput(
            employee_id="emp_c",
            home_unit_id="branch_zamalek",
            eligible_unit_ids=("branch_zamalek", "branch_maadi"),
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )
    req = SolveRequest("ros_shortage", "branch_maadi", employees, shifts, _cairo_profile(budget="0.00"))
    res = solve_roster(req)

    assert res.status == "infeasible"
    assert res.repair_optimality == "minimum_slack"
    assert len(res.repair_suggestions) == 1
    transfer = res.repair_suggestions[0]
    assert transfer.kind == "allow_transfer"
    assert transfer.transfer_mode == "cross_branch"
    assert transfer.employee_id == "emp_c"
    assert transfer.from_unit_id == "branch_zamalek"
    assert transfer.to_unit_id == "branch_maadi"
    assert transfer.cost_delta == Decimal("0.00")
    assert transfer.diagnostic_code == "HR-SH-04:repair_transfer"
    assert "emp_c" in transfer.hint

    all_kinds = {r.kind for r in res.minimal_repairs}
    assert all_kinds == {"allow_transfer", "approve_overtime", "relax_coverage"}


def test_unsat_cross_skill_transfer_and_leave_protection() -> None:
    shifts = (
        ShiftRequirementInput("sh_cash", "branch_maadi", "cashier", _utc(2026, 11, 28, 6), _utc(2026, 11, 28, 14), 1),
    )
    emp_cross = (
        EmployeeInput(
            employee_id="emp_bar",
            home_unit_id="branch_maadi",
            skills=("barista",),
            cross_trainable_skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )
    res_cross = solve_roster(SolveRequest("ros_xskill", "branch_maadi", emp_cross, shifts, _cairo_profile()))
    assert res_cross.status == "infeasible"
    assert len(res_cross.repair_suggestions) == 1
    assert res_cross.repair_suggestions[0].kind == "allow_transfer"
    assert res_cross.repair_suggestions[0].transfer_mode == "cross_skill"
    assert res_cross.repair_suggestions[0].employee_id == "emp_bar"

    emp_on_leave = (
        EmployeeInput(
            employee_id="emp_leave",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
            approved_leave=(TimeWindow(_utc(2026, 11, 28, 0), _utc(2026, 11, 29, 0)),),
        ),
    )
    res_leave = solve_roster(SolveRequest("ros_leave_block", "branch_maadi", emp_on_leave, shifts, _cairo_profile()))
    assert res_leave.status == "infeasible"
    assert len(res_leave.repair_suggestions) == 1
    assert res_leave.repair_suggestions[0].kind == "relax_coverage"
    assert res_leave.repair_suggestions[0].coverage_shortfall == 1


# ---------------------------------------------------------------------------
# 11. What-If Profile Comparison (Fairness-First vs Cost-First)
# ---------------------------------------------------------------------------


def test_what_if_fairness_first_vs_cost_first() -> None:
    shifts = (
        ShiftRequirementInput("sh_sat_am", "branch_maadi", "cashier", _utc(2026, 11, 28, 6), _utc(2026, 11, 28, 14), 1, is_weekend=True),
    )
    employees = (
        EmployeeInput(
            employee_id="emp_a",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("0.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
            prior_weekend_shifts=0,
        ),
        EmployeeInput(
            employee_id="emp_b",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
            prior_weekend_shifts=3,
        ),
    )
    cost_first = _cairo_profile(profile_id="cost_first", budget="0.00", weekend_fairness="low")
    fairness_first = _cairo_profile(profile_id="fairness_first", budget="1000.00", weekend_fairness="high")

    diff = compare_what_if(
        SolveRequest("ros_whatif", "branch_maadi", employees, shifts, cost_first),
        fairness_first,
    )
    assert diff.base_status == "optimal"
    assert diff.alt_status == "optimal"
    assert diff.changed_assignment_count == 1
    assert diff.changed_assignments[0].shift_id == "sh_sat_am"
    assert diff.changed_assignments[0].base_employee_ids == ("emp_b",)
    assert diff.changed_assignments[0].alt_employee_ids == ("emp_a",)
    assert diff.overtime_minutes_delta == 480
    assert diff.overtime_hours_delta == Decimal("8.00")
    assert diff.overtime_cost_minor_delta == 80_000
    assert diff.overtime_cost_delta == Decimal("800.00")
    assert diff.max_weekend_spread_delta == -2
    assert diff.objective_score_delta is not None
    assert "+800.00 EGP" in diff.summary


# ---------------------------------------------------------------------------
# 12. Strict `FunctionManifest` Schema Alignment & Live HTTP Worker Tests
# ---------------------------------------------------------------------------


def _assert_manifest_passes_norn_checker_rules(manifest: dict[str, object]) -> None:
    """Verify EVERY admission check from `crates/norn-checker/src/function.rs` (`check_function_manifest`)."""
    repo_root = Path(__file__).resolve().parents[3]
    schema_path = repo_root / "crates" / "norn-ir" / "schema" / "function_manifest.schema.json"
    schema = json.loads(schema_path.read_text(encoding="utf-8"))

    # 1. Exact closed schema keys (`#[serde(deny_unknown_fields)]`)
    allowed_top_keys = set(schema["properties"].keys())
    required_top_keys = set(schema["required"])
    assert set(manifest.keys()) <= allowed_top_keys, f"extra keys: {set(manifest.keys()) - allowed_top_keys}"
    assert required_top_keys <= set(manifest.keys()), f"missing keys: {required_top_keys - set(manifest.keys())}"

    # 2. ABI & NFC canonical text
    assert manifest["abi"] == "norn.function-abi/1"
    canon_text = canonical_json(manifest)
    assert unicodedata.normalize("NFC", canon_text) == canon_text

    digest_re = re.compile(r"^sha256:[0-9a-f]{64}$")
    defs = schema["$defs"]

    # 3. Implementation (`format != "host"` -> avoids `function.manifest.authored_host`)
    impl = manifest["implementation"]
    assert isinstance(impl, dict)
    assert set(impl.keys()) <= set(defs["Implementation"]["properties"].keys())
    assert set(defs["Implementation"]["required"]) <= set(impl.keys())
    assert impl["format"] == "oci_image"
    assert isinstance(impl["entryPoint"], str) and impl["entryPoint"].strip()
    assert isinstance(impl["digest"], str) and digest_re.match(impl["digest"])
    locator = impl.get("locator")
    if locator is not None:
        assert isinstance(locator, str)
        pinned = f"@{impl['digest']}"
        assert locator.endswith(pinned)
        ref = locator[: -len(pinned)]
        assert ref and "://" not in ref and not any(c in ref for c in ("@", "?", "#"))
        assert not any(c.isspace() for c in ref)

    # 4. Non-blank names & well-formed digests across schemas, runtime, platform
    assert isinstance(manifest["functionId"], str) and manifest["functionId"].strip()
    for schema_field in ("inputSchema", "outputSchema", "errorSchema"):
        sref = manifest[schema_field]
        assert isinstance(sref, dict)
        assert set(sref.keys()) == set(defs["SchemaRef"]["properties"].keys())
        assert isinstance(sref["id"], str) and sref["id"].strip()
        assert isinstance(sref["digest"], str) and digest_re.match(sref["digest"])

    runtime = manifest["runtime"]
    assert isinstance(runtime, dict)
    assert set(runtime.keys()) == set(defs["RuntimeIdentity"]["properties"].keys())
    assert isinstance(runtime["id"], str) and runtime["id"].strip()
    assert isinstance(runtime["version"], str) and runtime["version"].strip()
    assert isinstance(runtime["digest"], str) and digest_re.match(runtime["digest"])

    plat = manifest["platform"]
    assert isinstance(plat, dict)
    assert set(plat.keys()) == set(defs["Platform"]["properties"].keys())
    assert isinstance(plat["os"], str) and plat["os"].strip()
    assert isinstance(plat["arch"], str) and plat["arch"].strip()

    # 5. Dependency closure: non-null, non-empty for oci_image, strictly sorted by name, exact computed_digest
    dep = manifest["dependencyClosure"]
    assert isinstance(dep, dict)
    assert set(dep.keys()) == set(defs["DependencyClosure"]["properties"].keys())
    entries = dep["entries"]
    assert isinstance(entries, list) and len(entries) > 0
    prev_name: str | None = None
    for entry in entries:
        assert isinstance(entry, dict)
        assert set(entry.keys()) == set(defs["DependencyEntry"]["properties"].keys())
        name = entry["name"]
        assert isinstance(name, str) and name.strip()
        if prev_name is not None:
            assert prev_name < name
        prev_name = name
        assert isinstance(entry["digest"], str) and digest_re.match(entry["digest"])
    assert dep["digest"] == domain_sha256_digest(b"norn.function/dependency-closure/v1\0", entries)

    # 6. Determinism profile + cache policy + capabilities + positive bounds
    det = manifest["determinism"]
    assert isinstance(det, dict)
    assert set(det.keys()) == {"effectful"}
    eff = det["effectful"]
    assert isinstance(eff, dict)
    assert set(eff.keys()) == set(defs["EffectfulPolicy"]["properties"].keys())
    assert isinstance(eff["maxAttempts"], int) and eff["maxAttempts"] >= 1
    assert eff["providerIdempotency"] is True
    assert manifest["cache"] == "none"
    assert manifest["capabilities"] == []
    assert manifest["steps"] == []

    bounds = manifest["bounds"]
    assert isinstance(bounds, dict)
    assert set(bounds.keys()) == set(defs["ResourceBounds"]["properties"].keys())
    assert all(isinstance(v, int) and v > 0 for v in bounds.values())


def test_function_manifest_strictly_matches_norn_ir_schema_and_checker_rules() -> None:
    # 1. Default local mode (no published OCI image -> deterministic local bundle digest, locator=None)
    local_manifest = build_function_manifest()
    _assert_manifest_passes_norn_checker_rules(local_manifest)
    impl_local = local_manifest["implementation"]
    assert isinstance(impl_local, dict)
    assert impl_local["locator"] is None
    dep_entries = local_manifest["dependencyClosure"]
    assert isinstance(dep_entries, dict)
    entries = dep_entries["entries"]
    assert isinstance(entries, list)
    source_entry = next(e for e in entries if e["name"] == "norn-rostering-solver-source")
    assert source_entry["digest"] == package_source_digest()

    # 2. Published OCI container mode (explicit OCI image digest + registry ref)
    pinned_digest = "sha256:" + "a" * 64
    oci_manifest = build_function_manifest(
        oci_image_digest=pinned_digest,
        oci_image_ref="ghcr.io/norn-cloud/norn-rostering-solver",
    )
    _assert_manifest_passes_norn_checker_rules(oci_manifest)
    impl_oci = oci_manifest["implementation"]
    assert isinstance(impl_oci, dict)
    assert impl_oci["digest"] == pinned_digest
    assert impl_oci["locator"] == f"ghcr.io/norn-cloud/norn-rostering-solver@{pinned_digest}"


def _http_request(
    base_url: str,
    method: str,
    path: str,
    body: bytes | None = None,
) -> tuple[int, dict[str, Any]]:
    status, _headers, parsed = _http_request_with_headers(base_url, method, path, body)
    return status, parsed


def _http_request_with_headers(
    base_url: str,
    method: str,
    path: str,
    body: bytes | None = None,
) -> tuple[int, dict[str, str], dict[str, Any]]:
    req = urllib.request.Request(
        f"{base_url}{path}",
        data=body,
        method=method,
        headers={"Content-Type": "application/json"} if body is not None else {},
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            headers = {k.lower(): v for k, v in resp.headers.items()}
            return int(resp.status), headers, json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as err:
        headers = {k.lower(): v for k, v in err.headers.items()}
        return int(err.code), headers, json.loads(err.read().decode("utf-8"))


def test_live_http_worker_endpoints() -> None:
    server = create_worker_server("127.0.0.1", 0)
    port = int(server.server_address[1])
    base_url = f"http://127.0.0.1:{port}"
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    try:
        # 1. GET /health
        status, health = _http_request(base_url, "GET", "/health")
        assert status == 200
        assert health["status"] == "ok"
        assert health["backend"] == "ortools_cpsat"
        assert health["contract"] == CONTRACT_ID
        assert health["functionId"] == FUNCTION_ID
        assert health["version"] == FUNCTION_VERSION
        assert health["execution_contract"] == "unmarked_slice"
        assert health["source_digest"] == package_source_digest()
        assert health["manifest_digest"] == compute_manifest_digest()
        assert health["endpoints"]["invoke"] == f"/v1/functions/{FUNCTION_ID}/invoke"

        # 2. GET /manifest — strict checker-admissible FunctionManifest + digest header
        status, headers, manifest = _http_request_with_headers(base_url, "GET", "/manifest")
        assert status == 200
        assert manifest["abi"] == "norn.function-abi/1"
        assert manifest["functionId"] == FUNCTION_ID
        assert manifest["implementation"]["format"] == "oci_image"
        assert "manifestDigest" not in manifest
        assert "contractId" not in manifest
        assert "endpoints" not in manifest
        assert "semver" not in manifest
        assert headers["x-norn-manifest-digest"] == compute_manifest_digest(manifest)
        dep_names = [e["name"] for e in manifest["dependencyClosure"]["entries"]]
        assert dep_names == sorted(dep_names)
        assert "ortools" in dep_names
        assert manifest == build_function_manifest()

        # 3. POST /v1/functions/norn.solver.rostering/invoke — happy path (wrapped perform envelope)
        invoke_path = f"/v1/functions/{FUNCTION_ID}/invoke"
        envelope = {"invocation_id": "inv_001", "input": _valid_raw_payload()}
        status, result = _http_request(base_url, "POST", invoke_path, canonical_json(envelope).encode("utf-8"))
        assert status == 200
        assert result["status"] == "optimal"
        assert result["evidence_label"] == "exhaustive-finite"
        assert result["backend"] == "ortools_cpsat"
        assert result["invocation_id"] == "inv_001"
        assert result["solver_receipt_digest"].startswith("sha256:")
        assert verify_receipt_digest(result) is True
        assert result["perform_summary"] == {
            "assignmentCount": 1,
            "solverDigest": result["solver_receipt_digest"],
            "status": "optimal",
        }
        assert result["verification"]["passed"] is True

        # 4. POST /invoke — PII refusal fails closed with 422
        pii_payload = _valid_raw_payload()
        pii_payload["employees"][0]["bank_account"] = "EG0012345"
        status, pii_err = _http_request(base_url, "POST", invoke_path, json.dumps(pii_payload).encode("utf-8"))
        assert status == 422
        assert pii_err["code"] == "NORN-SOLVER-PII-FORBIDDEN"
        assert pii_err["status"] == "refused"

        # 5. POST /invoke — IEEE float refusal fails closed with 422
        float_body = json.dumps(_valid_raw_payload()).replace('"11.00"', "11.5").encode("utf-8")
        status, float_err = _http_request(base_url, "POST", invoke_path, float_body)
        assert status == 422
        assert float_err["code"] == "NORN-SOLVER-FLOAT-FORBIDDEN"

        # 6. POST /invoke — Invalid UTF-8 & duplicate JSON keys -> 400 (never 500!)
        status, utf8_err = _http_request(base_url, "POST", invoke_path, b"\xff\xfe")
        assert status == 400
        assert utf8_err["code"] == "NORN-SOLVER-JSON"

        status, dup_err = _http_request(base_url, "POST", invoke_path, b'{"roster_id":"a","roster_id":"b"}')
        assert status == 400
        assert dup_err["code"] == "NORN-SOLVER-DUPLICATE-KEY"

        # 7. POST /invoke — UNSAT multi-repair response
        unsat_payload = _valid_raw_payload()
        unsat_payload["shifts"][0]["required_count"] = 2
        status, unsat_res = _http_request(base_url, "POST", invoke_path, json.dumps(unsat_payload).encode("utf-8"))
        assert status == 200
        assert unsat_res["status"] == "infeasible"
        assert unsat_res["repair_optimality"] == "minimum_slack"
        assert len(unsat_res["repair_suggestions"]) == 1
        assert unsat_res["repair_suggestions"][0]["kind"] == "relax_coverage"
        assert unsat_res["repair_suggestions"][0]["coverage_shortfall"] == 1
        assert verify_receipt_digest(unsat_res) is True

        # 8. POST /invoke — what-if comparison over HTTP with independent comparison_digest verification
        what_if_envelope = _valid_raw_payload()
        what_if_envelope["alt_profile"] = {
            **what_if_envelope["profile"],
            "profile_id": "prof_2",
            "weekend_fairness_priority": "low",
        }
        status, wi_res = _http_request(base_url, "POST", invoke_path, canonical_json(what_if_envelope).encode("utf-8"))
        assert status == 200
        assert verify_receipt_digest(wi_res) is True
        assert wi_res["what_if"]["base_profile_id"] == "prof_1"
        assert wi_res["what_if"]["alt_profile_id"] == "prof_2"
        assert wi_res["what_if"]["comparison_digest"] == compute_comparison_digest(wi_res["what_if"])
        assert verify_comparison_digest(wi_res["what_if"]) is True
        tampered_wi = copy.deepcopy(wi_res["what_if"])
        tampered_wi["changed_assignment_count"] = 999
        assert verify_comparison_digest(tampered_wi) is False

        # 9. Unknown route returns 404
        status, not_found = _http_request(base_url, "GET", "/nonexistent")
        assert status == 404
        assert not_found["code"] == "NORN-SOLVER-NOT-FOUND"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def test_split_shift_avoidance_soft_objective() -> None:
    shifts = (
        ShiftRequirementInput("sh_am", "branch_maadi", "cashier", _utc(2026, 11, 23, 6), _utc(2026, 11, 23, 10), 1),
        ShiftRequirementInput("sh_pm", "branch_maadi", "cashier", _utc(2026, 11, 23, 14), _utc(2026, 11, 23, 18), 1),
    )
    employees = (
        EmployeeInput(
            employee_id="emp_1",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
            preferred_shift_ids=("sh_am", "sh_pm"),
        ),
        EmployeeInput(
            employee_id="emp_2",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )
    req = SolveRequest(
        "ros_split",
        "branch_maadi",
        employees,
        shifts,
        _cairo_profile(min_rest="2.00", staff_pref="low", avoid_split=True),
    )
    res = solve_roster(req)
    assert res.status == "optimal"
    assert res.metrics is not None
    assert res.metrics.split_shift_count == 0
    assigned_emps = {a.employee_id for a in res.assignments}
    assert assigned_emps == {"emp_1", "emp_2"}


def test_split_interval_by_local_day_conservation_across_dst() -> None:
    # America/New_York spring forward on 2026-03-08 (02:00 EST -> 03:00 EDT): 23-hour local day!
    ny = ZoneInfo("America/New_York")
    # 2026-03-08T04:00:00Z (= Sat Mar 7 23:00 EST) -> 2026-03-08T12:00:00Z (= Sun Mar 8 08:00 EDT) = 8h = 480 min
    s = _utc(2026, 3, 8, 4)
    e = _utc(2026, 3, 8, 12)
    split = split_interval_by_local_day(s, e, ny)
    assert sum(split.values()) == 480
    assert split == {
        datetime(2026, 3, 7, tzinfo=UTC).date(): 60,   # 23:00..24:00 EST
        datetime(2026, 3, 8, tzinfo=UTC).date(): 420,  # 00:00 EST..08:00 EDT (7 elapsed hours across spring forward)
    }
