"""Synthetic HR pilot authored in max-strict typed Python (#1389 Option B, Epic #1595).

Demonstrates:
- PII `MarkingsCatalog` + `Annotated[..., Marking(...)]`
- `core.TimeInterval` interface (`role="happening"`) with symbolically traced `timestamp_before` invariant
- `Employee` (`role="thing"`), `LeaveRequest` (`StrEnum` lifecycle + guards), `Shift` (`role="happening"`)
- Per-record `LawIR` traced via `Sym[T]` + exact `Decimal` (zero `float`, zero `unsafeZen`)
- External OR-Tools CP-SAT solver declared strictly as a `fn_ref` handle (`perform` boundary)
"""

from __future__ import annotations

from decimal import Decimal
from enum import StrEnum
from typing import Annotated

from norn_py import (
    BoolTag,
    Concept,
    Expr,
    KeySpec,
    Link,
    Marking,
    MarkingDecl,
    MarkingsCatalog,
    Sym,
    UnitRef,
    all_of,
    decimal_type,
    fn_ref,
    invariant,
    law,
    lifecycle,
    link,
    mask_last_n,
    mask_redacted,
    timestamp_before,
    timestamp_type,
    transition,
)

HOURS = UnitRef(system="ucum", code="h")
DAYS = UnitRef(system="ucum", code="d")


HR_MARKINGS = MarkingsCatalog(
    id="hr.privacy.markings",
    title="HR privacy markings",
    version="0.1.0",
    markings=(
        MarkingDecl(
            id="pii.demographic",
            label="Demographic PII",
            severity="high",
            category="pii",
            mask=mask_redacted(),
        ),
        MarkingDecl(
            id="pii.gov_id",
            label="Government ID",
            severity="critical",
            category="pii",
            mask=mask_last_n(4),
        ),
        MarkingDecl(
            id="pii.compensation",
            label="Compensation",
            severity="high",
            category="pii",
            mask=mask_redacted(),
        ),
    ),
)


class TimeInterval(
    Concept,
    name="core.TimeInterval",
    title="Time interval",
    version="0.1.0",
    interface=True,
    role="happening",
    description="Half-open UTC interval [starts_at, ends_at) for happenings (shifts, leaves, bookings).",
):
    starts_at: Annotated[str, timestamp_type("utc")]
    ends_at: Annotated[str, timestamp_type("utc")]
    timezone: str | None

    @invariant("proper_interval", message="starts_at must be strictly before ends_at")
    def _proper_interval(s: Sym[TimeInterval]) -> Expr[BoolTag]:
        return timestamp_before(s.starts_at, s.ends_at)


class Employee(
    Concept,
    name="hr.Employee",
    title="Employee",
    version="0.1.0",
    role="thing",
    identity=(KeySpec("primary", ("id",)),),
):
    id: str
    display_name: str
    gender: Annotated[str, Marking("pii.demographic")]
    national_id: Annotated[str, Marking("pii.gov_id")]
    currency: str
    base_salary: Annotated[Decimal, decimal_type(2, currency="currency"), Marking("pii.compensation")]


class LeaveStatus(StrEnum):
    DRAFT = "draft"
    PENDING = "pending"
    APPROVED = "approved"
    REJECTED = "rejected"
    CANCELLED = "cancelled"


class LeaveRequest(
    Concept,
    name="hr.LeaveRequest",
    title="Leave request",
    version="0.1.0",
    role="happening",
    implements=(TimeInterval,),
    identity=(KeySpec("primary", ("id",)),),
):
    id: str
    starts_at: Annotated[str, timestamp_type("utc")]
    ends_at: Annotated[str, timestamp_type("utc")]
    timezone: str | None
    days: Annotated[Decimal, decimal_type(2, unit=DAYS)]
    status: LeaveStatus = LeaveStatus.DRAFT
    employee: Annotated[Link[Employee], link(Employee, to="one", from_="many", required=True)]

    state = lifecycle(
        "status",
        LeaveStatus,
        initial=(LeaveStatus.DRAFT,),
        terminal=(LeaveStatus.APPROVED, LeaveStatus.REJECTED, LeaveStatus.CANCELLED),
        transitions=(
            transition(
                "submit",
                from_=(LeaveStatus.DRAFT,),
                to=LeaveStatus.PENDING,
                guards=(lambda s: s.days > Decimal("0.00"),),
            ),
            transition(
                "approve",
                from_=(LeaveStatus.PENDING,),
                to=LeaveStatus.APPROVED,
            ),
            transition(
                "reject",
                from_=(LeaveStatus.PENDING,),
                to=LeaveStatus.REJECTED,
            ),
            transition(
                "cancel",
                from_=(LeaveStatus.DRAFT, LeaveStatus.PENDING),
                to=LeaveStatus.CANCELLED,
            ),
        ),
    )


class Shift(
    Concept,
    name="hr.Shift",
    title="Shift commitment",
    version="0.1.0",
    role="happening",
    implements=(TimeInterval,),
    identity=(KeySpec("primary", ("id",)),),
):
    id: str
    starts_at: Annotated[str, timestamp_type("utc")]
    ends_at: Annotated[str, timestamp_type("utc")]
    timezone: str | None
    hours: Annotated[Decimal, decimal_type(2, unit=HOURS)]
    employee: Annotated[Link[Employee], link(Employee, to="one", from_="many", required=True)]


@law(
    id="hr.shift.valid_window_and_hours",
    title="Shift window and duration bounds",
    version="0.1.0",
    owner="hr",
    subject=Shift,
    rationale="Every committed shift must have a positive half-open UTC interval and bounded hours.",
)
def shift_bounds(s: Sym[Shift]) -> Expr[BoolTag]:
    return all_of(
        timestamp_before(s.starts_at, s.ends_at),
        s.hours > Decimal("0.00"),
        s.hours <= Decimal("12.00"),
    )


SOLVE_ROSTER = fn_ref(
    id="hr.solver.ortools_roster",
    version="0.1.0",
    input_type="hr.RosterSolveInput",
    output_type="hr.RosterProposal",
)
