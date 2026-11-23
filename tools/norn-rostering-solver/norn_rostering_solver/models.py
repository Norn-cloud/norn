"""Strict, PII-free, exact-integer wire models for `norn.solver.rostering@0.1.0`.

All time quantities are represented internally as exact integer minutes, and all
currency quantities are represented as exact integer minor units (1/100 of the
currency unit, with scale-2 `Decimal` wire views). Zero IEEE `float` values exist
in any business calculation or serialized output.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import date, datetime
from decimal import Decimal
from typing import Any, Literal

from .boundary import (
    SolverContractError,
    decimal_to_minor_units,
    format_utc,
    hours_to_minutes,
    minor_units_to_decimal,
    minutes_to_hours,
    parse_bool,
    parse_decimal,
    parse_int,
    parse_str,
    parse_str_list,
    parse_utc_timestamp,
    timedelta_exact_minutes,
    timedelta_exact_seconds,
    validate_preference_profile_payload,
    validate_solve_request_payload,
)

FUNCTION_ID = "norn.solver.rostering"
FUNCTION_VERSION = "0.1.0"
CONTRACT_ID = f"{FUNCTION_ID}@{FUNCTION_VERSION}"

Priority = Literal["low", "medium", "high"]
SolveStatus = Literal["optimal", "feasible", "infeasible"]
SolverBackend = Literal["ortools_cpsat"]
RepairKind = Literal["relax_coverage", "approve_overtime", "allow_transfer"]
TransferMode = Literal["cross_branch", "cross_skill"]


def _parse_priority(raw: Any, path: str) -> Priority:
    if raw == "low":
        return "low"
    if raw == "medium":
        return "medium"
    if raw == "high":
        return "high"
    raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} must be 'low', 'medium', or 'high', got {raw!r}")


@dataclass(frozen=True, slots=True)
class TimeWindow:
    """Half-open UTC interval `[starts_at, ends_at)` with exact integer minute duration."""

    starts_at: datetime
    ends_at: datetime

    def __post_init__(self) -> None:
        if self.starts_at.tzinfo is None or self.ends_at.tzinfo is None:
            raise SolverContractError("NORN-SOLVER-INTERVAL", "interval timestamps must be timezone-aware UTC")
        if self.ends_at <= self.starts_at:
            raise SolverContractError(
                "NORN-SOLVER-INTERVAL",
                f"interval ends_at ({format_utc(self.ends_at)}) must be strictly after starts_at ({format_utc(self.starts_at)})",
            )
        # Validate exact whole-minute boundary and zero microseconds.
        _ = timedelta_exact_minutes(self.ends_at - self.starts_at, "TimeWindow")

    @property
    def duration_seconds(self) -> int:
        return timedelta_exact_seconds(self.ends_at - self.starts_at)

    @property
    def duration_minutes(self) -> int:
        return timedelta_exact_minutes(self.ends_at - self.starts_at, "TimeWindow")

    @property
    def duration_hours(self) -> Decimal:
        return minutes_to_hours(self.duration_minutes)

    def overlaps(self, other: TimeWindow) -> bool:
        """Half-open overlap test: `[a, b)` overlaps `[c, d)` iff `a < d and c < b`."""
        return self.starts_at < other.ends_at and other.starts_at < self.ends_at

    def gap_minutes(self, other: TimeWindow) -> int:
        """Exact non-negative integer minutes between two non-overlapping intervals (0 if overlapping/touching)."""
        if self.overlaps(other):
            return 0
        if self.ends_at <= other.starts_at:
            return timedelta_exact_minutes(other.starts_at - self.ends_at, "rest_gap")
        return timedelta_exact_minutes(self.starts_at - other.ends_at, "rest_gap")

    def to_dict(self) -> dict[str, str]:
        return {"ends_at": format_utc(self.ends_at), "starts_at": format_utc(self.starts_at)}


@dataclass(frozen=True, slots=True)
class EmployeeInput:
    """Unmarked operational employee slice for rostering (never PII or base salary)."""

    employee_id: str
    home_unit_id: str
    skills: tuple[str, ...]
    max_regular_weekly_hours: Decimal
    max_overtime_weekly_hours: Decimal
    overtime_cost_per_hour: Decimal
    eligible_unit_ids: tuple[str, ...] = ()
    cross_trainable_skills: tuple[str, ...] = ()
    approved_leave: tuple[TimeWindow, ...] = ()
    preferred_shift_ids: tuple[str, ...] = ()
    avoid_shift_ids: tuple[str, ...] = ()
    prior_weekend_shifts: int = 0
    display_label: str = ""

    def __post_init__(self) -> None:
        if not self.employee_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "employee_id must be non-empty")
        if not self.home_unit_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", f"employee {self.employee_id!r} home_unit_id must be non-empty")
        if not self.display_label:
            object.__setattr__(self, "display_label", self.employee_id)
        object.__setattr__(self, "skills", tuple(sorted(set(self.skills))))
        object.__setattr__(
            self,
            "cross_trainable_skills",
            tuple(sorted(set(self.cross_trainable_skills) - set(self.skills))),
        )
        if self.home_unit_id not in self.eligible_unit_ids:
            object.__setattr__(
                self,
                "eligible_unit_ids",
                tuple(sorted({self.home_unit_id, *self.eligible_unit_ids})),
            )
        else:
            object.__setattr__(self, "eligible_unit_ids", tuple(sorted(set(self.eligible_unit_ids))))
        # Validate exact integer conversions eagerly.
        _ = self.max_regular_weekly_minutes
        _ = self.max_overtime_weekly_minutes
        _ = self.overtime_cost_minor_per_hour

    @property
    def max_regular_weekly_minutes(self) -> int:
        return hours_to_minutes(self.max_regular_weekly_hours, f"employee[{self.employee_id}].max_regular_weekly_hours")

    @property
    def max_overtime_weekly_minutes(self) -> int:
        return hours_to_minutes(self.max_overtime_weekly_hours, f"employee[{self.employee_id}].max_overtime_weekly_hours")

    @property
    def max_total_weekly_minutes(self) -> int:
        return self.max_regular_weekly_minutes + self.max_overtime_weekly_minutes

    @property
    def overtime_cost_minor_per_hour(self) -> int:
        return decimal_to_minor_units(
            self.overtime_cost_per_hour,
            f"employee[{self.employee_id}].overtime_cost_per_hour",
        )

    def to_dict(self) -> dict[str, object]:
        return {
            "approved_leave": [w.to_dict() for w in sorted(self.approved_leave, key=lambda x: (x.starts_at, x.ends_at))],
            "avoid_shift_ids": sorted(self.avoid_shift_ids),
            "cross_trainable_skills": sorted(self.cross_trainable_skills),
            "display_label": self.display_label,
            "eligible_unit_ids": sorted(self.eligible_unit_ids),
            "employee_id": self.employee_id,
            "home_unit_id": self.home_unit_id,
            "max_overtime_weekly_hours": format(self.max_overtime_weekly_hours.quantize(Decimal("0.01")), "f"),
            "max_overtime_weekly_minutes": self.max_overtime_weekly_minutes,
            "max_regular_weekly_hours": format(self.max_regular_weekly_hours.quantize(Decimal("0.01")), "f"),
            "max_regular_weekly_minutes": self.max_regular_weekly_minutes,
            "overtime_cost_minor_per_hour": self.overtime_cost_minor_per_hour,
            "overtime_cost_per_hour": format(self.overtime_cost_per_hour.quantize(Decimal("0.01")), "f"),
            "preferred_shift_ids": sorted(self.preferred_shift_ids),
            "prior_weekend_shifts": self.prior_weekend_shifts,
            "skills": sorted(self.skills),
        }


@dataclass(frozen=True, slots=True)
class ShiftRequirementInput:
    """Shift coverage requirement (matches `hr.ShiftRequirement`)."""

    shift_id: str
    unit_id: str
    role_code: str
    starts_at: datetime
    ends_at: datetime
    required_count: int
    is_weekend: bool = False

    def __post_init__(self) -> None:
        if not self.shift_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "shift_id must be non-empty")
        if not self.unit_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", f"shift {self.shift_id!r} unit_id must be non-empty")
        if not self.role_code:
            raise SolverContractError("NORN-SOLVER-SCHEMA", f"shift {self.shift_id!r} role_code must be non-empty")
        if self.ends_at <= self.starts_at:
            raise SolverContractError("NORN-SOLVER-INTERVAL", f"shift {self.shift_id!r} ends_at must be > starts_at")
        if self.required_count < 1:
            raise SolverContractError("NORN-SOLVER-SCHEMA", f"shift {self.shift_id!r} required_count must be >= 1")
        _ = self.duration_minutes

    @property
    def window(self) -> TimeWindow:
        return TimeWindow(self.starts_at, self.ends_at)

    @property
    def duration_minutes(self) -> int:
        return timedelta_exact_minutes(self.ends_at - self.starts_at, f"shift[{self.shift_id}]")

    @property
    def duration_hours(self) -> Decimal:
        return minutes_to_hours(self.duration_minutes)

    @property
    def work_date(self) -> date:
        return self.starts_at.date()

    @property
    def iso_week(self) -> tuple[int, int]:
        iso = self.starts_at.isocalendar()
        return (iso.year, iso.week)

    def to_dict(self) -> dict[str, object]:
        return {
            "duration_minutes": self.duration_minutes,
            "ends_at": format_utc(self.ends_at),
            "is_weekend": self.is_weekend,
            "required_count": self.required_count,
            "role_code": self.role_code,
            "shift_id": self.shift_id,
            "starts_at": format_utc(self.starts_at),
            "unit_id": self.unit_id,
        }


@dataclass(frozen=True, slots=True)
class PreferenceProfileInput:
    """Governed business-terms trade-offs (matches `hr.PreferenceProfile` + statutory floors)."""

    profile_id: str
    label: str
    currency: str
    weekly_overtime_budget: Decimal
    min_rest_hours: Decimal
    max_daily_hours: Decimal
    max_consecutive_days: int
    max_weekly_hours: Decimal = Decimal("48.00")
    weekend_fairness_priority: Priority = "high"
    staff_preference_priority: Priority = "medium"
    avoid_split_shifts: bool = True

    def __post_init__(self) -> None:
        if not self.profile_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "profile_id must be non-empty")
        if self.max_consecutive_days < 1:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "max_consecutive_days must be >= 1")
        if self.max_weekly_hours <= Decimal("0"):
            raise SolverContractError("NORN-SOLVER-SCHEMA", "max_weekly_hours must be > 0")
        if self.max_daily_hours <= Decimal("0"):
            raise SolverContractError("NORN-SOLVER-SCHEMA", "max_daily_hours must be > 0")
        _ = self.min_rest_minutes
        _ = self.max_daily_minutes
        _ = self.max_weekly_minutes
        _ = self.weekly_overtime_budget_minor

    @property
    def min_rest_minutes(self) -> int:
        return hours_to_minutes(self.min_rest_hours, "profile.min_rest_hours")

    @property
    def max_daily_minutes(self) -> int:
        return hours_to_minutes(self.max_daily_hours, "profile.max_daily_hours")

    @property
    def max_weekly_minutes(self) -> int:
        return hours_to_minutes(self.max_weekly_hours, "profile.max_weekly_hours")

    @property
    def weekly_overtime_budget_minor(self) -> int:
        return decimal_to_minor_units(self.weekly_overtime_budget, "profile.weekly_overtime_budget")

    def to_dict(self) -> dict[str, object]:
        return {
            "avoid_split_shifts": self.avoid_split_shifts,
            "currency": self.currency,
            "label": self.label,
            "max_consecutive_days": self.max_consecutive_days,
            "max_daily_hours": format(self.max_daily_hours.quantize(Decimal("0.01")), "f"),
            "max_daily_minutes": self.max_daily_minutes,
            "max_weekly_hours": format(self.max_weekly_hours.quantize(Decimal("0.01")), "f"),
            "max_weekly_minutes": self.max_weekly_minutes,
            "min_rest_hours": format(self.min_rest_hours.quantize(Decimal("0.01")), "f"),
            "min_rest_minutes": self.min_rest_minutes,
            "profile_id": self.profile_id,
            "staff_preference_priority": self.staff_preference_priority,
            "weekend_fairness_priority": self.weekend_fairness_priority,
            "weekly_overtime_budget": format(self.weekly_overtime_budget.quantize(Decimal("0.01")), "f"),
            "weekly_overtime_budget_minor": self.weekly_overtime_budget_minor,
        }

    @classmethod
    def from_dict(cls, raw_profile: Any, path: str = "$.profile") -> PreferenceProfileInput:
        return _preference_profile_from_dict(raw_profile, path)


@dataclass(frozen=True, slots=True)
class SolveRequest:
    """Canonical rostering request payload."""

    roster_id: str
    unit_id: str
    employees: tuple[EmployeeInput, ...]
    shifts: tuple[ShiftRequirementInput, ...]
    profile: PreferenceProfileInput
    allow_cross_branch_transfers: bool = False
    authorized_transfer_employee_ids: tuple[str, ...] = ()

    def __post_init__(self) -> None:
        if not self.roster_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "roster_id must be non-empty")
        emp_ids = [e.employee_id for e in self.employees]
        if len(emp_ids) != len(set(emp_ids)):
            raise SolverContractError("NORN-SOLVER-SCHEMA", "duplicate employee_id in request.employees")
        shift_ids = [s.shift_id for s in self.shifts]
        if len(shift_ids) != len(set(shift_ids)):
            raise SolverContractError("NORN-SOLVER-SCHEMA", "duplicate shift_id in request.shifts")
        object.__setattr__(
            self,
            "authorized_transfer_employee_ids",
            tuple(sorted(set(self.authorized_transfer_employee_ids))),
        )

    def to_dict(self) -> dict[str, object]:
        return {
            "allow_cross_branch_transfers": self.allow_cross_branch_transfers,
            "authorized_transfer_employee_ids": list(self.authorized_transfer_employee_ids),
            "contract": CONTRACT_ID,
            "employees": [e.to_dict() for e in sorted(self.employees, key=lambda x: x.employee_id)],
            "profile": self.profile.to_dict(),
            "roster_id": self.roster_id,
            "shifts": [
                s.to_dict()
                for s in sorted(self.shifts, key=lambda x: (x.starts_at, x.ends_at, x.unit_id, x.role_code, x.shift_id))
            ],
            "unit_id": self.unit_id,
        }

    @classmethod
    def from_dict(cls, raw: Any, path: str = "$") -> SolveRequest:
        validate_solve_request_payload(raw, path)
        try:
            roster_id = parse_str(raw["roster_id"], f"{path}.roster_id")
            unit_id = parse_str(raw["unit_id"], f"{path}.unit_id")
            raw_profile = raw["profile"]
            raw_employees = raw["employees"]
            raw_shifts = raw["shifts"]
        except KeyError as exc:
            raise SolverContractError("NORN-SOLVER-SCHEMA", f"missing required field {path}.{exc.args[0]}") from exc

        profile = PreferenceProfileInput.from_dict(raw_profile, f"{path}.profile")

        employees: list[EmployeeInput] = []
        for idx, emp in enumerate(raw_employees):
            p = f"{path}.employees[{idx}]"
            try:
                employee_id = parse_str(emp["employee_id"], f"{p}.employee_id")
                home_unit_id = parse_str(emp["home_unit_id"], f"{p}.home_unit_id")
                skills = parse_str_list(emp["skills"], f"{p}.skills")
                max_reg = _parse_hours_or_minutes(
                    emp,
                    hours_key="max_regular_weekly_hours",
                    minutes_key="max_regular_weekly_minutes",
                    path=p,
                    default_hours=None,
                )
            except KeyError as exc:
                raise SolverContractError("NORN-SOLVER-SCHEMA", f"missing required field {p}.{exc.args[0]}") from exc

            raw_leave = emp.get("approved_leave", [])
            leave_windows_list: list[TimeWindow] = []
            for j, w in enumerate(raw_leave):
                lp = f"{p}.approved_leave[{j}]"
                try:
                    leave_start = parse_utc_timestamp(w["starts_at"], f"{lp}.starts_at")
                    leave_end = parse_utc_timestamp(w["ends_at"], f"{lp}.ends_at")
                except KeyError as exc:
                    raise SolverContractError(
                        "NORN-SOLVER-SCHEMA",
                        f"missing required field {lp}.{exc.args[0]}",
                    ) from exc
                leave_windows_list.append(TimeWindow(starts_at=leave_start, ends_at=leave_end))

            max_ot = _parse_hours_or_minutes(
                emp,
                hours_key="max_overtime_weekly_hours",
                minutes_key="max_overtime_weekly_minutes",
                path=p,
                default_hours=Decimal("0.00"),
            )
            ot_cost = _parse_money_or_minor(
                emp,
                money_key="overtime_cost_per_hour",
                minor_key="overtime_cost_minor_per_hour",
                path=p,
                default_money=Decimal("0.00"),
            )
            display_label = parse_str(emp["display_label"], f"{p}.display_label") if "display_label" in emp else employee_id
            eligible_unit_ids = (
                parse_str_list(emp["eligible_unit_ids"], f"{p}.eligible_unit_ids")
                if "eligible_unit_ids" in emp
                else (home_unit_id,)
            )
            employees.append(
                EmployeeInput(
                    employee_id=employee_id,
                    display_label=display_label,
                    home_unit_id=home_unit_id,
                    eligible_unit_ids=eligible_unit_ids,
                    skills=skills,
                    cross_trainable_skills=parse_str_list(
                        emp.get("cross_trainable_skills", []),
                        f"{p}.cross_trainable_skills",
                    ),
                    max_regular_weekly_hours=max_reg,
                    max_overtime_weekly_hours=max_ot,
                    overtime_cost_per_hour=ot_cost,
                    approved_leave=tuple(leave_windows_list),
                    preferred_shift_ids=parse_str_list(
                        emp.get("preferred_shift_ids", []),
                        f"{p}.preferred_shift_ids",
                    ),
                    avoid_shift_ids=parse_str_list(
                        emp.get("avoid_shift_ids", []),
                        f"{p}.avoid_shift_ids",
                    ),
                    prior_weekend_shifts=parse_int(emp.get("prior_weekend_shifts", 0), f"{p}.prior_weekend_shifts"),
                )
            )

        shifts: list[ShiftRequirementInput] = []
        for idx, sh in enumerate(raw_shifts):
            p = f"{path}.shifts[{idx}]"
            try:
                shift_id = parse_str(sh["shift_id"], f"{p}.shift_id")
                role_code = parse_str(sh["role_code"], f"{p}.role_code")
                starts_at = parse_utc_timestamp(sh["starts_at"], f"{p}.starts_at")
                ends_at = parse_utc_timestamp(sh["ends_at"], f"{p}.ends_at")
                required_count = parse_int(sh["required_count"], f"{p}.required_count", minimum=1)
            except KeyError as exc:
                raise SolverContractError("NORN-SOLVER-SCHEMA", f"missing required field {p}.{exc.args[0]}") from exc
            sh_unit_id = parse_str(sh["unit_id"], f"{p}.unit_id") if "unit_id" in sh else unit_id
            shifts.append(
                ShiftRequirementInput(
                    shift_id=shift_id,
                    unit_id=sh_unit_id,
                    role_code=role_code,
                    starts_at=starts_at,
                    ends_at=ends_at,
                    required_count=required_count,
                    is_weekend=parse_bool(sh.get("is_weekend", False), f"{p}.is_weekend"),
                )
            )

        return cls(
            roster_id=roster_id,
            unit_id=unit_id,
            employees=tuple(employees),
            shifts=tuple(shifts),
            profile=profile,
            allow_cross_branch_transfers=parse_bool(
                raw.get("allow_cross_branch_transfers", False),
                f"{path}.allow_cross_branch_transfers",
            ),
            authorized_transfer_employee_ids=parse_str_list(
                raw.get("authorized_transfer_employee_ids", []),
                f"{path}.authorized_transfer_employee_ids",
            ),
        )


def _parse_hours_or_minutes(
    raw: dict[str, Any],
    *,
    hours_key: str,
    minutes_key: str,
    path: str,
    default_hours: Decimal | None,
) -> Decimal:
    if minutes_key in raw:
        mins = parse_int(raw[minutes_key], f"{path}.{minutes_key}", minimum=0)
        return minutes_to_hours(mins)
    if hours_key in raw:
        return parse_decimal(raw[hours_key], f"{path}.{hours_key}")
    if default_hours is not None:
        return default_hours
    raise KeyError(hours_key)


def _parse_money_or_minor(
    raw: dict[str, Any],
    *,
    money_key: str,
    minor_key: str,
    path: str,
    default_money: Decimal | None,
) -> Decimal:
    if minor_key in raw:
        minor = parse_int(raw[minor_key], f"{path}.{minor_key}", minimum=0)
        return minor_units_to_decimal(minor)
    if money_key in raw:
        return parse_decimal(raw[money_key], f"{path}.{money_key}")
    if default_money is not None:
        return default_money
    raise KeyError(money_key)


def _preference_profile_from_dict(raw_profile: Any, path: str = "$.profile") -> PreferenceProfileInput:
    validate_preference_profile_payload(raw_profile, path)
    try:
        profile_id = parse_str(raw_profile["profile_id"], f"{path}.profile_id")
        currency = parse_str(raw_profile["currency"], f"{path}.currency")
        budget = _parse_money_or_minor(
            raw_profile,
            money_key="weekly_overtime_budget",
            minor_key="weekly_overtime_budget_minor",
            path=path,
            default_money=None,
        )
        min_rest = _parse_hours_or_minutes(
            raw_profile,
            hours_key="min_rest_hours",
            minutes_key="min_rest_minutes",
            path=path,
            default_hours=None,
        )
        max_consec = parse_int(raw_profile["max_consecutive_days"], f"{path}.max_consecutive_days", minimum=1)
    except KeyError as exc:
        raise SolverContractError("NORN-SOLVER-SCHEMA", f"missing required field {path}.{exc.args[0]}") from exc

    max_daily = _parse_hours_or_minutes(
        raw_profile,
        hours_key="max_daily_hours",
        minutes_key="max_daily_minutes",
        path=path,
        default_hours=Decimal("10.00"),
    )
    max_weekly = _parse_hours_or_minutes(
        raw_profile,
        hours_key="max_weekly_hours",
        minutes_key="max_weekly_minutes",
        path=path,
        default_hours=Decimal("48.00"),
    )
    label = parse_str(raw_profile["label"], f"{path}.label") if "label" in raw_profile else profile_id
    return PreferenceProfileInput(
        profile_id=profile_id,
        label=label,
        currency=currency,
        weekly_overtime_budget=budget,
        min_rest_hours=min_rest,
        max_daily_hours=max_daily,
        max_weekly_hours=max_weekly,
        max_consecutive_days=max_consec,
        weekend_fairness_priority=_parse_priority(
            raw_profile.get("weekend_fairness_priority", "high"),
            f"{path}.weekend_fairness_priority",
        ),
        staff_preference_priority=_parse_priority(
            raw_profile.get("staff_preference_priority", "medium"),
            f"{path}.staff_preference_priority",
        ),
        avoid_split_shifts=parse_bool(raw_profile.get("avoid_split_shifts", True), f"{path}.avoid_split_shifts"),
    )


@dataclass(frozen=True, slots=True)
class Assignment:
    """One scheduled shift commitment emitted by the solver."""

    shift_id: str
    employee_id: str
    unit_id: str
    role_code: str
    starts_at: str
    ends_at: str
    duration_minutes: int
    hours: Decimal
    is_cross_branch: bool = False
    is_cross_skill: bool = False

    def to_dict(self) -> dict[str, object]:
        return {
            "duration_minutes": self.duration_minutes,
            "employee_id": self.employee_id,
            "ends_at": self.ends_at,
            "hours": format(self.hours.quantize(Decimal("0.01")), "f"),
            "is_cross_branch": self.is_cross_branch,
            "is_cross_skill": self.is_cross_skill,
            "role_code": self.role_code,
            "shift_id": self.shift_id,
            "starts_at": self.starts_at,
            "unit_id": self.unit_id,
        }


@dataclass(frozen=True, slots=True)
class RosterMetrics:
    """Exact integer/Decimal operational metrics for a verified schedule."""

    total_shifts_filled: int
    total_regular_minutes: int
    total_overtime_minutes: int
    total_regular_hours: Decimal
    total_overtime_hours: Decimal
    total_overtime_cost_minor: int
    total_overtime_cost: Decimal
    max_weekend_spread: int
    preference_hits: int
    avoid_violations: int
    split_shift_count: int
    objective_score: int

    def to_dict(self) -> dict[str, object]:
        return {
            "avoid_violations": self.avoid_violations,
            "max_weekend_spread": self.max_weekend_spread,
            "objective_score": self.objective_score,
            "preference_hits": self.preference_hits,
            "split_shift_count": self.split_shift_count,
            "total_overtime_cost": format(self.total_overtime_cost.quantize(Decimal("0.01")), "f"),
            "total_overtime_cost_minor": self.total_overtime_cost_minor,
            "total_overtime_hours": format(self.total_overtime_hours.quantize(Decimal("0.01")), "f"),
            "total_overtime_minutes": self.total_overtime_minutes,
            "total_regular_hours": format(self.total_regular_hours.quantize(Decimal("0.01")), "f"),
            "total_regular_minutes": self.total_regular_minutes,
            "total_shifts_filled": self.total_shifts_filled,
        }


@dataclass(frozen=True, slots=True)
class RepairSuggestion:
    """Typed, minimal, concrete UNSAT repair emitted by the minimum-slack CP-SAT repair model."""

    kind: RepairKind
    target_shift_id: str | None
    employee_id: str | None
    from_unit_id: str | None
    to_unit_id: str | None
    role_code: str | None
    transfer_mode: TransferMode | None
    coverage_shortfall: int | None
    delta_minutes: int | None
    delta_hours: Decimal | None
    cost_delta_minor: int
    cost_delta: Decimal
    requires_approval: bool
    hint: str
    diagnostic_code: str

    def to_dict(self) -> dict[str, object]:
        return {
            "cost_delta": format(self.cost_delta.quantize(Decimal("0.01")), "f"),
            "cost_delta_minor": self.cost_delta_minor,
            "coverage_shortfall": self.coverage_shortfall,
            "delta_hours": format(self.delta_hours.quantize(Decimal("0.01")), "f") if self.delta_hours is not None else None,
            "delta_minutes": self.delta_minutes,
            "diagnostic_code": self.diagnostic_code,
            "employee_id": self.employee_id,
            "from_unit_id": self.from_unit_id,
            "hint": self.hint,
            "kind": self.kind,
            "requires_approval": self.requires_approval,
            "role_code": self.role_code,
            "target_shift_id": self.target_shift_id,
            "to_unit_id": self.to_unit_id,
            "transfer_mode": self.transfer_mode,
        }


# Backwards-compatible alias for v0.1 callers.
MinimalRepair = RepairSuggestion


@dataclass(frozen=True, slots=True)
class VerificationSummary:
    """Independent post-verifier report attached to every solver receipt."""

    checked: bool
    passed: bool
    checks_run: tuple[str, ...]
    violation_count: int

    def to_dict(self) -> dict[str, object]:
        return {
            "checked": self.checked,
            "checks_run": list(self.checks_run),
            "passed": self.passed,
            "violation_count": self.violation_count,
        }


@dataclass(frozen=True, slots=True)
class SolveResponse:
    """Canonical deterministic solver response and receipt."""

    contract: str
    roster_id: str
    profile_id: str
    status: SolveStatus
    backend: SolverBackend
    assignments: tuple[Assignment, ...]
    metrics: RosterMetrics | None
    repair_suggestions: tuple[RepairSuggestion, ...]
    alternative_repairs: tuple[RepairSuggestion, ...]
    unsat_summary: str | None
    verification: VerificationSummary
    input_digest: str
    solver_receipt_digest: str

    @property
    def receipt_digest(self) -> str:
        """Backwards-compatible alias for `solver_receipt_digest`."""
        return self.solver_receipt_digest

    @property
    def minimal_repairs(self) -> tuple[RepairSuggestion, ...]:
        """Combined deduplicated primary + alternative repairs for callers inspecting all options."""
        seen: set[tuple[object, ...]] = set()
        out: list[RepairSuggestion] = []
        for r in (*self.repair_suggestions, *self.alternative_repairs):
            key = (r.kind, r.target_shift_id, r.employee_id, r.coverage_shortfall, r.delta_minutes)
            if key not in seen:
                seen.add(key)
                out.append(r)
        return tuple(out)

    def perform_summary(self) -> dict[str, object]:
        """Output summary matching `SolveRosterFn` in `registry/packs/hr/rostering.ts`."""
        return {
            "assignmentCount": len(self.assignments),
            "solverDigest": self.solver_receipt_digest,
            "status": self.status,
        }

    def to_dict(self) -> dict[str, object]:
        return {
            "alternative_repairs": [r.to_dict() for r in self.alternative_repairs],
            "assignment_count": len(self.assignments),
            "assignments": [a.to_dict() for a in self.assignments],
            "backend": self.backend,
            "contract": self.contract,
            "function_id": FUNCTION_ID,
            "input_digest": self.input_digest,
            "metrics": self.metrics.to_dict() if self.metrics is not None else None,
            "perform_summary": self.perform_summary(),
            "profile_id": self.profile_id,
            "repair_suggestions": [r.to_dict() for r in self.repair_suggestions],
            "roster_id": self.roster_id,
            "solver_receipt_digest": self.solver_receipt_digest,
            "status": self.status,
            "unsat_summary": self.unsat_summary,
            "verification": self.verification.to_dict(),
            "version": FUNCTION_VERSION,
        }


@dataclass(frozen=True, slots=True)
class AssignmentDiff:
    """Single shift whose assigned employee set changed between two what-if profiles."""

    shift_id: str
    unit_id: str
    role_code: str
    starts_at: str
    ends_at: str
    base_employee_ids: tuple[str, ...]
    alt_employee_ids: tuple[str, ...]

    def to_dict(self) -> dict[str, object]:
        return {
            "alt_employee_ids": list(self.alt_employee_ids),
            "base_employee_ids": list(self.base_employee_ids),
            "ends_at": self.ends_at,
            "role_code": self.role_code,
            "shift_id": self.shift_id,
            "starts_at": self.starts_at,
            "unit_id": self.unit_id,
        }


@dataclass(frozen=True, slots=True)
class WhatIfComparison:
    """Deterministic side-by-side comparison across two `PreferenceProfile` runs."""

    base_profile_id: str
    alt_profile_id: str
    base_status: SolveStatus
    alt_status: SolveStatus
    base_receipt_digest: str
    alt_receipt_digest: str
    changed_assignments: tuple[AssignmentDiff, ...]
    changed_assignment_count: int
    overtime_minutes_delta: int | None
    overtime_hours_delta: Decimal | None
    overtime_cost_minor_delta: int | None
    overtime_cost_delta: Decimal | None
    max_weekend_spread_delta: int | None
    preference_hits_delta: int | None
    objective_score_delta: int | None
    summary: str

    def to_dict(self) -> dict[str, object]:
        return {
            "alt_profile_id": self.alt_profile_id,
            "alt_receipt_digest": self.alt_receipt_digest,
            "alt_status": self.alt_status,
            "base_profile_id": self.base_profile_id,
            "base_receipt_digest": self.base_receipt_digest,
            "base_status": self.base_status,
            "changed_assignment_count": self.changed_assignment_count,
            "changed_assignments": [c.to_dict() for c in self.changed_assignments],
            "max_weekend_spread_delta": self.max_weekend_spread_delta,
            "objective_score_delta": self.objective_score_delta,
            "overtime_cost_delta": (
                format(self.overtime_cost_delta.quantize(Decimal("0.01")), "f")
                if self.overtime_cost_delta is not None
                else None
            ),
            "overtime_cost_minor_delta": self.overtime_cost_minor_delta,
            "overtime_hours_delta": (
                format(self.overtime_hours_delta.quantize(Decimal("0.01")), "f")
                if self.overtime_hours_delta is not None
                else None
            ),
            "overtime_minutes_delta": self.overtime_minutes_delta,
            "preference_hits_delta": self.preference_hits_delta,
            "summary": self.summary,
        }
