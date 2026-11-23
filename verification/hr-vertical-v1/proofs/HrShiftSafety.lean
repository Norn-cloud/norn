import Std

namespace Norn.Verification.HrShiftSafety

/-!
HR Shift Safety (`INV-SHIFT-SAFETY`, `HR-SH-01..04`).

This is a mathematical reference for `hr.Roster`, `hr.ShiftCommitment`,
`hr.LeaveRequest`, and `loc_eg.LaborProfile` (`min_rest_hours >= 11`). It has no
axioms, `sorry`, or `admit`. Every theorem is about this reference transition
system over unbounded exact integer UTC timestamps; it is not a refinement proof
for the Rust kernel, external CP-SAT solver, or SQLite storage.

Laws proved for every reachable state of the guarded transition system:
* `HR-SH-01`: no published roster contains overlapping shifts for the same employee.
* `HR-SH-02`: no shift in a published roster overlaps an approved leave request
  for the same employee.
* `HR-SH-03`: every pair of distinct shifts for the same employee in a published
  roster satisfies the active minimum rest gap (`>= statutoryMinRest`, e.g. 11h).
* `HR-SH-04`: the active minimum rest floor never drops below `statutoryMinRest`,
  and every published roster carries an accepted solver receipt digest.
-/

structure ShiftCommitment where
  id : Nat
  employee : Nat
  startUtc : Int
  endUtc : Int
  deriving DecidableEq

structure LeaveRequest where
  id : Nat
  employee : Nat
  startUtc : Int
  endUtc : Int
  approved : Bool
  deriving DecidableEq

def intervalsOverlap (s1 e1 s2 e2 : Int) : Prop :=
  s1 < e2 ∧ s2 < e1

def intervalsOverlapBool (s1 e1 s2 e2 : Int) : Bool :=
  decide (s1 < e2) && decide (s2 < e1)

def restGapRespected (s1 e1 s2 e2 minRest : Int) : Prop :=
  e1 + minRest ≤ s2 ∨ e2 + minRest ≤ s1

def restGapRespectedBool (s1 e1 s2 e2 minRest : Int) : Bool :=
  decide (e1 + minRest ≤ s2) || decide (e2 + minRest ≤ s1)

theorem intervalsOverlapBool_iff (s1 e1 s2 e2 : Int) :
    intervalsOverlapBool s1 e1 s2 e2 = true ↔ intervalsOverlap s1 e1 s2 e2 := by
  simp [intervalsOverlapBool, intervalsOverlap]

theorem restGapRespectedBool_iff (s1 e1 s2 e2 minRest : Int) :
    restGapRespectedBool s1 e1 s2 e2 minRest = true ↔
      restGapRespected s1 e1 s2 e2 minRest := by
  simp [restGapRespectedBool, restGapRespected]

/-- For a non-negative rest floor, respecting the minimum rest gap strictly implies
non-overlap (`HR-SH-03` implies `HR-SH-01`). -/
theorem restGap_implies_no_overlap
    {s1 e1 s2 e2 minRest : Int}
    (hmin : 0 ≤ minRest)
    (hgap : restGapRespected s1 e1 s2 e2 minRest) :
    ¬ intervalsOverlap s1 e1 s2 e2 := by
  intro hov
  rcases hov with ⟨h1, h2⟩
  rcases hgap with hle | hle <;> omega

def ShiftsHalfOpen (shifts : List ShiftCommitment) : Prop :=
  ∀ s ∈ shifts, s.startUtc < s.endUtc

def ShiftsNonOverlapping (shifts : List ShiftCommitment) : Prop :=
  ∀ a ∈ shifts, ∀ b ∈ shifts,
    a.id ≠ b.id → a.employee = b.employee →
    ¬ intervalsOverlap a.startUtc a.endUtc b.startUtc b.endUtc

def ShiftsRespectMinRest (shifts : List ShiftCommitment) (minRest : Int) : Prop :=
  ∀ a ∈ shifts, ∀ b ∈ shifts,
    a.id ≠ b.id → a.employee = b.employee →
    restGapRespected a.startUtc a.endUtc b.startUtc b.endUtc minRest

def ShiftsExcludeApprovedLeave
    (shifts : List ShiftCommitment) (leaves : List LeaveRequest) : Prop :=
  ∀ s ∈ shifts, ∀ l ∈ leaves,
    l.approved = true → s.employee = l.employee →
    ¬ intervalsOverlap s.startUtc s.endUtc l.startUtc l.endUtc

def checkShiftsHalfOpen (shifts : List ShiftCommitment) : Bool :=
  shifts.all (fun s => decide (s.startUtc < s.endUtc))

def checkPairNonOverlapping (a b : ShiftCommitment) : Bool :=
  if a.id = b.id then true
  else if a.employee = b.employee then
    !(intervalsOverlapBool a.startUtc a.endUtc b.startUtc b.endUtc)
  else true

def checkShiftsNonOverlapping (shifts : List ShiftCommitment) : Bool :=
  shifts.all (fun a => shifts.all (fun b => checkPairNonOverlapping a b))

def checkPairMinRest (minRest : Int) (a b : ShiftCommitment) : Bool :=
  if a.id = b.id then true
  else if a.employee = b.employee then
    restGapRespectedBool a.startUtc a.endUtc b.startUtc b.endUtc minRest
  else true

def checkShiftsRespectMinRest (shifts : List ShiftCommitment) (minRest : Int) : Bool :=
  shifts.all (fun a => shifts.all (fun b => checkPairMinRest minRest a b))

def checkShiftLeave (s : ShiftCommitment) (l : LeaveRequest) : Bool :=
  if l.approved = true then
    if s.employee = l.employee then
      !(intervalsOverlapBool s.startUtc s.endUtc l.startUtc l.endUtc)
    else true
  else true

def checkShiftsExcludeApprovedLeave
    (shifts : List ShiftCommitment) (leaves : List LeaveRequest) : Bool :=
  shifts.all (fun s => leaves.all (fun l => checkShiftLeave s l))

theorem checkShiftsHalfOpen_iff (shifts : List ShiftCommitment) :
    checkShiftsHalfOpen shifts = true ↔ ShiftsHalfOpen shifts := by
  constructor
  · intro h s hs
    exact of_decide_eq_true (List.all_eq_true.mp h s hs)
  · intro h
    exact List.all_eq_true.mpr (fun s hs => decide_eq_true (h s hs))

theorem checkPairNonOverlapping_iff (a b : ShiftCommitment) :
    checkPairNonOverlapping a b = true ↔
      (a.id ≠ b.id → a.employee = b.employee →
        ¬ intervalsOverlap a.startUtc a.endUtc b.startUtc b.endUtc) := by
  unfold checkPairNonOverlapping
  by_cases hid : a.id = b.id
  · rw [if_pos hid]
    constructor
    · intro _ hne
      exact absurd hid hne
    · intro _
      rfl
  · rw [if_neg hid]
    by_cases hemp : a.employee = b.employee
    · rw [if_pos hemp]
      constructor
      · intro h _ _ hov
        have ht := (intervalsOverlapBool_iff a.startUtc a.endUtc b.startUtc b.endUtc).mpr hov
        rw [ht] at h
        contradiction
      · intro h
        cases hb : intervalsOverlapBool a.startUtc a.endUtc b.startUtc b.endUtc
        · rfl
        · exact absurd ((intervalsOverlapBool_iff _ _ _ _).mp hb) (h hid hemp)
    · rw [if_neg hemp]
      constructor
      · intro _ _ heq
        exact absurd heq hemp
      · intro _
        rfl

theorem checkShiftsNonOverlapping_iff (shifts : List ShiftCommitment) :
    checkShiftsNonOverlapping shifts = true ↔ ShiftsNonOverlapping shifts := by
  constructor
  · intro h a ha b hb
    have hrow := List.all_eq_true.mp h a ha
    have hpair := List.all_eq_true.mp hrow b hb
    exact (checkPairNonOverlapping_iff a b).mp hpair
  · intro h
    exact List.all_eq_true.mpr (fun a ha =>
      List.all_eq_true.mpr (fun b hb =>
        (checkPairNonOverlapping_iff a b).mpr (h a ha b hb)))

theorem checkPairMinRest_iff (minRest : Int) (a b : ShiftCommitment) :
    checkPairMinRest minRest a b = true ↔
      (a.id ≠ b.id → a.employee = b.employee →
        restGapRespected a.startUtc a.endUtc b.startUtc b.endUtc minRest) := by
  unfold checkPairMinRest
  by_cases hid : a.id = b.id
  · rw [if_pos hid]
    constructor
    · intro _ hne
      exact absurd hid hne
    · intro _
      rfl
  · rw [if_neg hid]
    by_cases hemp : a.employee = b.employee
    · rw [if_pos hemp]
      constructor
      · intro h _ _
        exact (restGapRespectedBool_iff a.startUtc a.endUtc b.startUtc b.endUtc minRest).mp h
      · intro h
        exact (restGapRespectedBool_iff a.startUtc a.endUtc b.startUtc b.endUtc minRest).mpr (h hid hemp)
    · rw [if_neg hemp]
      constructor
      · intro _ _ heq
        exact absurd heq hemp
      · intro _
        rfl

theorem checkShiftsRespectMinRest_iff (shifts : List ShiftCommitment) (minRest : Int) :
    checkShiftsRespectMinRest shifts minRest = true ↔ ShiftsRespectMinRest shifts minRest := by
  constructor
  · intro h a ha b hb
    have hrow := List.all_eq_true.mp h a ha
    have hpair := List.all_eq_true.mp hrow b hb
    exact (checkPairMinRest_iff minRest a b).mp hpair
  · intro h
    exact List.all_eq_true.mpr (fun a ha =>
      List.all_eq_true.mpr (fun b hb =>
        (checkPairMinRest_iff minRest a b).mpr (h a ha b hb)))

theorem checkShiftLeave_iff (s : ShiftCommitment) (l : LeaveRequest) :
    checkShiftLeave s l = true ↔
      (l.approved = true → s.employee = l.employee →
        ¬ intervalsOverlap s.startUtc s.endUtc l.startUtc l.endUtc) := by
  unfold checkShiftLeave
  by_cases happ : l.approved = true
  · rw [if_pos happ]
    by_cases hemp : s.employee = l.employee
    · rw [if_pos hemp]
      constructor
      · intro h _ _ hov
        have ht := (intervalsOverlapBool_iff s.startUtc s.endUtc l.startUtc l.endUtc).mpr hov
        rw [ht] at h
        contradiction
      · intro h
        cases hb : intervalsOverlapBool s.startUtc s.endUtc l.startUtc l.endUtc
        · rfl
        · exact absurd ((intervalsOverlapBool_iff _ _ _ _).mp hb) (h happ hemp)
    · rw [if_neg hemp]
      constructor
      · intro _ _ heq
        exact absurd heq hemp
      · intro _
        rfl
  · rw [if_neg happ]
    constructor
    · intro _ htr
      exact absurd htr happ
    · intro _
      rfl

theorem checkShiftsExcludeApprovedLeave_iff
    (shifts : List ShiftCommitment) (leaves : List LeaveRequest) :
    checkShiftsExcludeApprovedLeave shifts leaves = true ↔
      ShiftsExcludeApprovedLeave shifts leaves := by
  constructor
  · intro h s hs l hl
    have hrow := List.all_eq_true.mp h s hs
    have hpair := List.all_eq_true.mp hrow l hl
    exact (checkShiftLeave_iff s l).mp hpair
  · intro h
    exact List.all_eq_true.mpr (fun s hs =>
      List.all_eq_true.mpr (fun l hl =>
        (checkShiftLeave_iff s l).mpr (h s hs l hl)))

inductive RosterStatus where
  | draft
  | proposed
  | published
  | superseded
  deriving DecidableEq

structure State where
  status : RosterStatus
  solverReceipt : Option Nat
  statutoryMinRest : Int
  minRestHours : Int
  shifts : List ShiftCommitment
  leaves : List LeaveRequest
  deriving DecidableEq

def initState (statutoryMinRest : Int) : State := {
  status := .draft
  solverReceipt := none
  statutoryMinRest := statutoryMinRest
  minRestHours := statutoryMinRest
  shifts := []
  leaves := []
}

def Safe (st : State) : Prop :=
  0 ≤ st.statutoryMinRest ∧
    st.statutoryMinRest ≤ st.minRestHours ∧
    (st.status = .published →
      st.solverReceipt ≠ none ∧
      ShiftsHalfOpen st.shifts ∧
      ShiftsNonOverlapping st.shifts ∧
      ShiftsRespectMinRest st.shifts st.minRestHours ∧
      ShiftsExcludeApprovedLeave st.shifts st.leaves)

inductive Op where
  | setDraftShifts (shifts : List ShiftCommitment)
  | propose (receipt : Option Nat)
  | reopenDraft
  | publish
  | supersede
  | approveLeave (leave : LeaveRequest)
  | setMinRest (newMinRest : Int)
  deriving DecidableEq

def step (st : State) : Op → Option State
  | .setDraftShifts newShifts =>
      if st.status = .draft then
        some { st with shifts := newShifts }
      else none
  | .propose receipt =>
      if st.status = .draft then
        some { st with status := .proposed, solverReceipt := receipt }
      else none
  | .reopenDraft =>
      if st.status = .proposed then
        some { st with status := .draft, solverReceipt := none }
      else none
  | .publish =>
      if st.status = .proposed ∧
          st.solverReceipt ≠ none ∧
          checkShiftsHalfOpen st.shifts = true ∧
          checkShiftsNonOverlapping st.shifts = true ∧
          checkShiftsRespectMinRest st.shifts st.minRestHours = true ∧
          checkShiftsExcludeApprovedLeave st.shifts st.leaves = true then
        some { st with status := .published }
      else none
  | .supersede =>
      if st.status = .published then
        some { st with status := .superseded }
      else none
  | .approveLeave leave =>
      if leave.approved = true ∧ leave.startUtc < leave.endUtc then
        if st.status = .published then
          if checkShiftsExcludeApprovedLeave st.shifts [leave] = true then
            some { st with leaves := leave :: st.leaves }
          else none
        else
          some { st with leaves := leave :: st.leaves }
      else none
  | .setMinRest newMinRest =>
      if st.statutoryMinRest ≤ newMinRest then
        if st.status = .published then
          if checkShiftsRespectMinRest st.shifts newMinRest = true then
            some { st with minRestHours := newMinRest }
          else none
        else
          some { st with minRestHours := newMinRest }
      else none

def exec : State → List Op → Option State
  | st, [] => some st
  | st, op :: ops =>
      match step st op with
      | none => none
      | some next => exec next ops

theorem init_safe {statutoryMinRest : Int} (hnonneg : 0 ≤ statutoryMinRest) :
    Safe (initState statutoryMinRest) := by
  refine ⟨hnonneg, by simp [initState], ?_⟩
  intro hpub
  simp [initState] at hpub

/-- Every admitted roster/leave operation preserves `Safe` (`INV-SHIFT-SAFETY`). -/
theorem step_preserves_safe {st next : State} {op : Op}
    (hsafe : Safe st) (hstep : step st op = some next) : Safe next := by
  rcases hsafe with ⟨hstat, hfloor, hpub⟩
  cases op with
  | setDraftShifts newShifts =>
      simp only [step] at hstep
      split at hstep
      · rename_i hdraft
        rw [← Option.some.inj hstep]
        refine ⟨hstat, hfloor, ?_⟩
        intro h
        dsimp only at h
        rw [hdraft] at h
        contradiction
      · contradiction
  | propose receipt =>
      simp only [step] at hstep
      split at hstep
      · rw [← Option.some.inj hstep]
        refine ⟨hstat, hfloor, ?_⟩
        intro h
        dsimp only at h
        contradiction
      · contradiction
  | reopenDraft =>
      simp only [step] at hstep
      split at hstep
      · rw [← Option.some.inj hstep]
        refine ⟨hstat, hfloor, ?_⟩
        intro h
        dsimp only at h
        contradiction
      · contradiction
  | publish =>
      simp only [step] at hstep
      split at hstep
      · rename_i guard
        rw [← Option.some.inj hstep]
        refine ⟨hstat, hfloor, ?_⟩
        intro _
        dsimp only
        exact ⟨
          guard.2.1,
          (checkShiftsHalfOpen_iff st.shifts).mp guard.2.2.1,
          (checkShiftsNonOverlapping_iff st.shifts).mp guard.2.2.2.1,
          (checkShiftsRespectMinRest_iff st.shifts st.minRestHours).mp guard.2.2.2.2.1,
          (checkShiftsExcludeApprovedLeave_iff st.shifts st.leaves).mp guard.2.2.2.2.2
        ⟩
      · contradiction
  | supersede =>
      simp only [step] at hstep
      split at hstep
      · rw [← Option.some.inj hstep]
        refine ⟨hstat, hfloor, ?_⟩
        intro h
        dsimp only at h
        contradiction
      · contradiction
  | approveLeave leave =>
      simp only [step] at hstep
      split at hstep
      · split at hstep
        · rename_i hispub
          split at hstep
          · rename_i hchk
            rw [← Option.some.inj hstep]
            have hold := hpub hispub
            have hnew := (checkShiftsExcludeApprovedLeave_iff st.shifts [leave]).mp hchk
            refine ⟨hstat, hfloor, ?_⟩
            intro _
            dsimp only
            refine ⟨hold.1, hold.2.1, hold.2.2.1, hold.2.2.2.1, ?_⟩
            intro s hs l hl
            rcases List.mem_cons.mp hl with rfl | hl
            · exact hnew s hs l (by simp)
            · exact hold.2.2.2.2 s hs l hl
          · contradiction
        · rename_i hnotpub
          rw [← Option.some.inj hstep]
          refine ⟨hstat, hfloor, ?_⟩
          intro h
          dsimp only at h
          exact absurd h hnotpub
      · contradiction
  | setMinRest newMinRest =>
      simp only [step] at hstep
      split at hstep
      · rename_i hge
        split at hstep
        · rename_i hispub
          split at hstep
          · rename_i hchk
            rw [← Option.some.inj hstep]
            have hold := hpub hispub
            refine ⟨hstat, hge, ?_⟩
            intro _
            dsimp only
            exact ⟨
              hold.1,
              hold.2.1,
              hold.2.2.1,
              (checkShiftsRespectMinRest_iff st.shifts newMinRest).mp hchk,
              hold.2.2.2.2
            ⟩
          · contradiction
        · rename_i hnotpub
          rw [← Option.some.inj hstep]
          refine ⟨hstat, hge, ?_⟩
          intro h
          dsimp only at h
          exact absurd h hnotpub
      · contradiction

/-- Inductive safety: every workflow starting from a safe state preserves `Safe`. -/
theorem run_preserves_safe :
    ∀ {st next : State} {ops : List Op},
      Safe st → exec st ops = some next → Safe next
  | st, next, [], hsafe, h => by
      simp only [exec] at h
      rw [← Option.some.inj h]
      exact hsafe
  | st, next, op :: ops, hsafe, h => by
      unfold exec at h
      split at h
      · contradiction
      · rename_i mid hmid
        exact run_preserves_safe (step_preserves_safe hsafe hmid) h

/-- `HR-SH-01`: publishing a roster containing overlapping shifts is refused. -/
theorem publish_refuses_overlapping_shifts {st : State}
    (hbad : ¬ ShiftsNonOverlapping st.shifts) :
    step st .publish = none := by
  simp only [step]
  split
  · rename_i guard
    exact absurd ((checkShiftsNonOverlapping_iff st.shifts).mp guard.2.2.2.1) hbad
  · rfl

/-- `HR-SH-02`: publishing a roster with a shift overlapping approved leave is refused. -/
theorem publish_refuses_approved_leave_overlap {st : State}
    (hbad : ¬ ShiftsExcludeApprovedLeave st.shifts st.leaves) :
    step st .publish = none := by
  simp only [step]
  split
  · rename_i guard
    exact absurd ((checkShiftsExcludeApprovedLeave_iff st.shifts st.leaves).mp guard.2.2.2.2.2) hbad
  · rfl

/-- `HR-SH-03`: publishing a roster that violates the minimum rest gap is refused. -/
theorem publish_refuses_rest_violation {st : State}
    (hbad : ¬ ShiftsRespectMinRest st.shifts st.minRestHours) :
    step st .publish = none := by
  simp only [step]
  split
  · rename_i guard
    exact absurd ((checkShiftsRespectMinRest_iff st.shifts st.minRestHours).mp guard.2.2.2.2.1) hbad
  · rfl

/-- `hr.Roster.published_requires_receipt`: publishing without a solver receipt refuses. -/
theorem publish_refuses_missing_receipt {st : State}
    (hnone : st.solverReceipt = none) :
    step st .publish = none := by
  simp only [step]
  split
  · rename_i guard
    exact absurd hnone guard.2.1
  · rfl

/-- `HR-SH-04`: silently relaxing `minRestHours` below `statutoryMinRest` is refused. -/
theorem relax_below_statutory_floor_refused {st : State} {newMinRest : Int}
    (hlow : newMinRest < st.statutoryMinRest) :
    step st (.setMinRest newMinRest) = none := by
  simp only [step]
  split
  · rename_i hge
    omega
  · rfl

/-- Egypt `HR-SH-03` concrete witness: closing shift `[15, 23)` followed by opening
shift `[30, 38)` (06:00 next day, 7h gap) violates the 11-hour statutory floor. -/
theorem egypt_clopening_7h_violates_11h_floor :
    ¬ restGapRespected 15 23 30 38 11 := by
  unfold restGapRespected
  omega

end Norn.Verification.HrShiftSafety
