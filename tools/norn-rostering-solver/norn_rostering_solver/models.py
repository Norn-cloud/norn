"""Strict, PII-free, exact-integer wire models for `norn.solver.rostering@0.1.0`.

All time quantities are stored and processed internally as exact integer minutes
(never lossily round-tripped through 2-decimal hours), and all currency quantities
are stored as exact integer minor units (1/100 of the currency unit). Zero IEEE
`float` values and zero free-text label fields exist in any solver input or
business calculation.
"""

from __future__ import annotations

from dataclasses import InitVar, dataclass
from datetime import UTC, date, datetime, timedelta
from decimal import Decimal
from typing import Any, Literal
from zoneinfo import ZoneInfo

from .boundary import (
    MAX_EMPLOYEES,
    MAX_HORIZON_DAYS,
    MAX_LEAVE_BLOCKS,
    MAX_SHIFTS,
    LimitExceededError,
    SolverContractError,
    format_utc,
    minor_units_to_decimal,
    minutes_to_hours,
    parse_bool,
    parse_hours_to_minutes,
    parse_iana_timezone,
    parse_int,
    parse_money_to_minor_units,
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
SolveStatus = Literal["optimal", "feasible", "infeasible", "unknown"]
EvidenceLabel = Literal["exhaustive-finite", "bounded", "unknown"]
SolverBackend = Literal["ortools_cpsat"]
RepairKind = Literal["relax_coverage", "approve_overtime", "allow_transfer"]
TransferMode = Literal["cross_branch", "cross_skill"]
RepairOptimality = Literal["minimum_slack", "feasible_slack"]


def _parse_priority(raw: Any, path: str) -> Priority:
    if raw == "low":
        return "low"
    if raw == "medium":
        return "medium"
    if raw == "high":
        return "high"
    raise SolverContractError("NORN-SOLVER-SCHEMA", f"{path} must be 'low', 'medium', or 'high', got {raw!r}")


def _next_local_midnight_utc(cur_utc: datetime, tz: ZoneInfo) -> datetime:
    """Return the exact UTC instant when the local calendar date in `tz` advances past `cur_utc`'s date."""
    cur_local = cur_utc.astimezone(tz)
    local_day = cur_local.date()
    next_day = local_day + timedelta(days=1)

    for fold in (0, 1):
        candidate = datetime(next_day.year, next_day.month, next_day.day, 0, 0, tzinfo=tz, fold=fold).astimezone(UTC)
        if candidate > cur_utc and candidate.astimezone(tz).date() >= next_day:
            return candidate

    # Defensive fallback for theoretical pathological offset jumps: advance hour-by-hour until date changes.
    probe = cur_utc + timedelta(minutes=1)
    while probe.astimezone(tz).date() == local_day:
        probe += timedelta(minutes=1)
    return probe


def split_interval_by_local_day(starts_at: datetime, ends_at: datetime, tz: ZoneInfo) -> dict[date, int]:
    """Split a half-open UTC interval `[starts_at, ends_at)` into exact integer minutes per local calendar day.

    Preserves exact minute conservation across midnight and DST transitions:
    `sum(result.values()) == timedelta_exact_minutes(ends_at - starts_at)`.
    """
    cur_utc = starts_at.astimezone(UTC)
    end_utc = ends_at.astimezone(UTC)
    if end_utc <= cur_utc:
        raise SolverContractError("NORN-SOLVER-INTERVAL", "interval ends_at must be strictly after starts_at")

    by_day: dict[date, int] = {}
    while cur_utc < end_utc:
        local_day = cur_utc.astimezone(tz).date()
        next_mid_utc = _next_local_midnight_utc(cur_utc, tz)
        seg_end_utc = end_utc if end_utc < next_mid_utc else next_mid_utc
        seg_minutes = timedelta_exact_minutes(seg_end_utc - cur_utc, "local_day_split")
        if seg_minutes > 0:
            by_day[local_day] = by_day.get(local_day, 0) + seg_minutes
        cur_utc = seg_end_utc
    return by_day


def split_interval_by_iso_week(
    starts_at: datetime,
    ends_at: datetime,
    tz: ZoneInfo,
) -> dict[tuple[int, int], int]:
    """Split a half-open UTC interval `[starts_at, ends_at)` into exact integer minutes per local ISO week."""
    by_day = split_interval_by_local_day(starts_at, ends_at, tz)
    by_week: dict[tuple[int, int], int] = {}
    for day, mins in by_day.items():
        iso = day.isocalendar()
        wk = (iso.year, iso.week)
        by_week[wk] = by_week.get(wk, 0) + mins
    return by_week


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


def _resolve_exact_minutes(
    explicit_minutes: int,
    hours_init: Decimal | int | str | None,
    *,
    path: str,
    default_minutes: int | None,
) -> int:
    has_min = explicit_minutes >= 0
    has_hr = hours_init is not None
    if has_min and has_hr:
        from_hr = parse_hours_to_minutes(hours_init, path)
        if explicit_minutes != from_hr:
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path} conflicting minutes ({explicit_minutes}) and hours ({hours_init} -> {from_hr} min)",
            )
        return explicit_minutes
    if has_min:
        return parse_int(explicit_minutes, path, minimum=0)
    if has_hr:
        return parse_hours_to_minutes(hours_init, path)
    if default_minutes is not None:
        return default_minutes
    raise SolverContractError("NORN-SOLVER-SCHEMA", f"missing required time quantity for {path}")


def _resolve_exact_minor_units(
    explicit_minor: int,
    money_init: Decimal | int | str | None,
    *,
    path: str,
    default_minor: int | None,
) -> int:
    has_minor = explicit_minor >= 0
    has_money = money_init is not None
    if has_minor and has_money:
        from_money = parse_money_to_minor_units(money_init, path)
        if explicit_minor != from_money:
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path} conflicting minor units ({explicit_minor}) and money ({money_init} -> {from_money} minor)",
            )
        return explicit_minor
    if has_minor:
        return parse_int(explicit_minor, path, minimum=0)
    if has_money:
        return parse_money_to_minor_units(money_init, path)
    if default_minor is not None:
        return default_minor
    raise SolverContractError("NORN-SOLVER-SCHEMA", f"missing required monetary quantity for {path}")


@dataclass(frozen=True, slots=True)
class EmployeeInput:
    """Unmarked operational employee slice for rostering (opaque IDs only — never PII or free-text labels)."""

    employee_id: str
    home_unit_id: str
    skills: tuple[str, ...]
    max_regular_weekly_minutes: int = -1
    max_overtime_weekly_minutes: int = -1
    overtime_cost_minor_per_hour: int = -1
    max_regular_weekly_hours: InitVar[Decimal | int | str | None] = None
    max_overtime_weekly_hours: InitVar[Decimal | int | str | None] = None
    overtime_cost_per_hour: InitVar[Decimal | int | str | None] = None
    eligible_unit_ids: tuple[str, ...] = ()
    cross_trainable_skills: tuple[str, ...] = ()
    approved_leave: tuple[TimeWindow, ...] = ()
    preferred_shift_ids: tuple[str, ...] = ()
    avoid_shift_ids: tuple[str, ...] = ()
    prior_weekend_shifts: int = 0

    def __post_init__(
        self,
        max_regular_weekly_hours: Decimal | int | str | None,
        max_overtime_weekly_hours: Decimal | int | str | None,
        overtime_cost_per_hour: Decimal | int | str | None,
    ) -> None:
        if not self.employee_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "employee_id must be non-empty")
        if not self.home_unit_id:
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"employee {self.employee_id!r} home_unit_id must be non-empty",
            )
        reg_min = _resolve_exact_minutes(
            self.max_regular_weekly_minutes,
            max_regular_weekly_hours,
            path=f"employee[{self.employee_id}].max_regular_weekly",
            default_minutes=None,
        )
        ot_min = _resolve_exact_minutes(
            self.max_overtime_weekly_minutes,
            max_overtime_weekly_hours,
            path=f"employee[{self.employee_id}].max_overtime_weekly",
            default_minutes=0,
        )
        ot_cost_minor = _resolve_exact_minor_units(
            self.overtime_cost_minor_per_hour,
            overtime_cost_per_hour,
            path=f"employee[{self.employee_id}].overtime_cost_per_hour",
            default_minor=0,
        )
        object.__setattr__(self, "max_regular_weekly_minutes", reg_min)
        object.__setattr__(self, "max_overtime_weekly_minutes", ot_min)
        object.__setattr__(self, "overtime_cost_minor_per_hour", ot_cost_minor)
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
        if self.prior_weekend_shifts < 0:
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"employee {self.employee_id!r} prior_weekend_shifts must be >= 0",
            )

    @property
    def max_total_weekly_minutes(self) -> int:
        return self.max_regular_weekly_minutes + self.max_overtime_weekly_minutes

    @property
    def overtime_cost_decimal(self) -> Decimal:
        return minor_units_to_decimal(self.overtime_cost_minor_per_hour)

    def to_dict(self) -> dict[str, object]:
        return {
            "approved_leave": [w.to_dict() for w in sorted(self.approved_leave, key=lambda x: (x.starts_at, x.ends_at))],
            "avoid_shift_ids": sorted(self.avoid_shift_ids),
            "cross_trainable_skills": sorted(self.cross_trainable_skills),
            "eligible_unit_ids": sorted(self.eligible_unit_ids),
            "employee_id": self.employee_id,
            "home_unit_id": self.home_unit_id,
            "max_overtime_weekly_minutes": self.max_overtime_weekly_minutes,
            "max_regular_weekly_minutes": self.max_regular_weekly_minutes,
            "overtime_cost_minor_per_hour": self.overtime_cost_minor_per_hour,
            "overtime_cost_per_hour": format(self.overtime_cost_decimal, "f"),
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
        if self.starts_at.tzinfo is None or self.ends_at.tzinfo is None:
            raise SolverContractError("NORN-SOLVER-INTERVAL", f"shift {self.shift_id!r} timestamps must be timezone-aware")
        if self.ends_at <= self.starts_at:
            raise SolverContractError("NORN-SOLVER-INTERVAL", f"shift {self.shift_id!r} ends_at must be > starts_at")
        if self.required_count < 1:
            raise SolverContractError("NORN-SOLVER-SCHEMA", f"shift {self.shift_id!r} required_count must be >= 1")
        if (self.ends_at - self.starts_at).days > MAX_HORIZON_DAYS:
            raise LimitExceededError(
                f"shift {self.shift_id!r} spans more than MAX_HORIZON_DAYS ({MAX_HORIZON_DAYS})"
            )
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

    def local_start_date(self, tz: ZoneInfo) -> date:
        return self.starts_at.astimezone(tz).date()

    def daily_minutes(self, tz: ZoneInfo) -> dict[date, int]:
        """Exact integer minutes of this shift falling on each local calendar day in `tz`."""
        return split_interval_by_local_day(self.starts_at, self.ends_at, tz)

    def weekly_minutes(self, tz: ZoneInfo) -> dict[tuple[int, int], int]:
        """Exact integer minutes of this shift falling in each local ISO week `(year, week)` in `tz`."""
        return split_interval_by_iso_week(self.starts_at, self.ends_at, tz)

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
    """Governed business-terms trade-offs and statutory floors (opaque profile_id, zero free-text labels)."""

    profile_id: str
    currency: str
    max_consecutive_days: int
    weekly_overtime_budget_minor: int = -1
    min_rest_minutes: int = -1
    max_daily_minutes: int = -1
    max_weekly_minutes: int = -1
    weekly_overtime_budget: InitVar[Decimal | int | str | None] = None
    min_rest_hours: InitVar[Decimal | int | str | None] = None
    max_daily_hours: InitVar[Decimal | int | str | None] = None
    max_weekly_hours: InitVar[Decimal | int | str | None] = None
    weekend_fairness_priority: Priority = "high"
    staff_preference_priority: Priority = "medium"
    avoid_split_shifts: bool = True

    def __post_init__(
        self,
        weekly_overtime_budget: Decimal | int | str | None,
        min_rest_hours: Decimal | int | str | None,
        max_daily_hours: Decimal | int | str | None,
        max_weekly_hours: Decimal | int | str | None,
    ) -> None:
        if not self.profile_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "profile_id must be non-empty")
        if not self.currency:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "currency must be non-empty")
        if self.max_consecutive_days < 1:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "max_consecutive_days must be >= 1")

        budget_minor = _resolve_exact_minor_units(
            self.weekly_overtime_budget_minor,
            weekly_overtime_budget,
            path="profile.weekly_overtime_budget",
            default_minor=None,
        )
        rest_min = _resolve_exact_minutes(
            self.min_rest_minutes,
            min_rest_hours,
            path="profile.min_rest",
            default_minutes=None,
        )
        daily_min = _resolve_exact_minutes(
            self.max_daily_minutes,
            max_daily_hours,
            path="profile.max_daily",
            default_minutes=600,
        )
        weekly_min = _resolve_exact_minutes(
            self.max_weekly_minutes,
            max_weekly_hours,
            path="profile.max_weekly",
            default_minutes=2_880,
        )
        if daily_min <= 0:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "max_daily_minutes must be > 0")
        if weekly_min <= 0:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "max_weekly_minutes must be > 0")

        object.__setattr__(self, "weekly_overtime_budget_minor", budget_minor)
        object.__setattr__(self, "min_rest_minutes", rest_min)
        object.__setattr__(self, "max_daily_minutes", daily_min)
        object.__setattr__(self, "max_weekly_minutes", weekly_min)

    @property
    def weekly_overtime_budget_decimal(self) -> Decimal:
        return minor_units_to_decimal(self.weekly_overtime_budget_minor)

    def to_dict(self) -> dict[str, object]:
        return {
            "avoid_split_shifts": self.avoid_split_shifts,
            "currency": self.currency,
            "max_consecutive_days": self.max_consecutive_days,
            "max_daily_minutes": self.max_daily_minutes,
            "max_weekly_minutes": self.max_weekly_minutes,
            "min_rest_minutes": self.min_rest_minutes,
            "profile_id": self.profile_id,
            "staff_preference_priority": self.staff_preference_priority,
            "weekend_fairness_priority": self.weekend_fairness_priority,
            "weekly_overtime_budget": format(self.weekly_overtime_budget_decimal, "f"),
            "weekly_overtime_budget_minor": self.weekly_overtime_budget_minor,
        }

    @classmethod
    def from_dict(cls, raw_profile: Any, path: str = "$.profile") -> PreferenceProfileInput:
        return _preference_profile_from_dict(raw_profile, path)


@dataclass(frozen=True, slots=True)
class SolveRequest:
    """Canonical unmarked rostering slice request payload."""

    roster_id: str
    unit_id: str
    employees: tuple[EmployeeInput, ...]
    shifts: tuple[ShiftRequirementInput, ...]
    profile: PreferenceProfileInput
    allow_cross_branch_transfers: bool = False
    authorized_transfer_employee_ids: tuple[str, ...] = ()
    iana_timezone: str = "UTC"

    def __post_init__(self) -> None:
        if not self.roster_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "roster_id must be non-empty")
        if not self.unit_id:
            raise SolverContractError("NORN-SOLVER-SCHEMA", "unit_id must be non-empty")
        validated_tz = parse_iana_timezone(self.iana_timezone, "$.iana_timezone")
        object.__setattr__(self, "iana_timezone", validated_tz)

        if len(self.employees) > MAX_EMPLOYEES:
            raise LimitExceededError(
                f"employees count ({len(self.employees)}) exceeds MAX_EMPLOYEES ({MAX_EMPLOYEES})"
            )
        if len(self.shifts) > MAX_SHIFTS:
            raise LimitExceededError(
                f"shifts count ({len(self.shifts)}) exceeds MAX_SHIFTS ({MAX_SHIFTS})"
            )
        total_leave = sum(len(e.approved_leave) for e in self.employees)
        if total_leave > MAX_LEAVE_BLOCKS:
            raise LimitExceededError(
                f"total approved_leave blocks ({total_leave}) exceeds MAX_LEAVE_BLOCKS ({MAX_LEAVE_BLOCKS})"
            )

        emp_ids = [e.employee_id for e in self.employees]
        if len(emp_ids) != len(set(emp_ids)):
            raise SolverContractError("NORN-SOLVER-SCHEMA", "duplicate employee_id in request.employees")
        shift_ids = [s.shift_id for s in self.shifts]
        if len(shift_ids) != len(set(shift_ids)):
            raise SolverContractError("NORN-SOLVER-SCHEMA", "duplicate shift_id in request.shifts")

        if self.shifts:
            tz = ZoneInfo(validated_tz)
            min_day: date | None = None
            max_day: date | None = None
            for sh in self.shifts:
                days = sh.daily_minutes(tz).keys()
                d_lo = min(days)
                d_hi = max(days)
                if min_day is None or d_lo < min_day:
                    min_day = d_lo
                if max_day is None or d_hi > max_day:
                    max_day = d_hi
            if min_day is not None and max_day is not None:
                span_days = (max_day - min_day).days + 1
                if span_days > MAX_HORIZON_DAYS:
                    raise LimitExceededError(
                        f"roster horizon spans {span_days} local calendar days ({min_day.isoformat()}..{max_day.isoformat()}), "
                        f"exceeding MAX_HORIZON_DAYS ({MAX_HORIZON_DAYS})"
                    )

        object.__setattr__(
            self,
            "authorized_transfer_employee_ids",
            tuple(sorted(set(self.authorized_transfer_employee_ids))),
        )

    @property
    def tz(self) -> ZoneInfo:
        return ZoneInfo(self.iana_timezone)

    def to_dict(self) -> dict[str, object]:
        return {
            "allow_cross_branch_transfers": self.allow_cross_branch_transfers,
            "authorized_transfer_employee_ids": list(self.authorized_transfer_employee_ids),
            "contract": CONTRACT_ID,
            "employees": [e.to_dict() for e in sorted(self.employees, key=lambda x: x.employee_id)],
            "iana_timezone": self.iana_timezone,
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

        iana_timezone = (
            parse_iana_timezone(raw["iana_timezone"], f"{path}.iana_timezone")
            if "iana_timezone" in raw
            else "UTC"
        )
        profile = PreferenceProfileInput.from_dict(raw_profile, f"{path}.profile")

        employees: list[EmployeeInput] = []
        for idx, emp in enumerate(raw_employees):
            p = f"{path}.employees[{idx}]"
            try:
                employee_id = parse_str(emp["employee_id"], f"{p}.employee_id")
                home_unit_id = parse_str(emp["home_unit_id"], f"{p}.home_unit_id")
                skills = parse_str_list(emp["skills"], f"{p}.skills")
                max_reg_min = _parse_exact_minutes_field(
                    emp,
                    hours_key="max_regular_weekly_hours",
                    minutes_key="max_regular_weekly_minutes",
                    path=p,
                    default_minutes=None,
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

            max_ot_min = _parse_exact_minutes_field(
                emp,
                hours_key="max_overtime_weekly_hours",
                minutes_key="max_overtime_weekly_minutes",
                path=p,
                default_minutes=0,
            )
            ot_cost_minor = _parse_exact_money_minor_field(
                emp,
                money_key="overtime_cost_per_hour",
                minor_key="overtime_cost_minor_per_hour",
                path=p,
                default_minor=0,
            )
            eligible_unit_ids = (
                parse_str_list(emp["eligible_unit_ids"], f"{p}.eligible_unit_ids")
                if "eligible_unit_ids" in emp
                else (home_unit_id,)
            )
            employees.append(
                EmployeeInput(
                    employee_id=employee_id,
                    home_unit_id=home_unit_id,
                    eligible_unit_ids=eligible_unit_ids,
                    skills=skills,
                    cross_trainable_skills=parse_str_list(
                        emp.get("cross_trainable_skills", []),
                        f"{p}.cross_trainable_skills",
                    ),
                    max_regular_weekly_minutes=max_reg_min,
                    max_overtime_weekly_minutes=max_ot_min,
                    overtime_cost_minor_per_hour=ot_cost_minor,
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
            iana_timezone=iana_timezone,
        )


def _parse_exact_minutes_field(
    raw: dict[str, Any],
    *,
    hours_key: str,
    minutes_key: str,
    path: str,
    default_minutes: int | None,
) -> int:
    has_min = minutes_key in raw
    has_hr = hours_key in raw
    if has_min and has_hr:
        mins = parse_int(raw[minutes_key], f"{path}.{minutes_key}", minimum=0)
        from_hr = parse_hours_to_minutes(raw[hours_key], f"{path}.{hours_key}")
        if mins != from_hr:
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path} conflicting {minutes_key} ({mins}) and {hours_key} ({raw[hours_key]} -> {from_hr} min)",
            )
        return mins
    if has_min:
        return parse_int(raw[minutes_key], f"{path}.{minutes_key}", minimum=0)
    if has_hr:
        return parse_hours_to_minutes(raw[hours_key], f"{path}.{hours_key}")
    if default_minutes is not None:
        return default_minutes
    raise KeyError(hours_key)


def _parse_exact_money_minor_field(
    raw: dict[str, Any],
    *,
    money_key: str,
    minor_key: str,
    path: str,
    default_minor: int | None,
) -> int:
    has_minor = minor_key in raw
    has_money = money_key in raw
    if has_minor and has_money:
        minor = parse_int(raw[minor_key], f"{path}.{minor_key}", minimum=0)
        from_money = parse_money_to_minor_units(raw[money_key], f"{path}.{money_key}")
        if minor != from_money:
            raise SolverContractError(
                "NORN-SOLVER-SCHEMA",
                f"{path} conflicting {minor_key} ({minor}) and {money_key} ({raw[money_key]} -> {from_money} minor)",
            )
        return minor
    if has_minor:
        return parse_int(raw[minor_key], f"{path}.{minor_key}", minimum=0)
    if has_money:
        return parse_money_to_minor_units(raw[money_key], f"{path}.{money_key}")
    if default_minor is not None:
        return default_minor
    raise KeyError(money_key)


def _preference_profile_from_dict(raw_profile: Any, path: str = "$.profile") -> PreferenceProfileInput:
    validate_preference_profile_payload(raw_profile, path)
    try:
        profile_id = parse_str(raw_profile["profile_id"], f"{path}.profile_id")
        currency = parse_str(raw_profile["currency"], f"{path}.currency")
        budget_minor = _parse_exact_money_minor_field(
            raw_profile,
            money_key="weekly_overtime_budget",
            minor_key="weekly_overtime_budget_minor",
            path=path,
            default_minor=None,
        )
        min_rest_min = _parse_exact_minutes_field(
            raw_profile,
            hours_key="min_rest_hours",
            minutes_key="min_rest_minutes",
            path=path,
            default_minutes=None,
        )
        max_consec = parse_int(raw_profile["max_consecutive_days"], f"{path}.max_consecutive_days", minimum=1)
    except KeyError as exc:
        raise SolverContractError("NORN-SOLVER-SCHEMA", f"missing required field {path}.{exc.args[0]}") from exc

    max_daily_min = _parse_exact_minutes_field(
        raw_profile,
        hours_key="max_daily_hours",
        minutes_key="max_daily_minutes",
        path=path,
        default_minutes=600,
    )
    max_weekly_min = _parse_exact_minutes_field(
        raw_profile,
        hours_key="max_weekly_hours",
        minutes_key="max_weekly_minutes",
        path=path,
        default_minutes=2_880,
    )
    return PreferenceProfileInput(
        profile_id=profile_id,
        currency=currency,
        weekly_overtime_budget_minor=budget_minor,
        min_rest_minutes=min_rest_min,
        max_daily_minutes=max_daily_min,
        max_weekly_minutes=max_weekly_min,
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
    """Typed, concrete UNSAT repair emitted by the slack-variable CP-SAT repair model."""

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
    optimality: RepairOptimality = "minimum_slack"
    is_minimum_slack: bool = True

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
            "is_minimum_slack": self.is_minimum_slack,
            "kind": self.kind,
            "optimality": self.optimality,
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
    """Canonical deterministic solver response and full-payload receipt."""

    contract: str
    roster_id: str
    profile_id: str
    status: SolveStatus
    evidence_label: EvidenceLabel
    backend: SolverBackend
    assignments: tuple[Assignment, ...]
    metrics: RosterMetrics | None
    repair_optimality: RepairOptimality | None
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
            "evidence_label": self.evidence_label,
            "function_id": FUNCTION_ID,
            "input_digest": self.input_digest,
            "metrics": self.metrics.to_dict() if self.metrics is not None else None,
            "perform_summary": self.perform_summary(),
            "profile_id": self.profile_id,
            "repair_optimality": self.repair_optimality,
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
