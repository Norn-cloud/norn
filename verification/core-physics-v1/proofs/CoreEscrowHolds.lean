import Std

namespace Norn.Verification.CoreEscrowHolds

/-!
`norn.core` law for escrow / leave-hold conservation (`core.law.escrow_non_negative`).

This is a mathematical reference for `core.Hold` and `hr.LeaveHold` (`after: { days: 7 }`).
It has no axioms, `sorry`, or `admit`. Every theorem is about this reference model;
it is not a refinement proof for the Rust kernel, checked `i128` arithmetic,
serialization, or Turso storage.

Laws proved for every reachable state of the guarded transition system:
* `core.law.escrow_non_negative`: every hold amount is non-negative, the sum of
  active holds never exceeds the backing balance, and available quantity is never
  negative (`available acct ≥ 0`).
* Concurrent overcommitting holds (`HR-LV-04`, e.g. balance 3 with two 2-day
  requests) cannot both commit under serialized admission.
* Expiry holds only once the 7-day timer deadline (`expiresAt ≤ now`) is reached;
  premature expiry is refused.
* Terminal holds (`consumed`, `released`, `expired`) reject every second resolution.
-/

inductive HoldStatus where
  | active
  | consumed
  | released
  | expired
  deriving DecidableEq

def HoldStatus.isTerminal : HoldStatus → Bool
  | .active => false
  | .consumed | .released | .expired => true

structure Hold where
  id : Nat
  amount : Int
  createdAt : Nat
  expiresAt : Nat
  status : HoldStatus
  deriving DecidableEq

/-- Standard 7-day hold expiry window (`hr.LeaveHold` `after: { days: 7 }`). -/
def holdTtlDays : Nat := 7

structure Account where
  now : Nat
  balance : Int
  holds : List Hold
  deriving DecidableEq

def initAccount : Account := {
  now := 0
  balance := 0
  holds := []
}

def activeHoldSum : List Hold → Int
  | [] => 0
  | h :: hs => (if h.status = .active then h.amount else 0) + activeHoldSum hs

def available (acct : Account) : Int :=
  acct.balance - activeHoldSum acct.holds

def idExists (id : Nat) : List Hold → Bool
  | [] => false
  | h :: hs => (h.id == id) || idExists id hs

def lookupHold (id : Nat) : List Hold → Option Hold
  | [] => none
  | h :: hs => if h.id = id then some h else lookupHold id hs

def setHoldStatus (id : Nat) (newStatus : HoldStatus) : List Hold → List Hold
  | [] => []
  | h :: hs =>
      if h.id = id then { h with status := newStatus } :: hs
      else h :: setHoldStatus id newStatus hs

def AllHoldsNonNeg (hs : List Hold) : Prop :=
  ∀ h ∈ hs, 0 ≤ h.amount

def Safe (acct : Account) : Prop :=
  0 ≤ acct.balance ∧
    AllHoldsNonNeg acct.holds ∧
    activeHoldSum acct.holds ≤ acct.balance

theorem lookupHold_mem (id : Nat) :
    ∀ {hs : List Hold} {h : Hold}, lookupHold id hs = some h → h ∈ hs
  | [], _, hlook => by simp [lookupHold] at hlook
  | x :: xs, h, hlook => by
      unfold lookupHold at hlook
      by_cases hid : x.id = id
      · rw [if_pos hid] at hlook
        have heq : x = h := Option.some.inj hlook
        subst heq
        simp
      · rw [if_neg hid] at hlook
        exact List.mem_cons_of_mem x (lookupHold_mem id hlook)

theorem activeHoldSum_nonneg :
    ∀ {hs : List Hold}, AllHoldsNonNeg hs → 0 ≤ activeHoldSum hs
  | [], _ => by
      simp only [activeHoldSum]
      omega
  | h :: hs, hnn => by
      have hh : 0 ≤ h.amount := hnn h (by simp)
      have hrest : 0 ≤ activeHoldSum hs :=
        activeHoldSum_nonneg (fun x hx => hnn x (by simp [hx]))
      simp only [activeHoldSum]
      by_cases hact : h.status = .active
      · rw [if_pos hact]
        omega
      · rw [if_neg hact]
        omega

theorem activeHoldSum_setHoldStatus_deactivate (id : Nat) (newStatus : HoldStatus)
    (hne : newStatus ≠ .active) :
    ∀ (hs : List Hold) (target : Hold),
      lookupHold id hs = some target →
      target.status = .active →
      activeHoldSum (setHoldStatus id newStatus hs) = activeHoldSum hs - target.amount
  | [], _, hlook, _ => by simp [lookupHold] at hlook
  | h :: hs, target, hlook, hact => by
      unfold lookupHold at hlook
      by_cases hid : h.id = id
      · rw [if_pos hid] at hlook
        have heq : h = target := Option.some.inj hlook
        subst heq
        simp only [setHoldStatus]
        rw [if_pos hid]
        simp only [activeHoldSum]
        dsimp only
        rw [if_neg hne, if_pos hact]
        omega
      · rw [if_neg hid] at hlook
        have ih := activeHoldSum_setHoldStatus_deactivate id newStatus hne hs target hlook hact
        simp only [setHoldStatus]
        rw [if_neg hid]
        simp only [activeHoldSum]
        rw [ih]
        omega

theorem setHoldStatus_preserves_nonneg (id : Nat) (newStatus : HoldStatus) :
    ∀ {hs : List Hold}, AllHoldsNonNeg hs → AllHoldsNonNeg (setHoldStatus id newStatus hs)
  | [], _ => fun _ hx => by simp [setHoldStatus] at hx
  | h :: hs, hnn => by
      intro x hx
      simp only [setHoldStatus] at hx
      by_cases hid : h.id = id
      · rw [if_pos hid] at hx
        rcases List.mem_cons.mp hx with rfl | hx
        · exact hnn h (by simp)
        · exact hnn x (by simp [hx])
      · rw [if_neg hid] at hx
        rcases List.mem_cons.mp hx with rfl | hx
        · exact hnn h (by simp)
        · exact setHoldStatus_preserves_nonneg id newStatus
            (fun y hy => hnn y (by simp [hy])) x hx

theorem safe_implies_available_nonneg {acct : Account} (hsafe : Safe acct) :
    0 ≤ available acct := by
  rcases hsafe with ⟨_, _, hbound⟩
  unfold available
  omega

inductive Op where
  | accrue (delta : Int)
  | placeHold (id : Nat) (amount : Int)
  | consumeHold (id : Nat)
  | releaseHold (id : Nat)
  | expireHold (id : Nat)
  | advanceClock (delta : Nat)
  deriving DecidableEq

def step (acct : Account) : Op → Option Account
  | .accrue delta =>
      if 0 < delta then
        some { acct with balance := acct.balance + delta }
      else none
  | .placeHold id amount =>
      if idExists id acct.holds = false ∧ 0 < amount ∧ amount ≤ available acct then
        let h : Hold := {
          id := id
          amount := amount
          createdAt := acct.now
          expiresAt := acct.now + holdTtlDays
          status := .active
        }
        some { acct with holds := h :: acct.holds }
      else none
  | .consumeHold id =>
      match lookupHold id acct.holds with
      | none => none
      | some h =>
          if h.status = .active ∧ h.amount ≤ acct.balance then
            some {
              acct with
              balance := acct.balance - h.amount
              holds := setHoldStatus id .consumed acct.holds
            }
          else none
  | .releaseHold id =>
      match lookupHold id acct.holds with
      | none => none
      | some h =>
          if h.status = .active then
            some { acct with holds := setHoldStatus id .released acct.holds }
          else none
  | .expireHold id =>
      match lookupHold id acct.holds with
      | none => none
      | some h =>
          if h.status = .active ∧ h.expiresAt ≤ acct.now then
            some { acct with holds := setHoldStatus id .expired acct.holds }
          else none
  | .advanceClock delta =>
      some { acct with now := acct.now + delta }

def exec : Account → List Op → Option Account
  | acct, [] => some acct
  | acct, op :: ops =>
      match step acct op with
      | none => none
      | some next => exec next ops

theorem init_safe : Safe initAccount := by
  refine ⟨by decide, ?_, by simp [initAccount, activeHoldSum]⟩
  intro _ hx
  simp [initAccount] at hx

/-- `core.law.escrow_non_negative`, one step: every admitted operation preserves
non-negative balance, non-negative holds, and `activeHoldSum ≤ balance`. -/
theorem step_preserves_safe {acct next : Account} {op : Op}
    (hsafe : Safe acct) (hstep : step acct op = some next) : Safe next := by
  rcases hsafe with ⟨hbal, hnn, hbound⟩
  cases op with
  | accrue delta =>
      simp only [step] at hstep
      split at hstep
      · rename_i hpos
        rw [← Option.some.inj hstep]
        refine ⟨?_, hnn, ?_⟩
        · dsimp only
          omega
        · dsimp only
          omega
      · contradiction
  | placeHold id amount =>
      simp only [step] at hstep
      split at hstep
      · rename_i guard
        rw [← Option.some.inj hstep]
        have hpos : 0 < amount := guard.2.1
        have hfit : amount ≤ acct.balance - activeHoldSum acct.holds := guard.2.2
        refine ⟨hbal, ?_, ?_⟩
        · intro x hx
          rcases List.mem_cons.mp hx with rfl | hx
          · dsimp only
            omega
          · exact hnn x hx
        · dsimp only
          simp only [activeHoldSum]
          dsimp only
          rw [if_pos rfl]
          omega
      · contradiction
  | consumeHold id =>
      simp only [step] at hstep
      split at hstep
      · contradiction
      · rename_i target hlook
        split at hstep
        · rename_i guard
          rw [← Option.some.inj hstep]
          have hle : target.amount ≤ acct.balance := guard.2
          have hsum := activeHoldSum_setHoldStatus_deactivate id .consumed
            (by decide) acct.holds target hlook guard.1
          refine ⟨?_, setHoldStatus_preserves_nonneg id .consumed hnn, ?_⟩
          · dsimp only
            omega
          · dsimp only
            rw [hsum]
            omega
        · contradiction
  | releaseHold id =>
      simp only [step] at hstep
      split at hstep
      · contradiction
      · rename_i target hlook
        split at hstep
        · rename_i hact
          rw [← Option.some.inj hstep]
          have hsum := activeHoldSum_setHoldStatus_deactivate id .released
            (by decide) acct.holds target hlook hact
          have htnn : 0 ≤ target.amount := hnn target (lookupHold_mem id hlook)
          refine ⟨hbal, setHoldStatus_preserves_nonneg id .released hnn, ?_⟩
          dsimp only
          rw [hsum]
          omega
        · contradiction
  | expireHold id =>
      simp only [step] at hstep
      split at hstep
      · contradiction
      · rename_i target hlook
        split at hstep
        · rename_i guard
          rw [← Option.some.inj hstep]
          have hsum := activeHoldSum_setHoldStatus_deactivate id .expired
            (by decide) acct.holds target hlook guard.1
          have htnn : 0 ≤ target.amount := hnn target (lookupHold_mem id hlook)
          refine ⟨hbal, setHoldStatus_preserves_nonneg id .expired hnn, ?_⟩
          dsimp only
          rw [hsum]
          omega
        · contradiction
  | advanceClock _ =>
      simp only [step] at hstep
      rw [← Option.some.inj hstep]
      exact ⟨hbal, hnn, hbound⟩

/-- Inductive safety: any workflow of admitted escrow/hold operations preserves `Safe`. -/
theorem run_preserves_safe :
    ∀ {acct next : Account} {ops : List Op},
      Safe acct → exec acct ops = some next → Safe next
  | acct, next, [], hsafe, h => by
      simp only [exec] at h
      rw [← Option.some.inj h]
      exact hsafe
  | acct, next, op :: ops, hsafe, h => by
      unfold exec at h
      split at h
      · contradiction
      · rename_i mid hmid
        exact run_preserves_safe (step_preserves_safe hsafe hmid) h

/-- Every workflow from the empty account keeps available quantity non-negative. -/
theorem run_available_nonneg {next : Account} {ops : List Op}
    (h : exec initAccount ops = some next) :
    0 ≤ available next :=
  safe_implies_available_nonneg (run_preserves_safe init_safe h)

/-- A hold exceeding currently available quantity is refused. -/
theorem overdraw_hold_refused {acct : Account} {id : Nat} {amount : Int}
    (hover : available acct < amount) :
    step acct (.placeHold id amount) = none := by
  simp only [step]
  split
  · rename_i guard
    have : amount ≤ available acct := guard.2.2
    omega
  · rfl

/-- Concurrent overcommitting holds (`HR-LV-04`): if two requests individually fit
`available acct` but together exceed it (`available acct < a1 + a2`), admitting the
first forces the second to be refused. -/
theorem concurrent_overcommitting_holds_cannot_both_admit
    {acct mid : Account} {id1 id2 : Nat} {a1 a2 : Int}
    (hfirst : step acct (.placeHold id1 a1) = some mid)
    (hover : available acct < a1 + a2) :
    step mid (.placeHold id2 a2) = none := by
  simp only [step] at hfirst
  split at hfirst
  · have hmid : mid = {
      acct with
      holds := {
        id := id1
        amount := a1
        createdAt := acct.now
        expiresAt := acct.now + holdTtlDays
        status := .active
      } :: acct.holds
    } := (Option.some.inj hfirst).symm
    apply overdraw_hold_refused
    rw [hmid]
    unfold available at hover ⊢
    dsimp only
    simp only [activeHoldSum]
    dsimp only
    rw [if_pos rfl]
    omega
  · contradiction

/-- Premature expiry before the 7-day timer deadline (`now < expiresAt`) is refused. -/
theorem premature_expiry_refused {acct : Account} {id : Nat} {h : Hold}
    (hlook : lookupHold id acct.holds = some h)
    (hearly : acct.now < h.expiresAt) :
    step acct (.expireHold id) = none := by
  simp only [step, hlook]
  split
  · rename_i guard
    have : h.expiresAt ≤ acct.now := guard.2
    omega
  · rfl

/-- A hold that is already terminal cannot be consumed, released, or expired again. -/
theorem terminal_hold_rejects_second_resolution {acct : Account} {id : Nat} {h : Hold}
    (hlook : lookupHold id acct.holds = some h)
    (hterm : h.status.isTerminal = true) :
    step acct (.consumeHold id) = none ∧
      step acct (.releaseHold id) = none ∧
      step acct (.expireHold id) = none := by
  have hne : h.status ≠ .active := by
    intro hact
    rw [hact] at hterm
    contradiction
  refine ⟨?_, ?_, ?_⟩
  · simp only [step, hlook]
    split
    · rename_i guard
      exact absurd guard.1 hne
    · rfl
  · simp only [step, hlook]
    split
    · rename_i hact
      exact absurd hact hne
    · rfl
  · simp only [step, hlook]
    split
    · rename_i guard
      exact absurd guard.1 hne
    · rfl

end Norn.Verification.CoreEscrowHolds
