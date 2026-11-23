// HR Shift Safety (`INV-SHIFT-SAFETY`, `HR-SH-01..04`), bounded:
// `hr.Roster` lifecycle (`draft` -> `proposed` -> `published` -> `superseded`),
// half-open UTC `hr.ShiftCommitment` intervals, approved `hr.LeaveRequest`
// intervals, solver receipt requirement, and statutory minimum rest floor
// (Egypt `loc_eg.LaborProfile`: 11 hours).
//
// `step` enforces all commit-time guards; `publishRosterUnchecked` and
// `relaxBelowStatutoryFloor` (under `stepUnchecked`) explicitly refute safety
// when admission guards or statutory floors are bypassed.
// This is a bounded model of the HR pack laws, not of the Rust kernel or solver.
module hr_shift_safety {
  type Shift = { present: bool, emp: int, startH: int, endH: int }
  type Leave = { status: str, emp: int, startH: int, endH: int }

  var rosterStatus: str
  var hasReceipt: bool
  var minRestHours: int
  var shifts: List[Shift]
  var leaves: List[Leave]
  var outcome: str

  pure val EMPLOYEES: Set[int] = 0.to(1)
  pure val SHIFT_SLOTS: Set[int] = 0.to(2)
  pure val LEAVE_SLOTS: Set[int] = 0.to(1)
  pure val STATUTORY_MIN_REST: int = 11
  // Representative UTC hour grid covering morning [6,14), afternoon/closing [14,22)
  // & [15,23), illegal clopening [30,38) (7h < 11h rest, HR-SH-03), and legal
  // next-day shift [34,42) (11h >= 11h rest).
  pure val HOURS: Set[int] = Set(6, 14, 15, 22, 23, 30, 34, 38, 42)
  pure val emptyShift: Shift = { present: false, emp: 0, startH: 0, endH: 0 }
  pure val emptyLeave: Leave = { status: "empty", emp: 0, startH: 0, endH: 0 }

  pure def intervalsOverlap(s1: int, e1: int, s2: int, e2: int): bool =
    s1 < e2 and s2 < e1

  pure def restGapOk(s1: int, e1: int, s2: int, e2: int, minRest: int): bool =
    e1 + minRest <= s2 or e2 + minRest <= s1

  pure def shiftsHalfOpen(ss: List[Shift]): bool =
    SHIFT_SLOTS.forall(i => ss[i].present implies ss[i].startH < ss[i].endH)

  pure def shiftsNonOverlapping(ss: List[Shift]): bool =
    SHIFT_SLOTS.forall(i =>
      SHIFT_SLOTS.forall(j =>
        (i != j and ss[i].present and ss[j].present and ss[i].emp == ss[j].emp) implies
          not(intervalsOverlap(ss[i].startH, ss[i].endH, ss[j].startH, ss[j].endH))
      )
    )

  pure def shiftsRespectMinRest(ss: List[Shift], minRest: int): bool =
    SHIFT_SLOTS.forall(i =>
      SHIFT_SLOTS.forall(j =>
        (i != j and ss[i].present and ss[j].present and ss[i].emp == ss[j].emp) implies
          restGapOk(ss[i].startH, ss[i].endH, ss[j].startH, ss[j].endH, minRest)
      )
    )

  pure def shiftsExcludeApprovedLeave(ss: List[Shift], ls: List[Leave]): bool =
    SHIFT_SLOTS.forall(i =>
      LEAVE_SLOTS.forall(k =>
        (ss[i].present and ls[k].status == "approved" and ss[i].emp == ls[k].emp) implies
          not(intervalsOverlap(ss[i].startH, ss[i].endH, ls[k].startH, ls[k].endH))
      )
    )

  action init = all {
    rosterStatus' = "draft",
    hasReceipt' = false,
    minRestHours' = STATUTORY_MIN_REST,
    shifts' = [emptyShift, emptyShift, emptyShift],
    leaves' = [emptyLeave, emptyLeave],
    outcome' = "init",
  }

  // Draft shifts may carry arbitrary candidate solver outputs (including illegal
  // overlaps or rest violations); safety is enforced before `published`.
  action setDraftShift(slot: int, emp: int, startH: int, endH: int): bool = {
    val ok = rosterStatus == "draft"
    val nextShift: Shift = { present: true, emp: emp, startH: startH, endH: endH }
    all {
      rosterStatus' = rosterStatus,
      hasReceipt' = hasReceipt,
      minRestHours' = minRestHours,
      shifts' = if (ok) shifts.replaceAt(slot, nextShift) else shifts,
      leaves' = leaves,
      outcome' = if (ok) "draft_updated" else "not_draft",
    }
  }

  action proposeRoster(receiptOk: bool): bool = {
    val ok = rosterStatus == "draft"
    all {
      rosterStatus' = if (ok) "proposed" else rosterStatus,
      hasReceipt' = if (ok) receiptOk else hasReceipt,
      minRestHours' = minRestHours,
      shifts' = shifts,
      leaves' = leaves,
      outcome' = if (ok) "proposed" else "not_draft",
    }
  }

  action reopenDraft: bool = {
    val ok = rosterStatus == "proposed"
    all {
      rosterStatus' = if (ok) "draft" else rosterStatus,
      hasReceipt' = if (ok) false else hasReceipt,
      minRestHours' = minRestHours,
      shifts' = shifts,
      leaves' = leaves,
      outcome' = if (ok) "reopened" else "not_proposed",
    }
  }

  action publishRoster: bool = {
    val verdict = if (rosterStatus != "proposed") "not_proposed"
      else if (not(hasReceipt)) "missing_solver_receipt"
      else if (minRestHours < STATUTORY_MIN_REST) "below_statutory_floor"
      else if (not(shiftsHalfOpen(shifts))) "invalid_interval"
      else if (not(shiftsNonOverlapping(shifts))) "shift_overlap"
      else if (not(shiftsRespectMinRest(shifts, minRestHours))) "rest_gap_violation"
      else if (not(shiftsExcludeApprovedLeave(shifts, leaves))) "approved_leave_conflict"
      else "commit"
    all {
      rosterStatus' = if (verdict == "commit") "published" else rosterStatus,
      hasReceipt' = hasReceipt,
      minRestHours' = minRestHours,
      shifts' = shifts,
      leaves' = leaves,
      outcome' = verdict,
    }
  }

  action supersedeRoster: bool = {
    val ok = rosterStatus == "published"
    all {
      rosterStatus' = if (ok) "superseded" else rosterStatus,
      hasReceipt' = hasReceipt,
      minRestHours' = minRestHours,
      shifts' = shifts,
      leaves' = leaves,
      outcome' = if (ok) "superseded" else "not_published",
    }
  }

  action approveLeave(slot: int, emp: int, startH: int, endH: int): bool = {
    val nextLeave: Leave = { status: "approved", emp: emp, startH: startH, endH: endH }
    val nextLeaves = leaves.replaceAt(slot, nextLeave)
    val verdict = if (leaves[slot].status == "approved") "already_approved"
      else if (startH >= endH) "invalid_leave_interval"
      else if (rosterStatus == "published" and not(shiftsExcludeApprovedLeave(shifts, nextLeaves))) "conflicts_with_published_shift"
      else "commit"
    all {
      rosterStatus' = rosterStatus,
      hasReceipt' = hasReceipt,
      minRestHours' = minRestHours,
      shifts' = shifts,
      leaves' = if (verdict == "commit") nextLeaves else leaves,
      outcome' = verdict,
    }
  }

  // Statutory rest floor cannot be silently relaxed below `STATUTORY_MIN_REST` (HR-SH-04).
  action setMinRest(newRest: int): bool = {
    val verdict = if (newRest < STATUTORY_MIN_REST) "below_statutory_floor"
      else if (rosterStatus == "published" and not(shiftsRespectMinRest(shifts, newRest))) "conflicts_with_published_roster"
      else "commit"
    all {
      rosterStatus' = rosterStatus,
      hasReceipt' = hasReceipt,
      minRestHours' = if (verdict == "commit") newRest else minRestHours,
      shifts' = shifts,
      leaves' = leaves,
      outcome' = verdict,
    }
  }

  action step = {
    nondet sSlot = SHIFT_SLOTS.oneOf()
    nondet lSlot = LEAVE_SLOTS.oneOf()
    nondet emp = EMPLOYEES.oneOf()
    nondet startH = HOURS.oneOf()
    nondet endH = HOURS.oneOf()
    nondet receiptOk = Set(true, false).oneOf()
    nondet newRest = Set(7, 11, 12).oneOf()
    nondet kind = Set("draft", "propose", "reopen", "publish", "supersede", "leave", "rest").oneOf()
    if (kind == "draft") setDraftShift(sSlot, emp, startH, endH)
    else if (kind == "propose") proposeRoster(receiptOk)
    else if (kind == "reopen") reopenDraft
    else if (kind == "publish") publishRoster
    else if (kind == "supersede") supersedeRoster
    else if (kind == "leave") approveLeave(lSlot, emp, startH, endH)
    else setMinRest(newRest)
  }

  // Negative refutation actions (excluded from guarded `step`):
  // 1. Publishing a proposed roster without overlap / rest-gap / leave checks
  //    refutes `publishedNoOverlap`, `publishedMinRest`, and `publishedNoApprovedLeaveOverlap`.
  // 2. Silently lowering `minRestHours` below 11h refutes `statutoryRestFloorPreserved`.
  action publishRosterUnchecked: bool = {
    val ok = rosterStatus == "proposed" and hasReceipt
    all {
      rosterStatus' = if (ok) "published" else rosterStatus,
      hasReceipt' = hasReceipt,
      minRestHours' = minRestHours,
      shifts' = shifts,
      leaves' = leaves,
      outcome' = if (ok) "unchecked_publish" else "refused",
    }
  }

  action relaxBelowStatutoryFloor(newRest: int): bool = all {
    rosterStatus' = rosterStatus,
    hasReceipt' = hasReceipt,
    minRestHours' = newRest,
    shifts' = shifts,
    leaves' = leaves,
    outcome' = "unchecked_relax",
  }

  action stepUnchecked = {
    nondet badRest = Set(7, 8).oneOf()
    any {
      step,
      publishRosterUnchecked,
      relaxBelowStatutoryFloor(badRest),
    }
  }

  def typeOk = all {
    rosterStatus.in(Set("draft", "proposed", "published", "superseded")),
    shifts.length() == 3,
    leaves.length() == 2,
    SHIFT_SLOTS.forall(i => shifts[i].emp.in(EMPLOYEES)),
    LEAVE_SLOTS.forall(k => leaves[k].status.in(Set("empty", "approved")) and leaves[k].emp.in(EMPLOYEES)),
  }

  // HR-SH-04: statutory minimum rest floor (Egypt 11h) is never relaxed.
  def statutoryRestFloorPreserved = minRestHours >= STATUTORY_MIN_REST

  // hr.Roster.published_requires_receipt: every published roster has a solver receipt.
  def publishedHasReceipt = rosterStatus == "published" implies hasReceipt

  // Every shift in a published roster has a strictly half-open UTC interval [startH, endH).
  def publishedShiftsHalfOpen = rosterStatus == "published" implies shiftsHalfOpen(shifts)

  // HR-SH-01: no published roster contains overlapping shifts for the same employee.
  def publishedNoOverlap = rosterStatus == "published" implies shiftsNonOverlapping(shifts)

  // HR-SH-03: every pair of shifts for the same employee in a published roster
  // respects the active minimum rest gap (at least STATUTORY_MIN_REST = 11h).
  def publishedMinRest =
    rosterStatus == "published" implies shiftsRespectMinRest(shifts, minRestHours)

  // HR-SH-02: no shift in a published roster overlaps an approved leave request.
  def publishedNoApprovedLeaveOverlap =
    rosterStatus == "published" implies shiftsExcludeApprovedLeave(shifts, leaves)
}
