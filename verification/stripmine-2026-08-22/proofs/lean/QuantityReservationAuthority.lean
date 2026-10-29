import Std

namespace Norn.Verification.QuantityReservationAuthority

/-!
Reference semantics for one full-resolution-only conserved quantity reservation.
Amounts are mathematical integers. Production separately checks canonical `i128`
arithmetic, exact scope/version/identity bindings, and atomic persistence. This file
does not claim a refinement proof to the Rust transition kernel or Turso adapter.
-/

inductive Phase where
  | empty
  | pending
  | posted
  | voided
  | expired
  deriving DecidableEq

inductive ResolutionKind where
  | post
  | void
  | expire
  deriving DecidableEq

structure Balances where
  source : Int
  destination : Int
  deriving DecidableEq

def conserved (balances : Balances) : Prop :=
  balances.source + balances.destination = 0

structure AuthorityState where
  phase : Phase
  ownerVersion : Nat
  reserved : Balances
  posted : Balances
  resolutionCount : Nat
  eventCount : Nat
  outboxCount : Nat
  receiptCount : Nat
  expiryAt : Nat
  deriving DecidableEq

def recordsAligned (state : AuthorityState) : Prop :=
  state.ownerVersion = state.eventCount ∧
    state.eventCount = state.outboxCount ∧
    state.outboxCount = state.receiptCount

def create (state : AuthorityState) (amount : Int) (expiryAt : Nat) : AuthorityState := {
  state with
  phase := .pending
  ownerVersion := state.ownerVersion + 1
  reserved := { source := -amount, destination := amount }
  eventCount := state.eventCount + 1
  outboxCount := state.outboxCount + 1
  receiptCount := state.receiptCount + 1
  expiryAt
}

def resolve (state : AuthorityState) (kind : ResolutionKind) : AuthorityState := {
  state with
  phase := match kind with
    | .post => .posted
    | .void => .voided
    | .expire => .expired
  ownerVersion := state.ownerVersion + 1
  reserved := { source := 0, destination := 0 }
  posted := match kind with
    | .post => {
        source := state.posted.source + state.reserved.source
        destination := state.posted.destination + state.reserved.destination
      }
    | .void | .expire => state.posted
  resolutionCount := state.resolutionCount + 1
  eventCount := state.eventCount + 1
  outboxCount := state.outboxCount + 1
  receiptCount := state.receiptCount + 1
}

def terminal : Phase → Bool
  | .posted | .voided | .expired => true
  | .empty | .pending => false

def canResolve (state : AuthorityState) : Bool := state.phase == .pending

def resolveWhenPending
    (state : AuthorityState) (kind : ResolutionKind) : Option AuthorityState :=
  if state.phase = .pending then some (resolve state kind) else none

theorem create_reservation_is_conserved
    (state : AuthorityState) (amount : Int) (expiryAt : Nat) :
    conserved (create state amount expiryAt).reserved := by
  simp only [create, conserved]
  omega

theorem create_preserves_record_alignment
    (state : AuthorityState) (amount : Int) (expiryAt : Nat)
    (aligned : recordsAligned state) :
    recordsAligned (create state amount expiryAt) := by
  rcases aligned with ⟨hVersion, hOutbox, hReceipt⟩
  simp [recordsAligned, create, hVersion, hOutbox, hReceipt]

theorem every_resolution_releases_the_full_reservation
    (state : AuthorityState) (kind : ResolutionKind) :
    (resolve state kind).reserved = { source := 0, destination := 0 } := by
  rfl

theorem full_post_preserves_posted_conservation
    (state : AuthorityState)
    (reservedConserved : conserved state.reserved)
    (postedConserved : conserved state.posted) :
    conserved (resolve state .post).posted := by
  unfold conserved at reservedConserved postedConserved ⊢
  simp only [resolve]
  omega

theorem void_and_expiry_leave_posted_balances_unchanged
    (state : AuthorityState) :
    (resolve state .void).posted = state.posted ∧
      (resolve state .expire).posted = state.posted := by
  simp [resolve]

theorem resolution_preserves_record_alignment
    (state : AuthorityState) (kind : ResolutionKind)
    (aligned : recordsAligned state) :
    recordsAligned (resolve state kind) := by
  rcases aligned with ⟨hVersion, hOutbox, hReceipt⟩
  simp [recordsAligned, resolve, hVersion, hOutbox, hReceipt]

theorem one_pending_resolution_is_admitted_terminal_and_counted_once
    (state : AuthorityState) (kind : ResolutionKind)
    (pending : state.phase = .pending)
    (unresolved : state.resolutionCount = 0) :
    resolveWhenPending state kind = some (resolve state kind) ∧
      terminal (resolve state kind).phase = true ∧
      (resolve state kind).resolutionCount = 1 := by
  cases kind <;> simp [resolveWhenPending, pending, resolve, terminal, unresolved]

theorem a_terminal_result_rejects_every_second_resolution
    (state : AuthorityState) (firstKind secondKind : ResolutionKind) :
    resolveWhenPending (resolve state firstKind) secondKind = none := by
  cases firstKind <;> simp [resolveWhenPending, resolve]

structure Request where
  expectedOwnerVersion : Nat
  inputIdentity : Nat
  deriving DecidableEq

structure StableReceipt where
  inputIdentity : Nat
  resultingOwnerVersion : Nat
  deriving DecidableEq

def commitOrReplay
    (state : AuthorityState)
    (receipt : Option StableReceipt)
    (request : Request)
    (next : AuthorityState) : Option (AuthorityState × StableReceipt) :=
  match receipt with
  | some stored =>
      if stored.inputIdentity = request.inputIdentity then some (state, stored) else none
  | none =>
      if request.expectedOwnerVersion = state.ownerVersion then
        some (next, { inputIdentity := request.inputIdentity,
                      resultingOwnerVersion := next.ownerVersion })
      else none

theorem exact_retry_is_an_authority_noop
    (state next : AuthorityState) (stored : StableReceipt) (request : Request)
    (same : stored.inputIdentity = request.inputIdentity) :
    commitOrReplay state (some stored) request next = some (state, stored) := by
  simp [commitOrReplay, same]

theorem changed_retry_conflicts
    (state next : AuthorityState) (stored : StableReceipt) (request : Request)
    (different : stored.inputIdentity ≠ request.inputIdentity) :
    commitOrReplay state (some stored) request next = none := by
  simp [commitOrReplay, different]

def expireWhenDue (state : AuthorityState) (trustedNow : Nat) : Option AuthorityState :=
  if state.phase = .pending ∧ state.expiryAt ≤ trustedNow then
    some (resolve state .expire)
  else none

theorem expiry_before_due_is_rejected
    (state : AuthorityState) (pending : state.phase = .pending)
    (trustedNow : Nat) (early : trustedNow < state.expiryAt) :
    expireWhenDue state trustedNow = none := by
  simp [expireWhenDue, pending, Nat.not_le_of_gt early]

theorem expiry_at_due_resolves
    (state : AuthorityState) (pending : state.phase = .pending) :
    expireWhenDue state state.expiryAt = some (resolve state .expire) := by
  simp [expireWhenDue, pending]

end Norn.Verification.QuantityReservationAuthority
