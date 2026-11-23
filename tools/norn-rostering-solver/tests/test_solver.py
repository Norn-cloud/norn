"""Comprehensive test suite for the OR-Tools CP-SAT rostering solver (`norn.solver.rostering@0.1.0`)."""

from __future__ import annotations

import json
import threading
import time
import urllib.error
import urllib.request
from datetime import UTC, datetime, timedelta
from decimal import Decimal
from typing import Any

import pytest

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
    Assignment,
    EmployeeInput,
    FloatRefusalError,
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
    create_worker_server,
    handle_invoke_payload,
    normalize_field_key,
    parse_strict_json,
    solve_roster,
    verify_or_raise,
    verify_roster,
)


def _utc(year: int, month: int, day: int, hour: int, minute: int = 0) -> datetime:
    return datetime(year, month, day, hour, minute, tzinfo=UTC)


def _cairo_profile(
    *,
    profile_id: str = "prof_cairo_balanced",
    label: str = "Balanced Cairo Retail",
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
        label=label,
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
        "profile": {
            "profile_id": "prof_1",
            "label": "Cairo Balanced",
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
                "display_label": "Staff #01",
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
# 1. Fail-Closed PII & IEEE Float Boundary Tests
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
        # Known core PII keys
        ("name", "SYNTH_TOKEN_NAME"),
        ("national_id", "SYNTH_NID_0001"),
        ("phone", "SYNTH_PHONE_0001"),
        ("salary", "10000.00"),
        ("medical_note", "SYNTH_NOTE_0001"),
        # Previously missed / audit PII variants & casing normalizations
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
    # 1. Unknown key on perform envelope
    env_extra: dict[str, Any] = {
        "invocation_id": "inv_adv_1",
        "input": _valid_raw_payload(),
        "shadow_header": "opaque",
    }
    status, body = handle_invoke_payload(canonical_json(env_extra).encode("utf-8"))
    assert status == 422
    assert body["code"] == "NORN-SOLVER-UNKNOWN-FIELD"
    assert "$.shadow_header" in str(body["error"])

    # 2. Unknown key nested inside `input` of perform envelope
    env_nested: dict[str, Any] = {
        "invocation_id": "inv_adv_2",
        "input": _valid_raw_payload(),
    }
    env_nested["input"]["employees"][0]["custom_badge_code"] = "B-01"
    status, body = handle_invoke_payload(canonical_json(env_nested).encode("utf-8"))
    assert status == 422
    assert body["code"] == "NORN-SOLVER-UNKNOWN-FIELD"
    assert "$.input.employees[0].custom_badge_code" in str(body["error"])

    # 3. Unknown key inside `alt_profile`
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

    # 4. Previously missed PII key via HTTP invoke payload
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
        "allow_cross_branch_transfers": False,
        "authorized_transfer_employee_ids": [],
        "profile": {
            "profile_id": "prof_allowlist",
            "label": "Full Allowlist Cairo Profile",
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
                "display_label": "Staff Token #01",
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
    assert res1.verification.checked and res1.verification.passed and res1.verification.violation_count == 0
    assert len(res1.assignments) == 1
    assert res1.assignments[0].employee_id == "emp_token_01"
    assert res1.solver_receipt_digest == res2.solver_receipt_digest
    assert canonical_json(res1.to_dict()) == canonical_json(res2.to_dict())


# ---------------------------------------------------------------------------
# 2. Determinism & Hard-Law Enforcement Tests
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
            display_label="Cashier A",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
            preferred_shift_ids=("sh_fri_pm",),
        ),
        EmployeeInput(
            employee_id="emp_b",
            display_label="Cashier B",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
            approved_leave=(TimeWindow(_utc(2026, 11, 28, 0), _utc(2026, 11, 28, 14)),),
        ),
        EmployeeInput(
            employee_id="emp_c",
            display_label="Cashier C",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("8.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
    )
    req1 = SolveRequest("ros_w48", "branch_maadi", employees, shifts, _cairo_profile())
    # Reversed employee & shift order must produce byte-identical output and receipt digest!
    req2 = SolveRequest("ros_w48", "branch_maadi", tuple(reversed(employees)), tuple(reversed(shifts)), _cairo_profile())

    r1 = solve_roster(req1)
    r2 = solve_roster(req1)
    r3 = solve_roster(req2)

    assert r1.contract == CONTRACT_ID
    assert r1.backend == "ortools_cpsat"
    assert r1.status == "optimal"
    assert r1.verification.checked and r1.verification.passed and r1.verification.violation_count == 0
    assert r1.solver_receipt_digest.startswith("sha256:")
    assert len(r1.solver_receipt_digest) == len("sha256:") + 64
    assert canonical_json(r1.to_dict()) == canonical_json(r2.to_dict())
    assert canonical_json(r1.to_dict()) == canonical_json(r3.to_dict())
    assert r1.solver_receipt_digest == r2.solver_receipt_digest == r3.solver_receipt_digest

    by_shift = {a.shift_id: a.employee_id for a in r1.assignments}
    assert by_shift["sh_fri_pm"] == "emp_a"
    assert by_shift["sh_sat_am"] == "emp_c"
    assert by_shift["sh_sat_pm"] == "emp_b"
    assert r1.metrics is not None
    assert r1.metrics.max_weekend_spread == 0


def test_hard_laws_max_consecutive_days_and_max_daily_and_weekly_hours() -> None:
    # 7 consecutive days (Mon Nov 23 .. Sun Nov 29), 1 shift of 8h/day.
    # Single employee cannot work all 7 days because Egypt max_consecutive_days == 6 AND max_weekly_hours == 48h!
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
    # Even with generous 60h weekly cap, 7 consecutive days MUST be INFEASIBLE under max_consecutive_days=6!
    req_7days = SolveRequest(
        "ros_7d",
        "branch_maadi",
        solo_emp,
        seven_shifts,
        _cairo_profile(max_weekly="60.00", max_consec=6),
    )
    res_7d = solve_roster(req_7days)
    assert res_7d.status == "infeasible"

    # Max daily hours (10h): two 6h shifts on the same day with 0h rest = 12h > 10h -> INFEASIBLE for 1 person.
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

    # Profile max_weekly_hours (48h) caps employee even if employee record permits 56h.
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
# 3. Independent Hard-Law Post-Verifier Negative Tests
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
        # emp_1 works s1 (overlaps approved leave [15, 20)), s2 (overlaps s1 & daily > 10h & weekly > 8h),
        # and s3 (missing barista skill, < 11h rest after s2, 2 consecutive days > max_consec=1).
        Assignment("sh_1", "emp_1", "branch_maadi", "cashier", "2026-11-23T08:00:00Z", "2026-11-23T16:00:00Z", 480, Decimal("8.00")),
        Assignment("sh_2", "emp_1", "branch_maadi", "cashier", "2026-11-23T14:00:00Z", "2026-11-23T22:00:00Z", 480, Decimal("8.00")),
        Assignment("sh_3", "emp_1", "branch_maadi", "barista", "2026-11-24T00:00:00Z", "2026-11-24T08:00:00Z", 480, Decimal("8.00")),
        # Unauthorized cross-branch + over-coverage on sh_1
        Assignment("sh_1", "emp_2", "branch_maadi", "cashier", "2026-11-23T08:00:00Z", "2026-11-23T16:00:00Z", 480, Decimal("8.00"), is_cross_branch=True),
    )

    res = verify_roster(req, bad_assignments)
    assert not res.passed
    codes = {v.code for v in res.violations}
    assert "HR-SH-01:coverage_mismatch" in codes
    assert "HR-SH-02:role_ineligible" in codes
    assert "HR-SH-02:unit_ineligible" in codes
    assert "HR-SH-03:overlapping_shifts" in codes
    assert "HR-SH-03:min_rest_violated" in codes
    assert "HR-SH-04:max_daily_hours_exceeded" in codes
    assert "HR-SH-04:max_weekly_hours_exceeded" in codes
    assert "HR-SH-04:max_consecutive_days_exceeded" in codes
    assert "HR-SH-04:approved_leave_overlap" in codes
    assert "HR-SH-04:overtime_budget_exceeded" in codes

    with pytest.raises(PostVerificationError) as exc_info:
        verify_or_raise(req, bad_assignments)
    assert exc_info.value.code == "NORN-SOLVER-POSTVERIFY-FAILED"


# ---------------------------------------------------------------------------
# 4. Realistic 25-Staff × 50-Shift Cairo Multi-Branch Benchmark (< 2s)
# ---------------------------------------------------------------------------


def _build_cairo_25x50_benchmark() -> SolveRequest:
    """25 staff × 50 shifts across 3 Cairo branches (Maadi, Zamalek, New Cairo) under Egypt labor law."""
    branches = ("branch_maadi", "branch_zamalek", "branch_new_cairo")
    employees: list[EmployeeInput] = []

    # 25 employees distributed across 3 Cairo branches:
    # - branch_maadi: 9 staff (4 cashiers, 3 baristas, 2 shift_leads — some dual-skilled)
    # - branch_zamalek: 8 staff (3 cashiers, 3 baristas, 2 shift_leads)
    # - branch_new_cairo: 8 staff (3 cashiers, 3 baristas, 2 shift_leads)
    roster_spec: list[tuple[str, tuple[str, ...], int]] = [
        # Maadi (9)
        ("branch_maadi", ("cashier",), 0),
        ("branch_maadi", ("cashier", "barista"), 1),
        ("branch_maadi", ("cashier",), 2),
        ("branch_maadi", ("cashier",), 1),
        ("branch_maadi", ("barista",), 0),
        ("branch_maadi", ("barista",), 2),
        ("branch_maadi", ("barista", "cashier"), 1),
        ("branch_maadi", ("shift_lead", "cashier"), 1),
        ("branch_maadi", ("shift_lead", "barista"), 0),
        # Zamalek (8)
        ("branch_zamalek", ("cashier",), 1),
        ("branch_zamalek", ("cashier", "barista"), 0),
        ("branch_zamalek", ("cashier",), 2),
        ("branch_zamalek", ("barista",), 1),
        ("branch_zamalek", ("barista",), 0),
        ("branch_zamalek", ("barista", "cashier"), 2),
        ("branch_zamalek", ("shift_lead", "cashier"), 0),
        ("branch_zamalek", ("shift_lead", "barista"), 1),
        # New Cairo (8)
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

    week_start = datetime(2026, 11, 23, 0, 0, tzinfo=UTC)  # Monday 2026-W48
    for idx, (home, skills, prior_wk) in enumerate(roster_spec, start=1):
        # Add realistic approved leave for 4 employees on specific days.
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
                display_label=f"Cairo Staff #{idx:02d}",
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

    # Exactly 50 shifts across 7 days and 3 Cairo branches:
    # - 7 days × 3 branches × 2 slots (AM 06-14 cashier, PM 14-22 barista) = 42 shifts
    # - Plus 8 peak shift_lead shifts (Fri/Sat across all 3 branches = 6, plus Thu Maadi & Zamalek = 2) => 50 shifts!
    shifts: list[ShiftRequirementInput] = []
    shift_seq = 0
    for day_offset in range(7):
        # Egypt weekend: Friday (day_offset=4) and Saturday (day_offset=5).
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
            label="Cairo Multi-Branch Egypt Statutory",
            budget="3000.00",
            min_rest="11.00",
            max_daily="10.00",
            max_weekly="48.00",
            max_consec=6,
        ),
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

    # Double-check independently via verifier.
    ver = verify_or_raise(req, res1.assignments)
    assert ver.passed

    expected_slots = sum(s.required_count for s in req.shifts)
    assert len(res1.assignments) == expected_slots
    assert res1.metrics is not None
    assert res1.metrics.total_shifts_filled == expected_slots

    # Determinism check on the 25×50 benchmark: second solve yields byte-identical digest.
    res2 = solve_roster(req)
    assert res1.solver_receipt_digest == res2.solver_receipt_digest
    assert canonical_json(res1.to_dict()) == canonical_json(res2.to_dict())


# ---------------------------------------------------------------------------
# 5. Multi-Repair Slack-Variable UNSAT Diagnostics Tests
# ---------------------------------------------------------------------------


def test_multi_repair_unsat_simultaneous_coverage_shortfall_and_overtime_constraint() -> None:
    # Two simultaneous bottlenecks in one roster:
    # 1. Understaffed cashier shift `sh_sat_cashier` requires 2 cashiers, but ONLY 1 cashier exists anywhere!
    #    -> MUST emit `relax_coverage` (-1 cashier on `sh_sat_cashier`).
    # 2. Overtime-constrained barista team: 2 barista shifts (Fri AM 8h + Sat PM 8h), only 1 home barista
    #    capped at 8h regular / 0h OT, no transfer barista available!
    #    -> MUST emit `approve_overtime` (+8h for `emp_barista`).
    shifts = (
        ShiftRequirementInput("sh_sat_cashier", "branch_maadi", "cashier", _utc(2026, 11, 28, 6), _utc(2026, 11, 28, 14), 2, is_weekend=True),
        ShiftRequirementInput("sh_fri_barista", "branch_maadi", "barista", _utc(2026, 11, 27, 6), _utc(2026, 11, 27, 14), 1, is_weekend=True),
        ShiftRequirementInput("sh_sat_barista", "branch_maadi", "barista", _utc(2026, 11, 28, 14), _utc(2026, 11, 28, 22), 1, is_weekend=True),
    )
    employees = (
        EmployeeInput(
            employee_id="emp_cashier_1",
            display_label="Mona (Cashier)",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("40.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
        EmployeeInput(
            employee_id="emp_barista_1",
            display_label="Youssef (Barista)",
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
    assert res.assignments == ()
    assert res.unsat_summary is not None

    kinds = {r.kind for r in res.repair_suggestions}
    assert kinds == {"relax_coverage", "approve_overtime"}

    cov_repair = next(r for r in res.repair_suggestions if r.kind == "relax_coverage")
    assert cov_repair.target_shift_id == "sh_sat_cashier"
    assert cov_repair.coverage_shortfall == 1
    assert cov_repair.requires_approval is True

    ot_repair = next(r for r in res.repair_suggestions if r.kind == "approve_overtime")
    assert ot_repair.employee_id == "emp_barista_1"
    assert ot_repair.delta_minutes == 480
    assert ot_repair.delta_hours == Decimal("8.00")
    assert ot_repair.cost_delta == Decimal("1000.00")
    assert ot_repair.cost_delta_minor == 100_000
    assert ot_repair.requires_approval is True

    # Deterministic repeat check on UNSAT diagnostics.
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
            display_label="Nadia (Maadi)",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("8.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("100.00"),
        ),
        EmployeeInput(
            employee_id="emp_b",
            display_label="Karim (Maadi)",
            home_unit_id="branch_maadi",
            skills=("cashier",),
            max_regular_weekly_hours=Decimal("8.00"),
            max_overtime_weekly_hours=Decimal("0.00"),
            overtime_cost_per_hour=Decimal("110.00"),
        ),
        EmployeeInput(
            employee_id="emp_c",
            display_label="Omar (Zamalek)",
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
    # Joint minimal repair picks zero-cost cross-branch transfer of Omar.
    assert len(res.repair_suggestions) == 1
    transfer = res.repair_suggestions[0]
    assert transfer.kind == "allow_transfer"
    assert transfer.transfer_mode == "cross_branch"
    assert transfer.employee_id == "emp_c"
    assert transfer.from_unit_id == "branch_zamalek"
    assert transfer.to_unit_id == "branch_maadi"
    assert transfer.cost_delta == Decimal("0.00")

    # Single-axis alternatives also include overtime and coverage options.
    all_kinds = {r.kind for r in res.minimal_repairs}
    assert all_kinds == {"allow_transfer", "approve_overtime", "relax_coverage"}


def test_unsat_cross_skill_transfer_and_leave_protection() -> None:
    # 1. Cross-skill repair when employee declares cross_trainable_skills.
    shifts = (
        ShiftRequirementInput("sh_cash", "branch_maadi", "cashier", _utc(2026, 11, 28, 6), _utc(2026, 11, 28, 14), 1),
    )
    emp_cross = (
        EmployeeInput(
            employee_id="emp_bar",
            display_label="Hana (Barista)",
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

    # 2. Statutory leave protection: if sole cashier is on approved leave, repair MUST ONLY suggest `relax_coverage`,
    #    NEVER violating approved leave!
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
# 6. What-If Profile Comparison (Fairness-First vs Cost-First)
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
    cost_first = _cairo_profile(profile_id="cost_first", label="Cost First", budget="0.00", weekend_fairness="low")
    fairness_first = _cairo_profile(profile_id="fairness_first", label="Fairness First", budget="1000.00", weekend_fairness="high")

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
# 7. Live HTTP Worker Tests (/health, /manifest, /v1/functions/.../invoke)
# ---------------------------------------------------------------------------


def _http_request(
    base_url: str,
    method: str,
    path: str,
    body: bytes | None = None,
) -> tuple[int, dict[str, Any]]:
    req = urllib.request.Request(
        f"{base_url}{path}",
        data=body,
        method=method,
        headers={"Content-Type": "application/json"} if body is not None else {},
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            return int(resp.status), json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as err:
        return int(err.code), json.loads(err.read().decode("utf-8"))


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

        # 2. GET /manifest
        status, manifest = _http_request(base_url, "GET", "/manifest")
        assert status == 200
        assert manifest["abi"] == "norn.function-abi/1"
        assert manifest["functionId"] == FUNCTION_ID
        assert manifest["contractId"] == CONTRACT_ID
        assert manifest["semver"] == FUNCTION_VERSION
        assert manifest["manifestDigest"].startswith("sha256:")
        assert manifest["endpoints"]["invoke"] == f"/v1/functions/{FUNCTION_ID}/invoke"
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
        assert result["backend"] == "ortools_cpsat"
        assert result["invocation_id"] == "inv_001"
        assert result["solver_receipt_digest"].startswith("sha256:")
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

        # 6. POST /invoke — UNSAT multi-repair response
        unsat_payload = _valid_raw_payload()
        unsat_payload["shifts"][0]["required_count"] = 2
        status, unsat_res = _http_request(base_url, "POST", invoke_path, json.dumps(unsat_payload).encode("utf-8"))
        assert status == 200
        assert unsat_res["status"] == "infeasible"
        assert len(unsat_res["repair_suggestions"]) == 1
        assert unsat_res["repair_suggestions"][0]["kind"] == "relax_coverage"
        assert unsat_res["repair_suggestions"][0]["coverage_shortfall"] == 1
        assert unsat_res["solver_receipt_digest"].startswith("sha256:")

        # 7. POST /invoke — what-if comparison over HTTP
        what_if_envelope = _valid_raw_payload()
        what_if_envelope["alt_profile"] = {
            **what_if_envelope["profile"],
            "profile_id": "prof_2",
            "label": "Low Fairness",
            "weekend_fairness_priority": "low",
        }
        status, wi_res = _http_request(base_url, "POST", invoke_path, canonical_json(what_if_envelope).encode("utf-8"))
        assert status == 200
        assert wi_res["what_if"]["base_profile_id"] == "prof_1"
        assert wi_res["what_if"]["alt_profile_id"] == "prof_2"

        # 8. Unknown route returns 404
        status, not_found = _http_request(base_url, "GET", "/nonexistent")
        assert status == 404
        assert not_found["code"] == "NORN-SOLVER-NOT-FOUND"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def test_split_shift_avoidance_soft_objective() -> None:
    # Two non-contiguous 4h shifts on the same day (06-10 and 14-18, 4h gap) with min_rest=2h.
    # Two employees available: with avoid_split_shifts=True, solver assigns each employee 1 shift (0 split shifts).
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
