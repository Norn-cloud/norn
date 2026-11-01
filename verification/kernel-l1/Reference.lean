import Std

/- Executable reference fragment, not a refinement theorem for the Rust kernel.
   Integers here are mathematical integers. Correspondence is tested only for
   the small exact-integer corpus; decimal arithmetic and evaluation fuel are
   deliberately outside this fragment. -/
namespace Norn.KernelL1

inductive Guard where
  | truth (value : Bool)
  | positive
  | atMost (bound : Int)
  | both (left right : Guard)
  deriving Repr

def evalGuard (amount : Int) : Guard → Bool
  | .truth value => value
  | .positive => amount > 0
  | .atMost bound => amount ≤ bound
  | .both left right => evalGuard amount left && evalGuard amount right

inductive Phase where
  | draft | submitted | approved | cancelled
  deriving BEq, DecidableEq, Repr

def submit (phase : Phase) (amount : Int) : Option Phase :=
  if phase == .draft && evalGuard amount .positive then some .submitted else none

inductive HitPolicy where
  | first | collect
  deriving Repr

structure Row where
  guard : Guard
  output : Int
  deriving Repr

def evalTable (policy : HitPolicy) (amount : Int) : List Row → List Int
  | [] => []
  | row :: rest =>
    if evalGuard amount row.guard then
      match policy with
      | .first => [row.output]
      | .collect => row.output :: evalTable policy amount rest
    else evalTable policy amount rest

-- u64 overflow is refused, never idealized as an unbounded increment.
def maxVersion : Nat := 18446744073709551615

def nextVersion (version : Nat) : Option Nat :=
  if version < maxVersion then some (version + 1) else none

structure State where
  version : Nat
  value : Int
  events : List Int
  outbox : List Int
  receipts : List (Nat × Int)
  deriving BEq, DecidableEq, Repr

structure Request where
  id : Nat
  fingerprint : Int
  expectedVersion : Nat
  value : Int
  deriving Repr

inductive Outcome where
  | committed | replay | reused | conflict | overflow
  deriving BEq, DecidableEq, Repr

-- Receipt lookup retains history; fingerprints are inputs, not a hashing model.
def receiptFor (id : Nat) : List (Nat × Int) → Option Int
  | [] => none
  | (stored, fingerprint) :: rest =>
      if stored == id then some fingerprint else receiptFor id rest

-- Single-object transaction model. Production host persistence remains a
-- separate obligation, including binding fingerprints to complete inputs.
def commit (state : State) (request : Request) : State × Outcome :=
  match receiptFor request.id state.receipts with
  | some fingerprint =>
    if fingerprint == request.fingerprint then (state, .replay) else (state, .reused)
  | none => fresh state request
where
  fresh (state : State) (request : Request) : State × Outcome :=
    if state.version != request.expectedVersion then (state, .conflict)
    else match nextVersion state.version with
      | none => (state, .overflow)
      | some version =>
        ({ version, value := request.value,
           events := state.events ++ [request.value],
           outbox := state.outbox ++ [request.value],
           receipts := (request.id, request.fingerprint) :: state.receipts }, .committed)

theorem submit_guard (phase : Phase) (amount : Int)
    (h : submit phase amount = some .submitted) : amount > 0 := by
  unfold submit at h
  split at h
  next condition =>
    exact of_decide_eq_true (Bool.and_eq_true_iff.mp condition).2
  next => cases h

theorem overflow_refused : nextVersion maxVersion = none := by
  simp [nextVersion]

theorem fresh_conflict_unchanged (state : State) (request : Request)
    (h : state.version ≠ request.expectedVersion) :
    commit.fresh state request = (state, .conflict) := by
  simp [commit.fresh, h]

theorem exact_replay_unchanged (state : State) (request : Request)
    (h : receiptFor request.id state.receipts = some request.fingerprint) :
    commit state request = (state, .replay) := by
  simp [commit, h]

theorem changed_input_refused (state : State) (request : Request) (fingerprint : Int)
    (h : receiptFor request.id state.receipts = some fingerprint)
    (different : fingerprint ≠ request.fingerprint) :
    commit state request = (state, .reused) := by
  simp [commit, h, different]

theorem fresh_atomic (state : State) (request : Request) (after : State)
    (h : commit.fresh state request = (after, .committed)) :
    after.value = request.value ∧
    after.events = state.events ++ [request.value] ∧
    after.outbox = state.outbox ++ [request.value] ∧
    after.receipts = (request.id, request.fingerprint) :: state.receipts := by
  unfold commit.fresh at h
  split at h
  · cases h
  · split at h
    · cases h
    · cases h
      simp


theorem successful_commit_replays (state : State) (request : Request) (after : State)
    (h : commit state request = (after, .committed)) :
    commit after request = (after, .replay) := by
  cases old : receiptFor request.id state.receipts with
  | none =>
      have fresh : commit.fresh state request = (after, .committed) := by
        simpa only [commit, old] using h
      have fields := fresh_atomic state request after fresh
      apply exact_replay_unchanged
      rw [fields.2.2.2]
      simp [receiptFor]
  | some fingerprint =>
      by_cases same : fingerprint = request.fingerprint
      · simp [commit, old, same] at h
      · simp [commit, old, same] at h

theorem fresh_overflow_unchanged (state : State) (request : Request)
    (current : state.version = maxVersion)
    (expected : request.expectedVersion = maxVersion) :
    commit.fresh state request = (state, .overflow) := by
  simp [commit.fresh, current, expected, nextVersion]


end Norn.KernelL1

-- Stable, deliberately tiny corpus transport. No parser or generated theorem
-- enters the trusted model; Rust consumes these rows in its ordinary tests.
def main : IO Unit := do
  let stdout ← IO.getStdout
  for (name, phase) in [("draft", Norn.KernelL1.Phase.draft),
      ("submitted", .submitted), ("approved", .approved), ("cancelled", .cancelled)] do
    for amount in ([-2, -1, 0, 1, 2] : List Int) do
      let accepted := (Norn.KernelL1.submit phase amount).isSome
      stdout.putStrLn s!"lifecycle\t{name}\t{amount}\t{accepted}"

  let rows : List Norn.KernelL1.Row := [⟨.atMost 0, -1⟩, ⟨.truth true, 1⟩, ⟨.positive, 2⟩]
  for (name, policy) in [("first", Norn.KernelL1.HitPolicy.first), ("collect", .collect)] do
    for amount in ([-2, -1, 0, 1, 2] : List Int) do
      let values := Norn.KernelL1.evalTable policy amount rows
      stdout.putStrLn s!"table\t{name}\t{amount}\t{String.intercalate "," (values.map toString)}"
