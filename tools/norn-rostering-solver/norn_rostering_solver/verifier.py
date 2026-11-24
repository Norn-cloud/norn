"""Independent hard-law post-verifier for `norn.solver.rostering@0.1.0`.

This module NEVER imports or trusts OR-Tools CP-SAT. Every candidate schedule emitted
by the solver is independently verified against all statutory and HR hard laws using
exact integer arithmetic and timezone-aware local calendar day / ISO week splitting;
any violation fails closed.

Diagnostic code alignment with `registry/packs/frappe-hr/ORACLE-CHECKLIST.md`:
- `HR-SH-01`: Non-overlapping shifts & no duplicate assignment on the same shift
- `HR-SH-02`: Approved leave exclusion
- `HR-SH-03`: Minimum rest gap between consecutive shifts
- `HR-SH-04`: Infeasibility repair discipline (emitted by `solver.py`, never relaxing statutory laws)
- `HR-SH-COV`: Exact shift headcount coverage & metadata integrity
- `HR-SH-ELIG`: Role/skill and branch/unit eligibility
- `HR-SH-STAT`: Statutory daily hours, statutory weekly hours, and max consecutive working days
- `HR-SH-BUDGET`: Weekly overtime budget cap
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass
from datetime import UTC, date, datetime, timedelta
from zoneinfo import ZoneInfo

from .boundary import PostVerificationError, format_utc, timedelta_exact_minutes
from .models import (
    Assignment,
    EmployeeInput,
    ShiftRequirementInput,
    SolveRequest,
    VerificationSummary,
)

ALL_HARD_LAW_CHECKS: tuple[str, ...] = (
    "HR-SH-01:non_overlapping_intervals",
    "HR-SH-02:approved_leave_exclusion",
    "HR-SH-03:minimum_rest_gap",
    "HR-SH-COV:exact_shift_coverage",
    "HR-SH-ELIG:role_and_unit_eligibility",
    "HR-SH-STAT:max_daily_hours",
    "HR-SH-STAT:max_weekly_hours",
    "HR-SH-STAT:max_consecutive_days",
    "HR-SH-BUDGET:overtime_budget_cap",
)

STATUTORY_CHECKS: tuple[str, ...] = (
    "HR-SH-01:non_overlapping_intervals",
    "HR-SH-02:approved_leave_exclusion",
    "HR-SH-03:minimum_rest_gap",
    "HR-SH-STAT:max_daily_hours",
    "HR-SH-STAT:max_weekly_hours",
    "HR-SH-STAT:max_consecutive_days",
)


@dataclass(frozen=True, slots=True)
class HardLawViolation:
    """One concrete hard-law violation detected by the independent verifier."""

    code: str
    detail: str

    def format(self) -> str:
        return f"{self.code}: {self.detail}"


@dataclass(frozen=True, slots=True)
class VerificationResult:
    """Result of independent hard-law verification."""

    passed: bool
    checks_run: tuple[str, ...]
    violations: tuple[HardLawViolation, ...]

    def to_summary(self) -> VerificationSummary:
        return VerificationSummary(
            checked=True,
            passed=self.passed,
            checks_run=self.checks_run,
            violation_count=len(self.violations),
        )


_EPOCH_UTC = datetime(1970, 1, 1, tzinfo=UTC)


def _count_shift_local_minutes_direct(
    starts_at: datetime,
    ends_at: datetime,
    tz: ZoneInfo,
) -> tuple[dict[date, int], dict[tuple[int, int], int]]:
    """Independently attribute every single UTC minute `[t, t+60s)` directly to its local calendar day & ISO week.

    Intentionally does NOT share `models.split_interval_by_local_day` so the independent
    post-verifier can never share a calendar/DST splitting bug with the solver.
    """
    start_min = timedelta_exact_minutes(starts_at.astimezone(UTC) - _EPOCH_UTC, "verifier_start")
    end_min = timedelta_exact_minutes(ends_at.astimezone(UTC) - _EPOCH_UTC, "verifier_end")
    daily: dict[date, int] = defaultdict(int)
    for epoch_min in range(start_min, end_min):
        local_day = (_EPOCH_UTC + timedelta(minutes=epoch_min)).astimezone(tz).date()
        daily[local_day] += 1
    weekly: dict[tuple[int, int], int] = defaultdict(int)
    for work_day, mins in daily.items():
        iso = work_day.isocalendar()
        weekly[(iso.year, iso.week)] += mins
    return dict(daily), dict(weekly)


def _max_consecutive_working_days(days: set[date]) -> int:
    if not days:
        return 0
    ordered = sorted(days)
    best = 1
    current = 1
    for i in range(1, len(ordered)):
        if ordered[i] == ordered[i - 1] + timedelta(days=1):
            current += 1
            if current > best:
                best = current
        else:
            current = 1
    return best


def verify_roster(
    request: SolveRequest,
    assignments: tuple[Assignment, ...] | list[Assignment],
    *,
    enforce_coverage: bool = True,
    enforce_eligibility: bool = True,
    enforce_employee_weekly_cap: bool = True,
    enforce_statutory_weekly_cap: bool = True,
    enforce_budget: bool = True,
) -> VerificationResult:
    """Independently check hard laws over `(request, assignments)` using exact local day/week splitting."""
    violations: list[HardLawViolation] = []
    shifts_by_id: dict[str, ShiftRequirementInput] = {s.shift_id: s for s in request.shifts}
    emps_by_id: dict[str, EmployeeInput] = {e.employee_id: e for e in request.employees}
    profile = request.profile
    tz = request.tz
    authorized_transfers = frozenset(request.authorized_transfer_employee_ids)

    assigned_per_shift: dict[str, list[str]] = defaultdict(list)
    shifts_per_emp: dict[str, list[ShiftRequirementInput]] = defaultdict(list)

    # 1. Structural integrity + eligibility + approved leave exclusion (HR-SH-02)
    for a in assignments:
        shift = shifts_by_id.get(a.shift_id)
        if shift is None:
            violations.append(
                HardLawViolation("HR-SH-COV:unknown_shift", f"assignment references unknown shift {a.shift_id!r}")
            )
            continue
        emp = emps_by_id.get(a.employee_id)
        if emp is None:
            violations.append(
                HardLawViolation(
                    "HR-SH-ELIG:unknown_employee",
                    f"assignment references unknown employee {a.employee_id!r}",
                )
            )
            continue

        # Metadata consistency against ShiftRequirement
        if (
            a.unit_id != shift.unit_id
            or a.role_code != shift.role_code
            or a.starts_at != format_utc(shift.starts_at)
            or a.ends_at != format_utc(shift.ends_at)
            or a.duration_minutes != shift.duration_minutes
        ):
            violations.append(
                HardLawViolation(
                    "HR-SH-COV:metadata_mismatch",
                    f"assignment ({a.shift_id}, {a.employee_id}) metadata disagrees with ShiftRequirement",
                )
            )

        assigned_per_shift[a.shift_id].append(a.employee_id)
        shifts_per_emp[a.employee_id].append(shift)

        if enforce_eligibility:
            if shift.role_code not in emp.skills:
                violations.append(
                    HardLawViolation(
                        "HR-SH-ELIG:role_ineligible",
                        f"employee {emp.employee_id!r} lacks required role {shift.role_code!r} for shift {shift.shift_id!r}",
                    )
                )
            if shift.unit_id != emp.home_unit_id:
                transfer_ok = (
                    (request.allow_cross_branch_transfers or emp.employee_id in authorized_transfers)
                    and shift.unit_id in emp.eligible_unit_ids
                )
                if not transfer_ok:
                    violations.append(
                        HardLawViolation(
                            "HR-SH-ELIG:unit_ineligible",
                            f"employee {emp.employee_id!r} (home {emp.home_unit_id!r}) is not authorized for unit {shift.unit_id!r} on shift {shift.shift_id!r}",
                        )
                    )

        # HR-SH-02: Approved leave exclusion (statutory — ALWAYS enforced)
        for leave_win in emp.approved_leave:
            if shift.window.overlaps(leave_win):
                violations.append(
                    HardLawViolation(
                        "HR-SH-02:approved_leave_overlap",
                        f"employee {emp.employee_id!r} assigned to shift {shift.shift_id!r} overlapping approved leave "
                        f"[{format_utc(leave_win.starts_at)}, {format_utc(leave_win.ends_at)})",
                    )
                )

    # 2. Duplicate assignment on same shift (HR-SH-01) & exact shift coverage (HR-SH-COV)
    for shift in sorted(request.shifts, key=lambda s: s.shift_id):
        assigned = assigned_per_shift.get(shift.shift_id, [])
        if len(assigned) != len(set(assigned)):
            violations.append(
                HardLawViolation(
                    "HR-SH-01:duplicate_assignment",
                    f"shift {shift.shift_id!r} assigns the same employee more than once",
                )
            )
        if enforce_coverage and len(assigned) != shift.required_count:
            violations.append(
                HardLawViolation(
                    "HR-SH-COV:coverage_mismatch",
                    f"shift {shift.shift_id!r} requires {shift.required_count} staff, got {len(assigned)}",
                )
            )

    # 3. Per-employee temporal laws (HR-SH-01 overlap, HR-SH-03 min rest, HR-SH-STAT daily/weekly/streak)
    weekly_ot_sub_minor: dict[tuple[int, int], int] = defaultdict(int)

    for emp_id in sorted(emps_by_id.keys()):
        emp = emps_by_id[emp_id]
        emp_shifts = sorted(shifts_per_emp.get(emp_id, []), key=lambda s: (s.starts_at, s.ends_at, s.shift_id))

        # Pairwise overlap (HR-SH-01) & minimum rest gap (HR-SH-03)
        for i in range(len(emp_shifts)):
            for j in range(i + 1, len(emp_shifts)):
                s1 = emp_shifts[i]
                s2 = emp_shifts[j]
                if s1.window.overlaps(s2.window):
                    violations.append(
                        HardLawViolation(
                            "HR-SH-01:overlapping_shifts",
                            f"employee {emp_id!r} has overlapping shifts {s1.shift_id!r} and {s2.shift_id!r}",
                        )
                    )
                else:
                    gap = s1.window.gap_minutes(s2.window)
                    if gap < profile.min_rest_minutes:
                        violations.append(
                            HardLawViolation(
                                "HR-SH-03:min_rest_violated",
                                f"employee {emp_id!r} rest gap between {s1.shift_id!r} and {s2.shift_id!r} is "
                                f"{gap} min (< {profile.min_rest_minutes} min required)",
                            )
                        )

        # Exact independent minute-by-minute UTC attribution across local calendar days & ISO weeks
        daily_minutes: dict[date, int] = defaultdict(int)
        weekly_minutes: dict[tuple[int, int], int] = defaultdict(int)
        for sh in emp_shifts:
            sh_daily, sh_weekly = _count_shift_local_minutes_direct(sh.starts_at, sh.ends_at, tz)
            for work_day, day_mins in sh_daily.items():
                daily_minutes[work_day] += day_mins
            for week_key, week_mins in sh_weekly.items():
                weekly_minutes[week_key] += week_mins

        for work_day, mins in sorted(daily_minutes.items()):
            if mins > profile.max_daily_minutes:
                violations.append(
                    HardLawViolation(
                        "HR-SH-STAT:max_daily_hours_exceeded",
                        f"employee {emp_id!r} works {mins} min on {work_day.isoformat()} ({request.iana_timezone}) "
                        f"(> {profile.max_daily_minutes} min daily cap)",
                    )
                )

        worked_days = {d for d, mins in daily_minutes.items() if mins > 0}
        max_streak = _max_consecutive_working_days(worked_days)
        if max_streak > profile.max_consecutive_days:
            violations.append(
                HardLawViolation(
                    "HR-SH-STAT:max_consecutive_days_exceeded",
                    f"employee {emp_id!r} works {max_streak} consecutive local days "
                    f"(> {profile.max_consecutive_days} day cap)",
                )
            )

        for week_key, mins in sorted(weekly_minutes.items()):
            if enforce_statutory_weekly_cap and mins > profile.max_weekly_minutes:
                violations.append(
                    HardLawViolation(
                        "HR-SH-STAT:max_weekly_hours_exceeded",
                        f"employee {emp_id!r} works {mins} min in ISO week {week_key[0]}-W{week_key[1]:02d} "
                        f"(> {profile.max_weekly_minutes} min statutory weekly cap)",
                    )
                )
            elif enforce_employee_weekly_cap and mins > emp.max_total_weekly_minutes:
                violations.append(
                    HardLawViolation(
                        "HR-SH-STAT:max_weekly_hours_exceeded",
                        f"employee {emp_id!r} works {mins} min in ISO week {week_key[0]}-W{week_key[1]:02d} "
                        f"(> {emp.max_total_weekly_minutes} min employee weekly cap)",
                    )
                )
            ot_min = max(0, mins - emp.max_regular_weekly_minutes)
            weekly_ot_sub_minor[week_key] += ot_min * emp.overtime_cost_minor_per_hour

    # 4. Exact weekly overtime budget cap (1/60 minor units — zero division, zero float)
    if enforce_budget:
        budget_sub_minor = profile.weekly_overtime_budget_minor * 60
        for week_key, total_sub_minor in sorted(weekly_ot_sub_minor.items()):
            if total_sub_minor > budget_sub_minor:
                violations.append(
                    HardLawViolation(
                        "HR-SH-BUDGET:overtime_budget_exceeded",
                        f"ISO week {week_key[0]}-W{week_key[1]:02d} overtime cost ({total_sub_minor} sub-minor units) "
                        f"exceeds weekly budget ({budget_sub_minor} sub-minor units)",
                    )
                )

    full_mode = (
        enforce_coverage
        and enforce_eligibility
        and enforce_employee_weekly_cap
        and enforce_statutory_weekly_cap
        and enforce_budget
    )
    checks = ALL_HARD_LAW_CHECKS if full_mode else STATUTORY_CHECKS
    return VerificationResult(
        passed=len(violations) == 0,
        checks_run=checks,
        violations=tuple(violations),
    )


def verify_or_raise(
    request: SolveRequest,
    assignments: tuple[Assignment, ...] | list[Assignment],
) -> VerificationResult:
    """Verify every hard law on a candidate schedule; fail closed with `PostVerificationError` on any violation."""
    result = verify_roster(request, assignments)
    if not result.passed:
        raise PostVerificationError(tuple(v.format() for v in result.violations))
    return result


def verify_statutory_laws(
    request: SolveRequest,
    assignments: tuple[Assignment, ...] | list[Assignment],
) -> VerificationResult:
    """Verify statutory hard laws (non-overlap, approved leave, min rest, daily/weekly statutory caps, consecutive days)."""
    return verify_roster(
        request,
        assignments,
        enforce_coverage=False,
        enforce_eligibility=False,
        enforce_employee_weekly_cap=False,
        enforce_statutory_weekly_cap=True,
        enforce_budget=False,
    )
