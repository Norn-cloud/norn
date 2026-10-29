import Std

namespace Norn.Verification.QuantityAuthority

/-!
Reference semantics for the pure unit-agnostic authority transition. Amounts
are mathematical integers here; the production checker separately rejects
non-canonical and overflowing `i128` operations. This file does not claim a
refinement proof to Rust or Turso.
-/

structure Balances where
  source : Int
  destination : Int
  deriving DecidableEq

def conserved (balances : Balances) : Prop :=
  balances.source + balances.destination = 0

def post (balances : Balances) (amount : Int) : Balances := {
  source := balances.source - amount
  destination := balances.destination + amount
}

/- A reversal is another posting of the exact negated admitted amount. It does
not receive the prior balance as an input from which it could reconstruct the
desired answer. -/
def negatePosting (after : Balances) (amount : Int) : Balances :=
  post after (-amount)

theorem posting_preserves_conservation
    (balances : Balances) (amount : Int)
    (h : conserved balances) : conserved (post balances amount) := by
  unfold conserved at h ⊢
  change balances.source - amount + (balances.destination + amount) = 0
  omega

theorem exact_reversal_restores_prior_balances
    (before : Balances) (amount : Int) :
    negatePosting (post before amount) amount = before := by
  cases before with
  | mk source destination =>
      simp only [negatePosting, post]
      congr <;> omega

def i128Min : Int := -(2 ^ 127)
def i128Max : Int := (2 ^ 127) - 1

/- Rejecting the unique minimum signed value is sufficient to keep exact
negation representable for every admitted i128 amount. -/
theorem admitted_i128_amount_has_representable_negation
    (amount : Int)
    (aboveMin : i128Min < amount)
    (atMostMax : amount ≤ i128Max) :
    i128Min ≤ -amount ∧ -amount ≤ i128Max := by
  simp only [i128Min, i128Max] at aboveMin atMostMax ⊢
  constructor <;> omega

structure AuthorityState where
  ownerVersion : Nat
  transactionCount : Nat
  eventCount : Nat
  outboxCount : Nat
  receiptCount : Nat
  balances : Balances
  deriving DecidableEq

def atomicallyAppend (state : AuthorityState) (nextBalances : Balances) : AuthorityState := {
  ownerVersion := state.ownerVersion + 1
  transactionCount := state.transactionCount + 1
  eventCount := state.eventCount + 1
  outboxCount := state.outboxCount + 1
  receiptCount := state.receiptCount + 1
  balances := nextBalances
}

def recordsAligned (state : AuthorityState) : Prop :=
  state.ownerVersion = state.transactionCount ∧
    state.transactionCount = state.eventCount ∧
    state.eventCount = state.outboxCount ∧
    state.outboxCount = state.receiptCount

theorem atomic_append_preserves_record_alignment
    (state : AuthorityState) (nextBalances : Balances)
    (h : recordsAligned state) :
    recordsAligned (atomicallyAppend state nextBalances) := by
  rcases h with ⟨hVersion, hEvent, hOutbox, hReceipt⟩
  constructor
  · simp [atomicallyAppend, hVersion]
  constructor
  · simp [atomicallyAppend, hEvent]
  constructor
  · simp [atomicallyAppend, hOutbox]
  · simp [atomicallyAppend, hReceipt]

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
    (nextBalances : Balances) : Option (AuthorityState × StableReceipt) :=
  match receipt with
  | some stored =>
      if stored.inputIdentity = request.inputIdentity then some (state, stored) else none
  | none =>
      if request.expectedOwnerVersion = state.ownerVersion then
        let next := atomicallyAppend state nextBalances
        some (next, { inputIdentity := request.inputIdentity,
                      resultingOwnerVersion := next.ownerVersion })
      else none

theorem exact_retry_is_authority_noop
    (state : AuthorityState) (stored : StableReceipt) (request : Request)
    (nextBalances : Balances)
    (same : stored.inputIdentity = request.inputIdentity) :
    commitOrReplay state (some stored) request nextBalances = some (state, stored) := by
  simp [commitOrReplay, same]

theorem conflicting_retry_rejects
    (state : AuthorityState) (stored : StableReceipt) (request : Request)
    (nextBalances : Balances)
    (different : stored.inputIdentity ≠ request.inputIdentity) :
    commitOrReplay state (some stored) request nextBalances = none := by
  simp [commitOrReplay, different]

theorem stale_first_commit_rejects
    (state : AuthorityState) (request : Request) (nextBalances : Balances)
    (stale : request.expectedOwnerVersion ≠ state.ownerVersion) :
    commitOrReplay state none request nextBalances = none := by
  simp [commitOrReplay, stale]

end Norn.Verification.QuantityAuthority
