/-
Written semantics of the commit admission order (#718 P8, lane K-A).

Admission runs fourteen stages in the order documented on `check_commit`. The
first stage that refuses ends admission with that refusal; no later stage runs.
If every stage passes, the commit is admitted. Stage bodies are parameters
(`run`); this file fixes only their order and the short-circuit, and depends on
nothing but Lean core: it mentions neither Rust, Aeneas nor the extraction.
-/
namespace Norn.Kernel.Commit

/-- The admission stages of `check_commit`. -/
inductive Stage where
  | eventLimit
  | shape
  | accounting
  | delivery
  | immutable
  | versions
  | linkAuthority
  | endpoints
  | linkValues
  | cardinality
  | requiredLinks
  | acyclic
  | invariants
  | laws
  deriving DecidableEq, Repr

/-- The written order: event limit, shape, accounting, delivery pending,
immutable objects, compare-and-swap and types, link authority, endpoints, link
values, cardinality, required links, acyclic links, invariants, laws. -/
def commitOrder : List Stage :=
  [.eventLimit, .shape, .accounting, .delivery, .immutable, .versions, .linkAuthority,
   .endpoints, .linkValues, .cardinality, .requiredLinks, .acyclic, .invariants, .laws]

/-- Run `stages` in order, threading the stage state. The first refusal ends
admission with that refusal and the state it left; otherwise the result is
`ok ()` with the final state. `m` carries whatever a stage body can do besides
refusing (panic or divergence in the extracted model). -/
def admission {m : Type → Type} [Monad m] {σ E : Type}
    (run : σ → Stage → m (Except E Unit × σ)) : List Stage → σ → m (Except E Unit × σ)
  | [], s => pure (.ok (), s)
  | stage :: rest, s => do
      let (r, s1) ← run s stage
      match r with
      | .error e => pure (.error e, s1)
      | .ok () => admission run rest s1

/-- Compare-and-swap (K1, K3): read the current version of each expectation in
order and report the first that differs from the version the turn read, as
(key, expected, current). A host read failure ends the scan with that failure.
Nothing after the first conflict or failure is read. Admission scans the
commit's reads, then its writes: `firstConflict current (reads ++ writes)`. -/
def firstConflict {m : Type → Type} [Monad m] {σ K E : Type}
    (current : σ → K → m (Except E Nat × σ)) :
    List (K × Nat) → σ → m (Except E (Option (K × Nat × Nat)) × σ)
  | [], s => pure (.ok none, s)
  | (key, expected) :: rest, s => do
      let (r, s1) ← current s key
      match r with
      | .error e => pure (.error e, s1)
      | .ok found =>
        if found = expected then firstConflict current rest s1
        else pure (.ok (some (key, expected, found)), s1)

/-- Every stage runs at most once. -/
theorem commitOrder_nodup : commitOrder.Nodup := by decide

/-- Every stage runs. -/
theorem commitOrder_complete (stage : Stage) : stage ∈ commitOrder := by
  cases stage <;> decide

/-- The order the kernel guarantees (K1, K3) and #718 P8 names: shape, then
delivery pending, then compare-and-swap and types, then link endpoints, then
cardinality. -/
theorem documented_order :
    [Stage.shape, .delivery, .versions, .endpoints, .cardinality].Sublist commitOrder := by
  decide

#print axioms commitOrder_nodup
#print axioms commitOrder_complete
#print axioms documented_order

end Norn.Kernel.Commit
