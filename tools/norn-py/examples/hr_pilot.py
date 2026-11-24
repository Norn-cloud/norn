"""Synthetic HR pilot authored in max-strict typed Python (#1389 Option B, Epic #1595).

Demonstrates:
- PII `MarkingsCatalog` + `Annotated[..., Marking(...)]`
- `hr_pilot.TimeSpan` interface (`role="happening"`) with symbolically traced `timestamp_before` invariant
  (distinct from `registry/core`'s `core.TimeInterval.interface@0.1.0` so there is no artifact collision)
- `Employee` (`role="thing"`), `LeaveRequest` (`StrEnum` lifecycle + guards + pinned `core.units@1.0.0`),
  `Shift` (`role="happening"` + pinned `core.units@1.0.0`)
- Concrete static field typing via `Field[T]`, `Timestamp`, `Quantity[Literal["h"|"d"]]`, `Money`, `Link`
- Per-record `LawIR` traced via `Sym[Shift]` + exact `Decimal` (zero `float`, zero `unsafeZen`)
- External OR-Tools CP-SAT solver declared strictly as a `fn_ref` handle (spike prototype outside collector)
"""

from __future__ import annotations

from decimal import Decimal
from enum import StrEnum
from typing import Annotated, Literal

from norn_py import (
    Concept,
    DependencyPin,
    Expr,
    Field,
    KeySpec,
    Link,
    Marking,
    MarkingDecl,
    MarkingsCatalog,
    Money,
    Quantity,
    Sym,
    Timestamp,
    UnitRef,
    all_of,
    decimal_type,
    field_default,
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

CORE_UNITS = DependencyPin("core.units", "1.0.0", kind="entity")
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


class TimeSpan(
    Concept,
    name="hr_pilot.TimeSpan",
    title="HR pilot time span",
    version="0.1.0",
    interface=True,
    role="happening",
    description="Half-open UTC interval [starts_at, ends_at) for HR pilot happenings (shifts, leaves).",
):
    starts_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    ends_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    timezone: Field[str | None]

    @invariant("proper_interval", message="starts_at must be strictly before ends_at")
    def _proper_interval(s: Sym[TimeSpan]) -> Expr[bool]:
        return timestamp_before(s.starts_at, s.ends_at)


class Employee(
    Concept,
    name="hr.Employee",
    title="Employee",
    version="0.1.0",
    role="thing",
    identity=(KeySpec("primary", ("id",)),),
):
    id: Field[str]
    display_name: Field[str]
    gender: Annotated[Field[str], Marking("pii.demographic")]
    national_id: Annotated[Field[str], Marking("pii.gov_id")]
    currency: Field[str]
    base_salary: Annotated[
        Field[Money[Literal["currency"]]],
        decimal_type(2, currency="currency"),
        Marking("pii.compensation"),
    ]


class LeaveStatus(StrEnum):
    DRAFT = "draft"
    PENDING = "pending"
    APPROVED = "approved"
    REJECTED = "rejected"
    CANCELLED = "cancelled"


def _positive_leave_days(s: Sym[LeaveRequest]) -> Expr[bool]:
    return s.days > Decimal("0.00")


class LeaveRequest(
    Concept,
    name="hr.LeaveRequest",
    title="Leave request",
    version="0.1.0",
    role="happening",
    implements=(TimeSpan,),
    identity=(KeySpec("primary", ("id",)),),
    dependencies=(CORE_UNITS,),
):
    id: Field[str]
    starts_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    ends_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    timezone: Field[str | None]
    days: Annotated[Field[Quantity[Literal["d"]]], decimal_type(2, unit=DAYS)]
    status: Field[LeaveStatus] = field_default(LeaveStatus.DRAFT)
    employee: Annotated[Field[Link[Employee]], link(Employee, to="one", from_="many", required=True)]

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
                guards=(_positive_leave_days,),
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
    implements=(TimeSpan,),
    identity=(KeySpec("primary", ("id",)),),
    dependencies=(CORE_UNITS,),
):
    id: Field[str]
    starts_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    ends_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    timezone: Field[str | None]
    hours: Annotated[Field[Quantity[Literal["h"]]], decimal_type(2, unit=HOURS)]
    employee: Annotated[Field[Link[Employee]], link(Employee, to="one", from_="many", required=True)]


@law(
    id="hr.shift.valid_window_and_hours",
    title="Shift window and duration bounds",
    version="0.1.0",
    owner="hr",
    subject=Shift,
    rationale="Every committed shift must have a positive half-open UTC interval and bounded hours.",
)
def shift_bounds(s: Sym[Shift]) -> Expr[bool]:
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
