/-!
# The decider's fold, step and envelope: written semantics

This is the written meaning of the kernel's decider core (ruling 2026-09-25
§3 and §5; K6 in #718). It imports nothing: it is stated over any monad `m`,
so a decider's `evolve`, `decide` and the turn's observers may fail, and the
semantics says exactly where a failure surfaces. The correspondence proofs in
`FoldProof.lean` and `StepProof.lean` instantiate `m` with Aeneas's `Result`
and show the production functions in `crates/norn-kernel/src/decider_fold.rs`
are these definitions.

* `foldLedger`: materialized state is a left fold of `evolve` over the ledger,
  first event first; the first failing `evolve` fails the fold.
* `replayLedger`: replay is that fold from the initial state.
* `firstExceeded`, `firstTurnExceeded`: the envelope caps, checked in the order
  events, commands, timers; the first cap exceeded is the one reported.
* `step`: a turn's checks in their written order. The world is writable; the
  decider accepts; the decision fits the caps; a world that may not perform
  requests no command. Only then are the decision's events folded into a copy
  of the state. A refusal carries no state and no events.
-/

namespace Norn.Kernel.Decider

/-- The ledger fold: `evolve` each event in order, first event first. -/
def foldLedger {m : Type → Type} [Monad m] {S E : Type} (evolve : S → E → m S)
    (state : S) (events : List E) : m S :=
  events.foldlM evolve state

/-- Replay: the ledger fold from the initial state. -/
def replayLedger {m : Type → Type} [Monad m] {S E : Type} (initial : m S)
    (evolve : S → E → m S) (events : List E) : m S := do
  let state ← initial
  foldLedger evolve state events

/-- An envelope kind. -/
inductive Cap where
  | events
  | commands
  | timers
  deriving DecidableEq, Repr

/-- One count, or one cap, per envelope kind. -/
structure Tally where
  events : Nat
  commands : Nat
  timers : Nat
  deriving DecidableEq, Repr

/-- The count of one kind. -/
def Tally.get (tally : Tally) : Cap → Nat
  | .events => tally.events
  | .commands => tally.commands
  | .timers => tally.timers

/-- The first kind, in the order events, commands, timers, whose count is above
    its cap. -/
def firstExceeded (caps counts : Tally) : Option Cap :=
  if caps.events < counts.events then some .events
  else if caps.commands < counts.commands then some .commands
  else if caps.timers < counts.timers then some .timers
  else none

/-- The largest `u64`: a turn total that overflows is reported as this. -/
def u64Max : Nat := 2 ^ 64 - 1

/-- A turn total as it is reported: the sum, saturated at `u64Max`. -/
def turnTotal (used added : Nat) : Nat := min (used + added) u64Max

/-- The first kind, in the order events, commands, timers, whose turn total
    `used + added` is above its cap, with that total as reported. -/
def firstTurnExceeded (used caps added : Tally) : Option (Cap × Nat) :=
  if caps.events < used.events + added.events then
    some (.events, turnTotal used.events added.events)
  else if caps.commands < used.commands + added.commands then
    some (.commands, turnTotal used.commands added.commands)
  else if caps.timers < used.timers + added.timers then
    some (.timers, turnTotal used.timers added.timers)
  else none

/-- Why a turn was refused. -/
inductive Refusal (R : Type) where
  | readOnlyWorld
  | rejected (rejection : R)
  | envelopeExceeded (cap : Cap) (actual : Nat)
  | performOutsideMain (commands : Nat)
  deriving DecidableEq, Repr

/-- What one turn offers the step: each observation is an action in `m`, so a
    failing observer fails the turn where it is consulted. -/
structure Turn (m : Type → Type) (S E D R : Type) where
  writable : m Bool
  mayPerform : m Bool
  decide : S → m (Except R D)
  counts : D → m Tally
  caps : m Tally
  events : D → m (List E)
  cloneState : S → m S
  evolve : S → E → m S

/-- One turn, its checks in the written order. `mayPerform` is consulted only
    when the decision requests a command. -/
def step {m : Type → Type} [Monad m] {S E D R : Type} (turn : Turn m S E D R)
    (state : S) : m (Except (Refusal R) (S × D)) := do
  let writable ← turn.writable
  if writable = false then
    return .error .readOnlyWorld
  let decided ← turn.decide state
  match decided with
  | .error rejection => return .error (.rejected rejection)
  | .ok decision =>
    let counts ← turn.counts decision
    let caps ← turn.caps
    match firstExceeded caps counts with
    | some cap => return .error (.envelopeExceeded cap (counts.get cap))
    | none =>
      if 0 < counts.commands then
        let mayPerform ← turn.mayPerform
        if mayPerform = false then
          return .error (.performOutsideMain counts.commands)
      let copy ← turn.cloneState state
      let events ← turn.events decision
      let next ← foldLedger turn.evolve copy events
      return .ok (next, decision)

end Norn.Kernel.Decider
