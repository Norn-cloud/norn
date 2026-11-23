"""Independent hard-law post-verifier for `norn.solver.rostering@0.1.0`.

This module NEVER imports or trusts OR-Tools CP-SAT. Every candidate schedule emitted
by the solver is independently verified against all statutory and HR hard laws
(`HR-SH-01..04`) using exact integer arithmetic; any violation fails closed.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass
from datetime import date, timedelta

from .boundary import PostVerificationError, format_utc
from .models import (
    Assignment,
    EmployeeInput,
    ShiftRequirementInput,
    SolveRequest,
    VerificationSummary,
)

ALL_HARD_LAW_CHECKS: tuple[str, ...] = (
    "HR-SH-01:exact_shift_coverage",
    "HR-SH-02:role_and_unit_eligibility",
    "HR-SH-03:non_overlapping_intervals",
    "HR-SH-03:minimum_rest_gap",
    "HR-SH-04:max_daily_hours",
    "HR-SH-04:max_weekly_hours",
    "HR-SH-04:max_consecutive_days",
    "HR-SH-04:approved_leave_exclusion",
    "HR-SH-04:overtime_budget_cap",
)

STATUTORY_CHECKS: tuple[str, ...] = (
    "HR-SH-03:non_overlapping_intervals",
    "HR-SH-03:minimum_rest_gap",
    "HR-SH-04:max_daily_hours",
    "HR-SH-04:max_consecutive_days",
    "HR-SH-04:approved_leave_exclusion",
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
    enforce_weekly_cap: bool = True,
    enforce_budget: bool = True,
) -> VerificationResult:
    """Independently check all hard laws over `(request, assignments)`."""
    violations: list[HardLawViolation] = []
    shifts_by_id: dict[str, ShiftRequirementInput] = {s.shift_id: s for s in request.shifts}
    emps_by_id: dict[str, EmployeeInput] = {e.employee_id: e for e in request.employees}
    profile = request.profile
    authorized_transfers = frozenset(request.authorized_transfer_employee_ids)

    assigned_per_shift: dict[str, list[str]] = defaultdict(list)
    shifts_per_emp: dict[str, list[ShiftRequirementInput]] = defaultdict(list)

    # 1. Structural integrity + eligibility + approved leave exclusion
    for a in assignments:
        shift = shifts_by_id.get(a.shift_id)
        if shift is None:
            violations.append(
                HardLawViolation("HR-SH-01:unknown_shift", f"assignment references unknown shift {a.shift_id!r}")
            )
            continue
        emp = emps_by_id.get(a.employee_id)
        if emp is None:
            violations.append(
                HardLawViolation("HR-SH-02:unknown_employee", f"assignment references unknown employee {a.employee_id!r}")
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
                    "HR-SH-01:metadata_mismatch",
                    f"assignment ({a.shift_id}, {a.employee_id}) metadata disagrees with ShiftRequirement",
                )
            )

        assigned_per_shift[a.shift_id].append(a.employee_id)
        shifts_per_emp[a.employee_id].append(shift)

        if enforce_eligibility:
            if shift.role_code not in emp.skills:
                violations.append(
                    HardLawViolation(
                        "HR-SH-02:role_ineligible",
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
                            "HR-SH-02:unit_ineligible",
                            f"employee {emp.employee_id!r} (home {emp.home_unit_id!r}) is not authorized for unit {shift.unit_id!r} on shift {shift.shift_id!r}",
                        )
                    )

        # Approved leave exclusion (statutory — ALWAYS enforced)
        for leave_win in emp.approved_leave:
            if shift.window.overlaps(leave_win):
                violations.append(
                    HardLawViolation(
                        "HR-SH-04:approved_leave_overlap",
                        f"employee {emp.employee_id!r} assigned to shift {shift.shift_id!r} overlapping approved leave "
                        f"[{format_utc(leave_win.starts_at)}, {format_utc(leave_win.ends_at)})",
                    )
                )

    # 2. Exact shift coverage & duplicate assignment on same shift
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
                    "HR-SH-01:coverage_mismatch",
                    f"shift {shift.shift_id!r} requires {shift.required_count} staff, got {len(assigned)}",
                )
            )

    # 3. Per-employee temporal laws (overlap, min rest, daily hours, weekly hours, consecutive days)
    weekly_ot_sub_minor: dict[tuple[int, int], int] = defaultdict(int)

    for emp_id in sorted(emps_by_id.keys()):
        emp = emps_by_id[emp_id]
        emp_shifts = sorted(shifts_per_emp.get(emp_id, []), key=lambda s: (s.starts_at, s.ends_at, s.shift_id))

        # Overlap & minimum rest gap (pairwise over sorted shifts)
        for i in range(len(emp_shifts)):
            for j in range(i + 1, len(emp_shifts)):
                s1 = emp_shifts[i]
                s2 = emp_shifts[j]
                if s1.window.overlaps(s2.window):
                    violations.append(
                        HardLawViolation(
                            "HR-SH-03:overlapping_shifts",
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

        # Daily hours & consecutive working days
        daily_minutes: dict[date, int] = defaultdict(int)
        weekly_minutes: dict[tuple[int, int], int] = defaultdict(int)
        for sh in emp_shifts:
            daily_minutes[sh.work_date] += sh.duration_minutes
            weekly_minutes[sh.iso_week] += sh.duration_minutes

        for work_day, mins in sorted(daily_minutes.items()):
            if mins > profile.max_daily_minutes:
                violations.append(
                    HardLawViolation(
                        "HR-SH-04:max_daily_hours_exceeded",
                        f"employee {emp_id!r} works {mins} min on {work_day.isoformat()} "
                        f"(> {profile.max_daily_minutes} min daily cap)",
                    )
                )

        max_streak = _max_consecutive_working_days(set(daily_minutes.keys()))
        if max_streak > profile.max_consecutive_days:
            violations.append(
                HardLawViolation(
                    "HR-SH-04:max_consecutive_days_exceeded",
                    f"employee {emp_id!r} works {max_streak} consecutive days "
                    f"(> {profile.max_consecutive_days} day cap)",
                )
            )

        effective_weekly_cap = min(profile.max_weekly_minutes, emp.max_total_weekly_minutes)
        for week_key, mins in sorted(weekly_minutes.items()):
            if enforce_weekly_cap and mins > effective_weekly_cap:
                violations.append(
                    HardLawViolation(
                        "HR-SH-04:max_weekly_hours_exceeded",
                        f"employee {emp_id!r} works {mins} min in ISO week {week_key[0]}-W{week_key[1]:02d} "
                        f"(> {effective_weekly_cap} min weekly cap)",
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
                        "HR-SH-04:overtime_budget_exceeded",
                        f"ISO week {week_key[0]}-W{week_key[1]:02d} overtime cost ({total_sub_minor} sub-minor units) "
                        f"exceeds weekly budget ({budget_sub_minor} sub-minor units)",
                    )
                )

    checks = ALL_HARD_LAW_CHECKS if (enforce_coverage and enforce_eligibility and enforce_weekly_cap and enforce_budget) else STATUTORY_CHECKS
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
    """Verify statutory hard laws (non-overlap, min rest, daily hours, consecutive days, approved leave)."""
    return verify_roster(
        request,
        assignments,
        enforce_coverage=False,
        enforce_eligibility=False,
        enforce_weekly_cap=False,
        enforce_budget=False,
    )
