import Std

namespace Norn.Verification.CoreQuantity

/-!
`norn.core` laws for resources and commitments: quantity conservation, escrow
and quota non-negativity, and commitment completion.

This is a mathematical reference for the `norn.core` physics pack. It has no
axioms, `sorry` or `admit`. Each section models the only operations the pack
exposes for that primitive, and every theorem is about this model. It is not a
refinement proof for the Rust kernel's quantity ledger, its `i128`
arithmetic, its timers or its storage; that correspondence is tested, not
proved.

Laws (ids as cited by the core pack):
* `core.law.quantity_conservation`: for one resource and unit, stock received
  equals stock issued plus stock on hand. This is conservation per resource
  only. It does not couple a transformation's outputs to its inputs: a
  production receipt with no recorded consumption satisfies it, so that
  coupling is unverified here.
* `core.law.escrow_non_negative`: holds (escrow, reservations, quota use) never
  go negative and never exceed the balance, so available stock is never
  negative.
* `core.law.commitment_completion`: a commitment's terminal states (fulfilled,
  cancelled, breached) are absorbing, no operation drops a commitment, every
  transition strictly lowers its rank, and once the due time has passed a
  terminal step is always enabled.
-/

/-! ## Quantity conservation -/

/-- Location `0` is outside the business; every other location holds stock on hand. -/
def outside : Nat := 0

/-- One movement of a quantity of a single resource, in its unit. -/
structure Move where
  source : Nat
  target : Nat
  qty : Nat

/-- Net change of stock held inside the business. -/
def onHandDelta (m : Move) : Int :=
  (if m.target ≠ outside then (m.qty : Int) else 0) - (if m.source ≠ outside then (m.qty : Int) else 0)

def onHand : List Move → Int
  | [] => 0
  | m :: ms => onHandDelta m + onHand ms

/-- Stock entering from outside (purchases, production output). -/
def received : List Move → Int
  | [] => 0
  | m :: ms => (if m.source = outside ∧ m.target ≠ outside then (m.qty : Int) else 0) + received ms

/-- Stock leaving to outside (sales, consumption by a transformation, scrap). -/
def issued : List Move → Int
  | [] => 0
  | m :: ms => (if m.source ≠ outside ∧ m.target = outside then (m.qty : Int) else 0) + issued ms

/-- `core.law.quantity_conservation`: stock in = stock out + on hand, for every movement history. -/
theorem received_eq_issued_add_onHand : ∀ ms : List Move, received ms = issued ms + onHand ms
  | [] => rfl
  | m :: ms => by
      have ih := received_eq_issued_add_onHand ms
      simp only [received, issued, onHand, onHandDelta]
      by_cases hs : m.source = outside <;> by_cases ht : m.target = outside <;> simp [hs, ht] <;> omega

/-- Balance of one location: what moved in minus what moved out. -/
def balanceAt (loc : Nat) : List Move → Int
  | [] => 0
  | m :: ms =>
      (if m.target = loc then (m.qty : Int) else 0) - (if m.source = loc then (m.qty : Int) else 0) + balanceAt loc ms

/-- Every movement is a transfer: what leaves one location arrives at another. -/
theorem move_conserves (m : Move) (ms : List Move) (a b : Nat) (ha : m.source = a) (hb : m.target = b) (hab : a ≠ b) :
    balanceAt a (m :: ms) + balanceAt b (m :: ms) = balanceAt a ms + balanceAt b ms := by
  have hba : b ≠ a := fun h => hab h.symm
  simp [balanceAt, ha, hb, hab, hba] <;> omega

/-! ## Escrow and quota non-negativity -/

/-- One balance partition: the posted balance and the quantity held against it. -/
structure Slot where
  balance : Int
  held : Int

def available (s : Slot) : Int := s.balance - s.held

def Valid (s : Slot) : Prop := 0 ≤ s.held ∧ s.held ≤ s.balance

/-- The only operations on a held balance. Each refuses rather than going negative. -/
inductive QOp where
  | receive (q : Nat)
  | issue (q : Nat)
  | reserve (q : Nat)
  | release (q : Nat)
  | consume (q : Nat)

def qstep (s : Slot) : QOp → Option Slot
  | .receive q => some ⟨s.balance + q, s.held⟩
  | .issue q => if (q : Int) ≤ available s then some ⟨s.balance - q, s.held⟩ else none
  | .reserve q => if (q : Int) ≤ available s then some ⟨s.balance, s.held + q⟩ else none
  | .release q => if (q : Int) ≤ s.held then some ⟨s.balance, s.held - q⟩ else none
  | .consume q => if (q : Int) ≤ s.held then some ⟨s.balance - q, s.held - q⟩ else none

/-- `core.law.escrow_non_negative`, one step. -/
theorem qstep_valid {s next : Slot} {op : QOp} (hs : Valid s) (h : qstep s op = some next) : Valid next := by
  obtain ⟨h0, h1⟩ := hs
  cases op with
  | receive q =>
      simp only [qstep, Option.some.injEq] at h
      subst h
      simp only [Valid]
      exact ⟨by omega, by omega⟩
  | issue q =>
      simp only [qstep] at h
      split at h
      · rename_i hq
        simp only [Option.some.injEq] at h
        subst h
        simp only [Valid]
        simp only [available] at hq
        exact ⟨by omega, by omega⟩
      · contradiction
  | reserve q =>
      simp only [qstep] at h
      split at h
      · rename_i hq
        simp only [Option.some.injEq] at h
        subst h
        simp only [Valid]
        simp only [available] at hq
        exact ⟨by omega, by omega⟩
      · contradiction
  | release q =>
      simp only [qstep] at h
      split at h
      · rename_i hq
        simp only [Option.some.injEq] at h
        subst h
        simp only [Valid]
        exact ⟨by omega, by omega⟩
      · contradiction
  | consume q =>
      simp only [qstep] at h
      split at h
      · rename_i hq
        simp only [Option.some.injEq] at h
        subst h
        simp only [Valid]
        exact ⟨by omega, by omega⟩
      · contradiction

def qexec : Slot → List QOp → Option Slot
  | s, [] => some s
  | s, op :: ops =>
      match qstep s op with
      | none => none
      | some next => qexec next ops

/-- Inheritance: any workflow over holds keeps every partition valid. -/
theorem qexec_valid : ∀ {s next : Slot} {ops : List QOp}, Valid s → qexec s ops = some next → Valid next
  | s, next, [], hs, h => by
      simp only [qexec, Option.some.injEq] at h
      subst h
      exact hs
  | s, next, op :: ops, hs, h => by
      unfold qexec at h
      split at h
      · contradiction
      · rename_i mid hmid
        exact qexec_valid (qstep_valid hs hmid) h

theorem valid_available_non_negative {s : Slot} (h : Valid s) : 0 ≤ available s := by
  obtain ⟨_, h1⟩ := h
  simp only [available]
  omega

/-! ## Commitment completion -/

inductive CState where
  | requested
  | promised
  | fulfilled
  | cancelled
  | breached
  deriving DecidableEq

def terminal : CState → Bool
  | .fulfilled | .cancelled | .breached => true
  | .requested | .promised => false

/-- Steps to the furthest terminal state. -/
def rank : CState → Nat
  | .requested => 2
  | .promised => 1
  | .fulfilled | .cancelled | .breached => 0

/-- The commitment lifecycle (DEMO request → promise → execute). There is no delete event. -/
inductive CEvent where
  | promise
  | fulfil
  | cancel
  /-- The due time passed with no promise: the request lapses. -/
  | lapse
  /-- The due time passed with the promise unfulfilled: it is breached, never silently dropped. -/
  | breach
  deriving DecidableEq

def transition : CState → CEvent → Option CState
  | .requested, .promise => some .promised
  | .promised, .fulfil => some .fulfilled
  | .requested, .cancel => some .cancelled
  | .promised, .cancel => some .cancelled
  | .requested, .lapse => some .cancelled
  | .promised, .breach => some .breached
  | _, _ => none

/-- Events that need the cell clock to have reached the due time. -/
def timed : CEvent → Bool
  | .lapse | .breach => true
  | .promise | .fulfil | .cancel => false

/-- The event the deadline timer raises for a state. -/
def deadlineEvent : CState → CEvent
  | .requested => .lapse
  | _ => .breach

structure Commitment where
  state : CState
  due : Nat

def cstep (now : Nat) (c : Commitment) (e : CEvent) : Option Commitment :=
  if timed e = true ∧ now < c.due then none
  else (transition c.state e).map (fun s => ⟨s, c.due⟩)

theorem transition_terminal (s : CState) (e : CEvent) (h : terminal s = true) : transition s e = none := by
  revert h
  cases s <;> cases e <;> decide

theorem transition_rank (s : CState) (e : CEvent) {s' : CState} (h : transition s e = some s') :
    rank s' < rank s := by
  cases s <;> cases e <;> simp [transition] at h <;> subst h <;> decide

/-- Terminal states are absorbing: no event leaves fulfilled, cancelled or breached. -/
theorem terminal_absorbing (now : Nat) (c : Commitment) (e : CEvent) (h : terminal c.state = true) :
    cstep now c e = none := by
  simp [cstep, transition_terminal c.state e h]

/-- Every transition strictly lowers the rank, so a commitment takes at most two steps to end. -/
theorem cstep_rank {now : Nat} {c next : Commitment} {e : CEvent} (h : cstep now c e = some next) :
    rank next.state < rank c.state := by
  unfold cstep at h
  split at h
  · contradiction
  · cases hn : transition c.state e with
    | none => simp [hn] at h
    | some s =>
        simp only [hn, Option.map_some, Option.some.injEq] at h
        subst h
        exact transition_rank c.state e hn

/-- Progress: once the due time has passed, the deadline event ends any open commitment. -/
theorem deadline_terminates {now : Nat} {c : Commitment} (hdue : c.due ≤ now) (hn : terminal c.state = false) :
    ∃ next, cstep now c (deadlineEvent c.state) = some next ∧ terminal next.state = true := by
  obtain ⟨st, due⟩ := c
  have hd : due ≤ now := hdue
  have hnot : ¬ now < due := Nat.not_lt.mpr hd
  cases st with
  | requested => exact ⟨⟨.cancelled, due⟩, by simp [cstep, deadlineEvent, timed, transition, hnot], rfl⟩
  | promised => exact ⟨⟨.breached, due⟩, by simp [cstep, deadlineEvent, timed, transition, hnot], rfl⟩
  | fulfilled => simp [terminal] at hn
  | cancelled => simp [terminal] at hn
  | breached => simp [terminal] at hn

/-- A book of commitments; an event acts on one entry by index and never removes one. -/
def bstep (now : Nat) (cs : List Commitment) (i : Nat) (e : CEvent) : Option (List Commitment) :=
  match cs[i]? with
  | none => none
  | some c => (cstep now c e).map (fun next => cs.set i next)

/-- `core.law.commitment_completion`: no event drops a commitment. -/
theorem bstep_never_drops {now : Nat} {cs next : List Commitment} {i : Nat} {e : CEvent}
    (h : bstep now cs i e = some next) : next.length = cs.length := by
  unfold bstep at h
  split at h
  · contradiction
  · rename_i c _
    cases hc : cstep now c e with
    | none => simp [hc] at h
    | some c' =>
        simp only [hc, Option.map_some, Option.some.injEq] at h
        subst h
        simp

end Norn.Verification.CoreQuantity
