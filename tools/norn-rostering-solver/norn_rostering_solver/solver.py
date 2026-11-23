"""Deterministic Google OR-Tools CP-SAT rostering solver (`norn.solver.rostering@0.1.0`).

Architecture:
1. Primary CP-SAT model:
   - Single-threaded (`num_search_workers = 1`), pinned `random_seed = 0`, deterministic
     sorted ordering of all entities, variables, and constraints.
   - Strict integer arithmetic only (exact integer minutes and integer sub-minor currency
     units; zero IEEE `float`).
   - Enforces ALL hard laws (`HR-SH-01..04` + statutory baselines).
   - Optimizes governed `PreferenceProfile` business-terms trade-offs.
2. Independent hard-law post-verification (`verifier.verify_or_raise`):
   - Never trusts CP-SAT; independently checks every emitted assignment before returning
     a receipt, failing closed on any discrepancy.
3. Secondary minimum-slack CP-SAT UNSAT repair model:
   - When primary model is INFEASIBLE, solves a deterministic minimum-slack CP-SAT model
     with bounded slack variables for `relax_coverage`, `approve_overtime`, and
     `allow_transfer` (cross-branch and cross-skill), while keeping statutory laws
     (non-overlap, min rest, max daily hours, max consecutive days, approved leave)
     strictly enforced and independently post-verified.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import replace
from datetime import date, timedelta
from decimal import Decimal

from ortools.sat.python import cp_model

from .boundary import format_utc, minor_units_to_decimal, minutes_to_hours, sha256_digest
from .models import (
    CONTRACT_ID,
    Assignment,
    AssignmentDiff,
    EmployeeInput,
    PreferenceProfileInput,
    Priority,
    RepairSuggestion,
    RosterMetrics,
    ShiftRequirementInput,
    SolveRequest,
    SolveResponse,
    SolveStatus,
    TransferMode,
    VerificationSummary,
    WhatIfComparison,
)
from .verifier import verify_or_raise, verify_statutory_laws

# Exact integer minor-unit weights per business priority (100 minor units = 1 currency unit).
_WEEKEND_FAIRNESS_MINOR: dict[Priority, int] = {
    "low": 10_000,      # 100.00 currency units per weekend spread point
    "medium": 30_000,   # 300.00 currency units per weekend spread point
    "high": 60_000,     # 600.00 currency units per weekend spread point
}

_STAFF_PREF_MINOR: dict[Priority, int] = {
    "low": 2_500,       # 25.00 currency units per preference hit
    "medium": 10_000,   # 100.00 currency units per preference hit
    "high": 25_000,     # 250.00 currency units per preference hit
}

_SPLIT_SHIFT_PENALTY_MINOR = 20_000  # 200.00 currency units per split shift

# Sub-minor scale: 60 sub-minor units per 1 minor unit so `ot_minutes * rate_minor_per_hour`
# is 100% exact integer arithmetic without division or floats.
_SUB_MINOR_PER_MINOR = 60

# Deterministic repair objective hierarchy (exact integer weights, safely inside int64).
_REPAIR_W_COVERAGE_SHORTFALL = 10_000_000_000
_REPAIR_W_CROSS_SKILL = 1_000_000_000
_REPAIR_W_OVERTIME_EMP = 100_000_000
_REPAIR_W_CROSS_BRANCH = 50_000_000
_REPAIR_W_OVERTIME_MINUTE = 100_000
_REPAIR_W_BUDGET_SUB_MINOR = 10

# Maximum extra overtime overflow per employee/week explored in repair pass (24h = 1440 min;
# still strictly bounded by statutory daily hours, rest gaps, and max consecutive days).
_MAX_REPAIR_OVERTIME_OVERFLOW_MINUTES = 1_440

# Deterministic CP-SAT search budget (integer deterministic time units).
_PRIMARY_MAX_DETERMINISTIC_TIME = 30
_REPAIR_MAX_DETERMINISTIC_TIME = 15


def _new_deterministic_solver(max_deterministic_time: int) -> cp_model.CpSolver:
    solver = cp_model.CpSolver()
    solver.parameters.num_search_workers = 1
    solver.parameters.random_seed = 0
    solver.parameters.cp_model_presolve = True
    solver.parameters.max_deterministic_time = max_deterministic_time
    return solver


def _sorted_inputs(
    request: SolveRequest,
) -> tuple[list[EmployeeInput], list[ShiftRequirementInput]]:
    employees = sorted(request.employees, key=lambda e: e.employee_id)
    shifts = sorted(
        request.shifts,
        key=lambda s: (s.starts_at, s.ends_at, s.unit_id, s.role_code, s.shift_id),
    )
    return employees, shifts


def _overlaps_approved_leave(emp: EmployeeInput, shift: ShiftRequirementInput) -> bool:
    return any(shift.window.overlaps(leave_win) for leave_win in emp.approved_leave)


def _is_primary_eligible(
    emp: EmployeeInput,
    shift: ShiftRequirementInput,
    *,
    allow_cross_branch_transfers: bool,
    authorized_transfer_emp_ids: frozenset[str],
) -> bool:
    if shift.role_code not in emp.skills:
        return False
    if shift.unit_id != emp.home_unit_id:
        if not (allow_cross_branch_transfers or emp.employee_id in authorized_transfer_emp_ids):
            return False
        if shift.unit_id not in emp.eligible_unit_ids:
            return False
    return not _overlaps_approved_leave(emp, shift)


def _transfer_eligibility(
    emp: EmployeeInput,
    shift: ShiftRequirementInput,
) -> TransferMode | None:
    """Return transfer mode if `(emp, shift)` is eligible via transfer slack, else None."""
    if _overlaps_approved_leave(emp, shift):
        return None
    if shift.role_code in emp.skills:
        if shift.unit_id != emp.home_unit_id and shift.unit_id in emp.eligible_unit_ids:
            return "cross_branch"
        return None
    if shift.role_code in emp.cross_trainable_skills and shift.unit_id in emp.eligible_unit_ids:
        return "cross_skill"
    return None


def _add_statutory_temporal_constraints(
    model: cp_model.CpModel,
    employees: list[EmployeeInput],
    shifts: list[ShiftRequirementInput],
    x: dict[tuple[int, int], cp_model.IntVar],
    profile: PreferenceProfileInput,
) -> None:
    """Add non-negotiable statutory hard laws: non-overlap, min rest, max daily hours, max consecutive days."""
    if not shifts:
        return

    min_rest_min = profile.min_rest_minutes
    max_daily_min = profile.max_daily_minutes
    max_consec = profile.max_consecutive_days

    # Precompute pairwise shift conflicts (overlap OR gap < min_rest_min).
    conflict_pairs: list[tuple[int, int]] = []
    for i in range(len(shifts)):
        for j in range(i + 1, len(shifts)):
            s1 = shifts[i]
            s2 = shifts[j]
            if s1.window.overlaps(s2.window) or s1.window.gap_minutes(s2.window) < min_rest_min:
                conflict_pairs.append((i, j))

    all_dates = sorted({s.work_date for s in shifts})
    d_min = all_dates[0]
    d_max = all_dates[-1]
    span_days = (d_max - d_min).days + 1
    calendar_dates = [d_min + timedelta(days=offset) for offset in range(span_days)]

    shifts_by_date: dict[date, list[int]] = defaultdict(list)
    for s_idx, sh in enumerate(shifts):
        shifts_by_date[sh.work_date].append(s_idx)

    for e_idx in range(len(employees)):
        # 1. Pairwise non-overlap + minimum rest gap
        for s1_idx, s2_idx in conflict_pairs:
            v1 = x.get((e_idx, s1_idx))
            v2 = x.get((e_idx, s2_idx))
            if v1 is not None and v2 is not None:
                model.add(v1 + v2 <= 1)

        # 2. Maximum daily hours + daily working indicators
        worked_day: dict[date, cp_model.IntVar] = {}
        for day in calendar_dates:
            s_indices = shifts_by_date.get(day, [])
            day_terms: list[tuple[int, cp_model.IntVar]] = []
            for s_idx in s_indices:
                v = x.get((e_idx, s_idx))
                if v is not None:
                    day_terms.append((shifts[s_idx].duration_minutes, v))
            if not day_terms:
                continue
            model.add(sum(dur * var for dur, var in day_terms) <= max_daily_min)
            w_var = model.new_bool_var(f"worked_e{e_idx}_{day.isoformat()}")
            model.add_max_equality(w_var, [var for _, var in day_terms])
            worked_day[day] = w_var

        # 3. Maximum consecutive working days (sliding window of length max_consec + 1)
        window_len = max_consec + 1
        if len(calendar_dates) >= window_len:
            for start_i in range(len(calendar_dates) - window_len + 1):
                window_vars = [
                    worked_day[calendar_dates[start_i + k]]
                    for k in range(window_len)
                    if calendar_dates[start_i + k] in worked_day
                ]
                if len(window_vars) > max_consec:
                    model.add(sum(window_vars) <= max_consec)


def _compute_exact_metrics(
    employees: list[EmployeeInput],
    shifts: list[ShiftRequirementInput],
    assigned_pairs: list[tuple[int, int]],
    active_emp_indices: list[int],
    profile: PreferenceProfileInput,
) -> RosterMetrics:
    """Compute exact integer/Decimal business metrics directly from an assignment set."""
    by_emp: dict[int, list[ShiftRequirementInput]] = {i: [] for i in range(len(employees))}
    for e_idx, s_idx in assigned_pairs:
        by_emp[e_idx].append(shifts[s_idx])

    total_regular_min = 0
    total_overtime_min = 0
    total_ot_sub_minor = 0
    pref_hits = 0
    avoid_violations = 0
    split_shift_count = 0
    weekend_counts: list[int] = []

    active_set = set(active_emp_indices) if active_emp_indices else set(range(len(employees)))

    for e_idx, emp in enumerate(employees):
        emp_shifts = by_emp[e_idx]
        by_week: dict[tuple[int, int], int] = defaultdict(int)
        by_day: dict[date, list[ShiftRequirementInput]] = defaultdict(list)

        for sh in emp_shifts:
            by_week[sh.iso_week] += sh.duration_minutes
            by_day[sh.work_date].append(sh)
            if sh.shift_id in emp.preferred_shift_ids:
                pref_hits += 1
            if sh.shift_id in emp.avoid_shift_ids:
                avoid_violations += 1

        reg_cap_min = emp.max_regular_weekly_minutes
        for mins in by_week.values():
            reg = min(mins, reg_cap_min)
            ot = max(0, mins - reg_cap_min)
            total_regular_min += reg
            total_overtime_min += ot
            total_ot_sub_minor += ot * emp.overtime_cost_minor_per_hour

        if e_idx in active_set:
            wk = emp.prior_weekend_shifts + sum(1 for sh in emp_shifts if sh.is_weekend)
            weekend_counts.append(wk)

        for day_shifts in by_day.values():
            if len(day_shifts) > 1:
                ordered = sorted(day_shifts, key=lambda s: (s.starts_at, s.ends_at))
                for i in range(1, len(ordered)):
                    if ordered[i].starts_at > ordered[i - 1].ends_at:
                        split_shift_count += 1

    weekend_spread = (max(weekend_counts) - min(weekend_counts)) if weekend_counts else 0
    weekend_weight_sub = _WEEKEND_FAIRNESS_MINOR[profile.weekend_fairness_priority] * _SUB_MINOR_PER_MINOR
    pref_weight_sub = _STAFF_PREF_MINOR[profile.staff_preference_priority] * _SUB_MINOR_PER_MINOR
    split_weight_sub = (_SPLIT_SHIFT_PENALTY_MINOR * _SUB_MINOR_PER_MINOR) if profile.avoid_split_shifts else 0

    objective_score = (
        total_ot_sub_minor
        + weekend_weight_sub * weekend_spread
        - pref_weight_sub * pref_hits
        + (2 * pref_weight_sub) * avoid_violations
        + split_weight_sub * split_shift_count
    )

    # Exact minor currency units (half-to-even via Decimal when sub-minor isn't divisible by 60).
    total_ot_cost_dec = (Decimal(total_ot_sub_minor) / Decimal(6000)).quantize(Decimal("0.01"))
    total_ot_cost_minor = int(total_ot_cost_dec * Decimal(100))

    return RosterMetrics(
        total_shifts_filled=len(assigned_pairs),
        total_regular_minutes=total_regular_min,
        total_overtime_minutes=total_overtime_min,
        total_regular_hours=minutes_to_hours(total_regular_min),
        total_overtime_hours=minutes_to_hours(total_overtime_min),
        total_overtime_cost_minor=total_ot_cost_minor,
        total_overtime_cost=total_ot_cost_dec,
        max_weekend_spread=weekend_spread,
        preference_hits=pref_hits,
        avoid_violations=avoid_violations,
        split_shift_count=split_shift_count,
        objective_score=objective_score,
    )


def _solve_primary_cpsat(
    request: SolveRequest,
) -> tuple[SolveStatus, tuple[Assignment, ...], RosterMetrics | None]:
    """Build and solve the deterministic primary hard-law OR-Tools CP-SAT model."""
    employees, shifts = _sorted_inputs(request)
    profile = request.profile
    auth_transfers = frozenset(request.authorized_transfer_employee_ids)

    model = cp_model.CpModel()
    x: dict[tuple[int, int], cp_model.IntVar] = {}

    eligible_emps_per_shift: list[list[int]] = [[] for _ in range(len(shifts))]
    active_emp_set: set[int] = set()

    for e_idx, emp in enumerate(employees):
        for s_idx, sh in enumerate(shifts):
            if _is_primary_eligible(
                emp,
                sh,
                allow_cross_branch_transfers=request.allow_cross_branch_transfers,
                authorized_transfer_emp_ids=auth_transfers,
            ):
                x[(e_idx, s_idx)] = model.new_bool_var(f"x_e{e_idx}_s{s_idx}")
                eligible_emps_per_shift[s_idx].append(e_idx)
                active_emp_set.add(e_idx)

    # Fast exact infeasibility detection if any shift lacks enough eligible staff.
    for s_idx, sh in enumerate(shifts):
        if len(eligible_emps_per_shift[s_idx]) < sh.required_count:
            return "infeasible", (), None
        model.add(sum(x[(e_idx, s_idx)] for e_idx in eligible_emps_per_shift[s_idx]) == sh.required_count)

    # Statutory temporal hard laws (non-overlap, min rest, max daily hours, max consecutive days).
    _add_statutory_temporal_constraints(model, employees, shifts, x, profile)

    # Weekly hour caps + exact overtime minutes + weekly overtime budget cap.
    weeks = sorted({sh.iso_week for sh in shifts})
    shifts_by_week: dict[tuple[int, int], list[int]] = defaultdict(list)
    for s_idx, sh in enumerate(shifts):
        shifts_by_week[sh.iso_week].append(s_idx)

    budget_sub_minor = profile.weekly_overtime_budget_minor * _SUB_MINOR_PER_MINOR
    ot_cost_terms: list[cp_model.LinearExpr] = []

    for w_idx, week_key in enumerate(weeks):
        week_shift_indices = shifts_by_week[week_key]
        week_sub_minor_terms: list[cp_model.LinearExpr] = []
        for e_idx, emp in enumerate(employees):
            emp_week_terms = [
                shifts[s_idx].duration_minutes * x[(e_idx, s_idx)]
                for s_idx in week_shift_indices
                if (e_idx, s_idx) in x
            ]
            if not emp_week_terms:
                continue
            max_possible_min = sum(
                shifts[s_idx].duration_minutes for s_idx in week_shift_indices if (e_idx, s_idx) in x
            )
            effective_cap = min(profile.max_weekly_minutes, emp.max_total_weekly_minutes)
            total_w = model.new_int_var(0, min(max_possible_min, effective_cap), f"tot_e{e_idx}_w{w_idx}")
            model.add(total_w == sum(emp_week_terms))

            reg_cap = emp.max_regular_weekly_minutes
            max_ot = max(0, min(max_possible_min, effective_cap) - reg_cap)
            ot_w = model.new_int_var(0, max_ot, f"ot_e{e_idx}_w{w_idx}")
            diff_w = model.new_int_var(-reg_cap, max(0, max_possible_min - reg_cap), f"otdiff_e{e_idx}_w{w_idx}")
            model.add(diff_w == total_w - reg_cap)
            model.add_max_equality(ot_w, [0, diff_w])

            if emp.overtime_cost_minor_per_hour > 0:
                term = ot_w * emp.overtime_cost_minor_per_hour
                week_sub_minor_terms.append(term)
                ot_cost_terms.append(term)

        if week_sub_minor_terms:
            model.add(sum(week_sub_minor_terms) <= budget_sub_minor)

    # Soft objective terms (exact integer sub-minor scale, lower is better).
    objective_terms: list[cp_model.LinearExpr] = list(ot_cost_terms)

    # 1. Weekend fairness min-max spread across active employees.
    active_emp_indices = sorted(active_emp_set) if active_emp_set else list(range(len(employees)))
    weekend_shift_indices = [s_idx for s_idx, sh in enumerate(shifts) if sh.is_weekend]
    if active_emp_indices:
        wk_vars: list[cp_model.IntVar] = []
        max_possible_wk = max(e.prior_weekend_shifts for e in employees) + len(weekend_shift_indices)
        for e_idx in active_emp_indices:
            emp = employees[e_idx]
            wk_shift_vars = [x[(e_idx, s_idx)] for s_idx in weekend_shift_indices if (e_idx, s_idx) in x]
            wk_v = model.new_int_var(emp.prior_weekend_shifts, max_possible_wk, f"wk_e{e_idx}")
            model.add(wk_v == emp.prior_weekend_shifts + sum(wk_shift_vars))
            wk_vars.append(wk_v)
        max_wk = model.new_int_var(0, max_possible_wk, "wk_max")
        min_wk = model.new_int_var(0, max_possible_wk, "wk_min")
        model.add_max_equality(max_wk, wk_vars)
        model.add_min_equality(min_wk, wk_vars)
        wk_weight = _WEEKEND_FAIRNESS_MINOR[profile.weekend_fairness_priority] * _SUB_MINOR_PER_MINOR
        objective_terms.append(wk_weight * (max_wk - min_wk))

    # 2. Staff shift preferences & avoid penalties.
    pref_weight = _STAFF_PREF_MINOR[profile.staff_preference_priority] * _SUB_MINOR_PER_MINOR
    for e_idx, emp in enumerate(employees):
        pref_set = frozenset(emp.preferred_shift_ids)
        avoid_set = frozenset(emp.avoid_shift_ids)
        for s_idx, sh in enumerate(shifts):
            var = x.get((e_idx, s_idx))
            if var is None:
                continue
            if sh.shift_id in pref_set:
                objective_terms.append(-pref_weight * var)
            if sh.shift_id in avoid_set:
                objective_terms.append((2 * pref_weight) * var)

    # 3. Split-shift avoidance (two non-contiguous shifts on the same UTC date).
    if profile.avoid_split_shifts:
        split_weight = _SPLIT_SHIFT_PENALTY_MINOR * _SUB_MINOR_PER_MINOR
        shifts_by_date: dict[date, list[int]] = defaultdict(list)
        for s_idx, sh in enumerate(shifts):
            shifts_by_date[sh.work_date].append(s_idx)
        for day_indices in shifts_by_date.values():
            if len(day_indices) < 2:
                continue
            for a_i in range(len(day_indices)):
                for b_i in range(a_i + 1, len(day_indices)):
                    s1_idx = day_indices[a_i]
                    s2_idx = day_indices[b_i]
                    s1 = shifts[s1_idx]
                    s2 = shifts[s2_idx]
                    # Only non-contiguous pairs whose gap satisfies min_rest can both be worked.
                    if s1.ends_at < s2.starts_at and s1.window.gap_minutes(s2.window) >= profile.min_rest_minutes:
                        for e_idx in range(len(employees)):
                            v1 = x.get((e_idx, s1_idx))
                            v2 = x.get((e_idx, s2_idx))
                            if v1 is not None and v2 is not None:
                                split_v = model.new_bool_var(f"split_e{e_idx}_s{s1_idx}_s{s2_idx}")
                                model.add(split_v >= v1 + v2 - 1)
                                objective_terms.append(split_weight * split_v)

    model.minimize(sum(objective_terms))

    solver = _new_deterministic_solver(_PRIMARY_MAX_DETERMINISTIC_TIME)
    raw_status = solver.solve(model)

    if raw_status not in (cp_model.OPTIMAL, cp_model.FEASIBLE):
        return "infeasible", (), None

    assigned_pairs: list[tuple[int, int]] = []
    assignments_list: list[Assignment] = []
    for (e_idx, s_idx), var in sorted(x.items()):
        if solver.value(var) == 1:
            assigned_pairs.append((e_idx, s_idx))
            emp = employees[e_idx]
            sh = shifts[s_idx]
            assignments_list.append(
                Assignment(
                    shift_id=sh.shift_id,
                    employee_id=emp.employee_id,
                    unit_id=sh.unit_id,
                    role_code=sh.role_code,
                    starts_at=format_utc(sh.starts_at),
                    ends_at=format_utc(sh.ends_at),
                    duration_minutes=sh.duration_minutes,
                    hours=sh.duration_hours,
                    is_cross_branch=(sh.unit_id != emp.home_unit_id),
                    is_cross_skill=False,
                )
            )

    assignments_list.sort(key=lambda a: (a.starts_at, a.shift_id, a.employee_id))
    metrics = _compute_exact_metrics(employees, shifts, assigned_pairs, active_emp_indices, profile)
    status: SolveStatus = "optimal" if raw_status == cp_model.OPTIMAL else "feasible"
    return status, tuple(assignments_list), metrics


def _solve_repair_cpsat(
    request: SolveRequest,
    *,
    allow_coverage_slack: bool,
    allow_overtime_slack: bool,
    allow_transfer_slack: bool,
) -> tuple[RepairSuggestion, ...] | None:
    """Deterministic minimum-slack CP-SAT repair model.

    Strictly preserves statutory labor laws (non-overlap, min rest, max daily hours,
    max consecutive days, approved leave) while adding bounded integer slack variables
    across the enabled repair axes:
      - `relax_coverage`: per-shift headcount shortfall `shortfall[s] in [0, required_count]`
      - `approve_overtime`: per-employee/week cap overflow + overtime budget overflow
      - `allow_transfer`: cross-branch and cross-skill transfer assignments
    """
    employees, shifts = _sorted_inputs(request)
    profile = request.profile
    auth_transfers = frozenset(request.authorized_transfer_employee_ids)

    model = cp_model.CpModel()
    x: dict[tuple[int, int], cp_model.IntVar] = {}
    transfer_vars: dict[tuple[int, int], TransferMode] = {}
    vars_per_shift: list[list[int]] = [[] for _ in range(len(shifts))]

    for e_idx, emp in enumerate(employees):
        for s_idx, sh in enumerate(shifts):
            if _is_primary_eligible(
                emp,
                sh,
                allow_cross_branch_transfers=request.allow_cross_branch_transfers,
                authorized_transfer_emp_ids=auth_transfers,
            ):
                x[(e_idx, s_idx)] = model.new_bool_var(f"rx_e{e_idx}_s{s_idx}")
                vars_per_shift[s_idx].append(e_idx)
            elif allow_transfer_slack:
                mode = _transfer_eligibility(emp, sh)
                if mode is not None:
                    x[(e_idx, s_idx)] = model.new_bool_var(f"rt_e{e_idx}_s{s_idx}")
                    transfer_vars[(e_idx, s_idx)] = mode
                    vars_per_shift[s_idx].append(e_idx)

    # Coverage constraints (with bounded shortfall slack when enabled).
    shortfall_vars: dict[int, cp_model.IntVar] = {}
    for s_idx, sh in enumerate(shifts):
        assigned_sum = sum(x[(e_idx, s_idx)] for e_idx in vars_per_shift[s_idx])
        if allow_coverage_slack:
            sf = model.new_int_var(0, sh.required_count, f"sf_s{s_idx}")
            model.add(assigned_sum + sf == sh.required_count)
            shortfall_vars[s_idx] = sf
        else:
            if len(vars_per_shift[s_idx]) < sh.required_count:
                return None
            model.add(assigned_sum == sh.required_count)

    # Non-negotiable statutory temporal laws (NEVER relaxed).
    _add_statutory_temporal_constraints(model, employees, shifts, x, profile)

    # Weekly hours & overtime budget constraints (with bounded overflow slack when enabled).
    weeks = sorted({sh.iso_week for sh in shifts})
    shifts_by_week: dict[tuple[int, int], list[int]] = defaultdict(list)
    for s_idx, sh in enumerate(shifts):
        shifts_by_week[sh.iso_week].append(s_idx)

    budget_sub_minor = profile.weekly_overtime_budget_minor * _SUB_MINOR_PER_MINOR
    ot_overflow_vars: dict[tuple[int, int], cp_model.IntVar] = {}
    ot_used_vars: dict[tuple[int, int], cp_model.IntVar] = {}
    ot_min_vars: dict[tuple[int, int], cp_model.IntVar] = {}
    budget_overflow_vars: dict[int, cp_model.IntVar] = {}
    total_ot_sub_minor_terms: list[cp_model.LinearExpr] = []

    for w_idx, week_key in enumerate(weeks):
        week_shift_indices = shifts_by_week[week_key]
        week_sub_minor_terms: list[cp_model.LinearExpr] = []
        max_week_sub_minor = 0

        for e_idx, emp in enumerate(employees):
            emp_week_terms = [
                shifts[s_idx].duration_minutes * x[(e_idx, s_idx)]
                for s_idx in week_shift_indices
                if (e_idx, s_idx) in x
            ]
            if not emp_week_terms:
                continue

            max_possible_min = sum(
                shifts[s_idx].duration_minutes for s_idx in week_shift_indices if (e_idx, s_idx) in x
            )
            base_cap = min(profile.max_weekly_minutes, emp.max_total_weekly_minutes)
            reg_cap = emp.max_regular_weekly_minutes

            if allow_overtime_slack:
                upper_cap = min(max_possible_min, base_cap + _MAX_REPAIR_OVERTIME_OVERFLOW_MINUTES)
                total_w = model.new_int_var(0, upper_cap, f"rtot_e{e_idx}_w{w_idx}")
                model.add(total_w == sum(emp_week_terms))

                max_ov = max(0, upper_cap - base_cap)
                ov_w = model.new_int_var(0, max_ov, f"rov_e{e_idx}_w{w_idx}")
                diff_cap = model.new_int_var(-base_cap, max(0, upper_cap - base_cap), f"rcapdiff_e{e_idx}_w{w_idx}")
                model.add(diff_cap == total_w - base_cap)
                model.add_max_equality(ov_w, [0, diff_cap])
                ot_overflow_vars[(e_idx, w_idx)] = ov_w

                used_w = model.new_bool_var(f"rotused_e{e_idx}_w{w_idx}")
                if max_ov > 0:
                    model.add(ov_w <= max_ov * used_w)
                    model.add(ov_w >= used_w)
                else:
                    model.add(used_w == 0)
                ot_used_vars[(e_idx, w_idx)] = used_w
            else:
                upper_cap = min(max_possible_min, base_cap)
                total_w = model.new_int_var(0, upper_cap, f"rtot_e{e_idx}_w{w_idx}")
                model.add(total_w == sum(emp_week_terms))

            max_ot_min = max(0, upper_cap - reg_cap)
            ot_w = model.new_int_var(0, max_ot_min, f"rot_e{e_idx}_w{w_idx}")
            diff_reg = model.new_int_var(-reg_cap, max(0, upper_cap - reg_cap), f"rregdiff_e{e_idx}_w{w_idx}")
            model.add(diff_reg == total_w - reg_cap)
            model.add_max_equality(ot_w, [0, diff_reg])
            ot_min_vars[(e_idx, w_idx)] = ot_w

            if emp.overtime_cost_minor_per_hour > 0 and max_ot_min > 0:
                term = ot_w * emp.overtime_cost_minor_per_hour
                week_sub_minor_terms.append(term)
                total_ot_sub_minor_terms.append(term)
                max_week_sub_minor += max_ot_min * emp.overtime_cost_minor_per_hour

        if week_sub_minor_terms:
            if allow_overtime_slack:
                max_budget_ov = max(0, max_week_sub_minor - budget_sub_minor)
                b_ov = model.new_int_var(0, max_budget_ov, f"rbov_w{w_idx}")
                model.add(sum(week_sub_minor_terms) <= budget_sub_minor + b_ov)
                budget_overflow_vars[w_idx] = b_ov
            else:
                model.add(sum(week_sub_minor_terms) <= budget_sub_minor)

    # Lexicographically-weighted exact integer slack objective (minimize total disruption).
    repair_obj: list[cp_model.LinearExpr] = []
    for sf in shortfall_vars.values():
        repair_obj.append(_REPAIR_W_COVERAGE_SHORTFALL * sf)
    for (e_idx, s_idx), mode in sorted(transfer_vars.items()):
        w = _REPAIR_W_CROSS_BRANCH if mode == "cross_branch" else _REPAIR_W_CROSS_SKILL
        repair_obj.append(w * x[(e_idx, s_idx)])
    for used_w in ot_used_vars.values():
        repair_obj.append(_REPAIR_W_OVERTIME_EMP * used_w)
    for ov_w in ot_overflow_vars.values():
        repair_obj.append(_REPAIR_W_OVERTIME_MINUTE * ov_w)
    for b_ov in budget_overflow_vars.values():
        repair_obj.append(_REPAIR_W_BUDGET_SUB_MINOR * b_ov)
    repair_obj.extend(total_ot_sub_minor_terms)

    model.minimize(sum(repair_obj))

    solver = _new_deterministic_solver(_REPAIR_MAX_DETERMINISTIC_TIME)
    raw_status = solver.solve(model)
    if raw_status not in (cp_model.OPTIMAL, cp_model.FEASIBLE):
        return None

    # Independently verify that the repaired schedule respects ALL statutory laws!
    repaired_assignments: list[Assignment] = []
    assigned_by_emp_week: dict[tuple[int, int], list[ShiftRequirementInput]] = defaultdict(list)
    week_index_by_key = {wk: idx for idx, wk in enumerate(weeks)}

    for (e_idx, s_idx), var in sorted(x.items()):
        if solver.value(var) == 1:
            emp = employees[e_idx]
            sh = shifts[s_idx]
            mode = transfer_vars.get((e_idx, s_idx))
            repaired_assignments.append(
                Assignment(
                    shift_id=sh.shift_id,
                    employee_id=emp.employee_id,
                    unit_id=sh.unit_id,
                    role_code=sh.role_code,
                    starts_at=format_utc(sh.starts_at),
                    ends_at=format_utc(sh.ends_at),
                    duration_minutes=sh.duration_minutes,
                    hours=sh.duration_hours,
                    is_cross_branch=(sh.unit_id != emp.home_unit_id),
                    is_cross_skill=(mode == "cross_skill"),
                )
            )
            assigned_by_emp_week[(e_idx, week_index_by_key[sh.iso_week])].append(sh)

    statutory_check = verify_statutory_laws(request, repaired_assignments)
    if not statutory_check.passed:
        return None

    suggestions: list[RepairSuggestion] = []

    # 1. Coverage shortfall repairs (`relax_coverage`)
    for s_idx, sf_var in sorted(shortfall_vars.items()):
        sf_val = int(solver.value(sf_var))
        if sf_val > 0:
            sh = shifts[s_idx]
            new_count = sh.required_count - sf_val
            delta_min = sh.duration_minutes * sf_val
            suggestions.append(
                RepairSuggestion(
                    kind="relax_coverage",
                    target_shift_id=sh.shift_id,
                    employee_id=None,
                    from_unit_id=sh.unit_id,
                    to_unit_id=sh.unit_id,
                    role_code=sh.role_code,
                    transfer_mode=None,
                    coverage_shortfall=sf_val,
                    delta_minutes=delta_min,
                    delta_hours=minutes_to_hours(delta_min),
                    cost_delta_minor=0,
                    cost_delta=Decimal("0.00"),
                    requires_approval=True,
                    hint=(
                        f"Relax {sh.role_code} coverage on {sh.shift_id} at {sh.unit_id} "
                        f"({format_utc(sh.starts_at)}–{format_utc(sh.ends_at)}) "
                        f"from {sh.required_count} to {new_count} (-{sf_val} headcount)"
                    ),
                    diagnostic_code="NORN-ROSTER-REPAIR-COVERAGE",
                )
            )

    # 2. Cross-branch / cross-skill transfer repairs (`allow_transfer`)
    for (e_idx, s_idx), mode in sorted(transfer_vars.items()):
        if solver.value(x[(e_idx, s_idx)]) == 1:
            emp = employees[e_idx]
            sh = shifts[s_idx]
            w_idx = week_index_by_key[sh.iso_week]
            ot_min_var = ot_min_vars.get((e_idx, w_idx))
            ot_min = int(solver.value(ot_min_var)) if ot_min_var is not None else 0
            cost_sub = min(sh.duration_minutes, ot_min) * emp.overtime_cost_minor_per_hour
            cost_dec = (Decimal(cost_sub) / Decimal(6000)).quantize(Decimal("0.01"))
            cost_minor = int(cost_dec * Decimal(100))

            if mode == "cross_branch":
                hint = (
                    f"Transfer {emp.display_label} from {emp.home_unit_id} to {sh.unit_id} "
                    f"to cover {sh.role_code} shift {sh.shift_id} ({format_utc(sh.starts_at)}–{format_utc(sh.ends_at)})"
                )
            else:
                hint = (
                    f"Authorize cross-skill assignment for {emp.display_label} ({emp.home_unit_id}) "
                    f"to cover {sh.role_code} shift {sh.shift_id} at {sh.unit_id} "
                    f"({format_utc(sh.starts_at)}–{format_utc(sh.ends_at)})"
                )

            suggestions.append(
                RepairSuggestion(
                    kind="allow_transfer",
                    target_shift_id=sh.shift_id,
                    employee_id=emp.employee_id,
                    from_unit_id=emp.home_unit_id,
                    to_unit_id=sh.unit_id,
                    role_code=sh.role_code,
                    transfer_mode=mode,
                    coverage_shortfall=None,
                    delta_minutes=sh.duration_minutes,
                    delta_hours=sh.duration_hours,
                    cost_delta_minor=cost_minor,
                    cost_delta=cost_dec,
                    requires_approval=True,
                    hint=hint,
                    diagnostic_code="NORN-ROSTER-REPAIR-TRANSFER",
                )
            )

    # 3. Overtime cap / budget overflow repairs (`approve_overtime`)
    for w_idx in range(len(weeks)):
        b_ov_var = budget_overflow_vars.get(w_idx)
        b_ov_val = int(solver.value(b_ov_var)) if b_ov_var is not None else 0

        for e_idx, emp in enumerate(employees):
            ov_var = ot_overflow_vars.get((e_idx, w_idx))
            ot_var = ot_min_vars.get((e_idx, w_idx))
            ov_min = int(solver.value(ov_var)) if ov_var is not None else 0
            ot_min = int(solver.value(ot_var)) if ot_var is not None else 0

            # Emit overtime repair if employee exceeded their weekly cap OR incurred overtime driving budget overflow.
            if ov_min <= 0 and not (b_ov_val > 0 and ot_min > 0):
                continue

            reported_min = ov_min if ov_min > 0 else ot_min
            cost_sub = reported_min * emp.overtime_cost_minor_per_hour
            cost_dec = (Decimal(cost_sub) / Decimal(6000)).quantize(Decimal("0.01"))
            cost_minor = int(cost_dec * Decimal(100))

            emp_week_shifts = assigned_by_emp_week.get((e_idx, w_idx), [])
            target_shift = emp_week_shifts[-1].shift_id if emp_week_shifts else None
            target_role = emp_week_shifts[-1].role_code if emp_week_shifts else None
            target_unit = emp_week_shifts[-1].unit_id if emp_week_shifts else emp.home_unit_id

            suggestions.append(
                RepairSuggestion(
                    kind="approve_overtime",
                    target_shift_id=target_shift,
                    employee_id=emp.employee_id,
                    from_unit_id=emp.home_unit_id,
                    to_unit_id=target_unit,
                    role_code=target_role,
                    transfer_mode=None,
                    coverage_shortfall=None,
                    delta_minutes=reported_min,
                    delta_hours=minutes_to_hours(reported_min),
                    cost_delta_minor=cost_minor,
                    cost_delta=cost_dec,
                    requires_approval=True,
                    hint=(
                        f"Approve +{minutes_to_hours(reported_min)}h ({reported_min} min) overtime for "
                        f"{emp.display_label} (+{cost_dec} {profile.currency}; "
                        f"statutory rest, daily & leave laws satisfied)"
                    ),
                    diagnostic_code="NORN-ROSTER-REPAIR-OVERTIME",
                )
            )

    suggestions.sort(
        key=lambda r: (
            r.kind,
            r.cost_delta_minor,
            r.target_shift_id or "",
            r.employee_id or "",
            r.coverage_shortfall or 0,
            r.delta_minutes or 0,
        )
    )
    return tuple(suggestions)


def _find_unsat_repairs(
    request: SolveRequest,
) -> tuple[tuple[RepairSuggestion, ...], tuple[RepairSuggestion, ...]]:
    """Run the joint minimum-slack CP-SAT repair model plus single-axis alternatives."""
    joint = _solve_repair_cpsat(
        request,
        allow_coverage_slack=True,
        allow_overtime_slack=True,
        allow_transfer_slack=True,
    )
    primary_repairs: tuple[RepairSuggestion, ...] = joint if joint is not None else ()

    primary_keys = {
        (r.kind, r.target_shift_id, r.employee_id, r.coverage_shortfall, r.delta_minutes)
        for r in primary_repairs
    }
    alternatives: list[RepairSuggestion] = []
    seen_alt: set[tuple[object, ...]] = set(primary_keys)

    for cov, ot, tr in (
        (False, False, True),
        (False, True, False),
        (True, False, False),
    ):
        probe = _solve_repair_cpsat(
            request,
            allow_coverage_slack=cov,
            allow_overtime_slack=ot,
            allow_transfer_slack=tr,
        )
        if not probe:
            continue
        for r in probe:
            key = (r.kind, r.target_shift_id, r.employee_id, r.coverage_shortfall, r.delta_minutes)
            if key not in seen_alt:
                seen_alt.add(key)
                alternatives.append(r)

    alternatives.sort(
        key=lambda r: (
            r.kind,
            r.cost_delta_minor,
            r.target_shift_id or "",
            r.employee_id or "",
            r.coverage_shortfall or 0,
            r.delta_minutes or 0,
        )
    )
    return primary_repairs, tuple(alternatives)


def solve_roster(request: SolveRequest) -> SolveResponse:
    """Solve one shift-rostering problem deterministically with OR-Tools CP-SAT."""
    input_digest = sha256_digest(request.to_dict())
    status, assignments, metrics = _solve_primary_cpsat(request)

    if status in ("optimal", "feasible") and metrics is not None:
        # Independent hard-law post-verification (fails closed on any violation).
        ver_result = verify_or_raise(request, assignments)
        verification = ver_result.to_summary()
        repair_suggestions: tuple[RepairSuggestion, ...] = ()
        alternative_repairs: tuple[RepairSuggestion, ...] = ()
        unsat_summary: str | None = None
    else:
        status = "infeasible"
        assignments = ()
        metrics = None
        verification = VerificationSummary(
            checked=False,
            passed=False,
            checks_run=(),
            violation_count=0,
        )
        repair_suggestions, alternative_repairs = _find_unsat_repairs(request)
        if repair_suggestions:
            kinds_summary = ", ".join(sorted({r.kind for r in repair_suggestions}))
            unsat_summary = (
                f"No legal roster satisfies all hard constraints for unit {request.unit_id}; "
                f"minimum-slack CP-SAT repair plan requires {len(repair_suggestions)} action(s) ({kinds_summary})."
            )
        else:
            unsat_summary = (
                f"No legal roster satisfies all hard constraints for unit {request.unit_id}, "
                "and no bounded statutory-compliant repair plan was found."
            )

    receipt_payload: dict[str, object] = {
        "alternative_repairs": [r.to_dict() for r in alternative_repairs],
        "assignments": [a.to_dict() for a in assignments],
        "backend": "ortools_cpsat",
        "contract": CONTRACT_ID,
        "input_digest": input_digest,
        "profile_id": request.profile.profile_id,
        "repair_suggestions": [r.to_dict() for r in repair_suggestions],
        "roster_id": request.roster_id,
        "status": status,
        "verification": verification.to_dict(),
    }
    solver_receipt_digest = sha256_digest(receipt_payload)

    return SolveResponse(
        contract=CONTRACT_ID,
        roster_id=request.roster_id,
        profile_id=request.profile.profile_id,
        status=status,
        backend="ortools_cpsat",
        assignments=assignments,
        metrics=metrics,
        repair_suggestions=repair_suggestions,
        alternative_repairs=alternative_repairs,
        unsat_summary=unsat_summary,
        verification=verification,
        input_digest=input_digest,
        solver_receipt_digest=solver_receipt_digest,
    )


def compare_what_if(base_request: SolveRequest, alt_profile: PreferenceProfileInput) -> WhatIfComparison:
    """Deterministic side-by-side comparison across two `PreferenceProfile` runs."""
    base_res = solve_roster(base_request)
    alt_res = solve_roster(replace(base_request, profile=alt_profile))

    # Build deterministic per-shift assignment diff.
    shifts_by_id = {s.shift_id: s for s in base_request.shifts}
    base_by_shift: dict[str, list[str]] = defaultdict(list)
    alt_by_shift: dict[str, list[str]] = defaultdict(list)
    for a in base_res.assignments:
        base_by_shift[a.shift_id].append(a.employee_id)
    for a in alt_res.assignments:
        alt_by_shift[a.shift_id].append(a.employee_id)

    diffs: list[AssignmentDiff] = []
    for sh in sorted(shifts_by_id.values(), key=lambda s: (s.starts_at, s.ends_at, s.shift_id)):
        b_emps = tuple(sorted(base_by_shift.get(sh.shift_id, [])))
        a_emps = tuple(sorted(alt_by_shift.get(sh.shift_id, [])))
        if b_emps != a_emps:
            diffs.append(
                AssignmentDiff(
                    shift_id=sh.shift_id,
                    unit_id=sh.unit_id,
                    role_code=sh.role_code,
                    starts_at=format_utc(sh.starts_at),
                    ends_at=format_utc(sh.ends_at),
                    base_employee_ids=b_emps,
                    alt_employee_ids=a_emps,
                )
            )

    if base_res.metrics is not None and alt_res.metrics is not None:
        ot_min_delta = alt_res.metrics.total_overtime_minutes - base_res.metrics.total_overtime_minutes
        hours_delta = (alt_res.metrics.total_overtime_hours - base_res.metrics.total_overtime_hours).quantize(
            Decimal("0.01")
        )
        cost_minor_delta = alt_res.metrics.total_overtime_cost_minor - base_res.metrics.total_overtime_cost_minor
        cost_delta = minor_units_to_decimal(cost_minor_delta)
        spread_delta = alt_res.metrics.max_weekend_spread - base_res.metrics.max_weekend_spread
        pref_delta = alt_res.metrics.preference_hits - base_res.metrics.preference_hits
        obj_delta = alt_res.metrics.objective_score - base_res.metrics.objective_score
        summary = (
            f"Switching from {base_request.profile.label!r} to {alt_profile.label!r}: "
            f"{len(diffs)} shift(s) changed, "
            f"overtime cost {cost_delta:+} {alt_profile.currency}, "
            f"overtime hours {hours_delta:+}h, "
            f"weekend spread {spread_delta:+d}, "
            f"staff preference hits {pref_delta:+d}, "
            f"objective delta {obj_delta:+d}."
        )
    else:
        ot_min_delta = None
        hours_delta = None
        cost_minor_delta = None
        cost_delta = None
        spread_delta = None
        pref_delta = None
        obj_delta = None
        summary = (
            f"{base_request.profile.label!r} -> {base_res.status}; "
            f"{alt_profile.label!r} -> {alt_res.status} ({len(diffs)} shift(s) changed)."
        )

    return WhatIfComparison(
        base_profile_id=base_request.profile.profile_id,
        alt_profile_id=alt_profile.profile_id,
        base_status=base_res.status,
        alt_status=alt_res.status,
        base_receipt_digest=base_res.solver_receipt_digest,
        alt_receipt_digest=alt_res.solver_receipt_digest,
        changed_assignments=tuple(diffs),
        changed_assignment_count=len(diffs),
        overtime_minutes_delta=ot_min_delta,
        overtime_hours_delta=hours_delta,
        overtime_cost_minor_delta=cost_minor_delta,
        overtime_cost_delta=cost_delta,
        max_weekend_spread_delta=spread_delta,
        preference_hits_delta=pref_delta,
        objective_score_delta=obj_delta,
        summary=summary,
    )
