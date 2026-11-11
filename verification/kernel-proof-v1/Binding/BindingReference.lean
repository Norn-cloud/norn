/-!
# K6/K7 written semantics: idempotency replay and receipt binding

The written meaning of the kernel's replay decision (K6, `action_commit`'s
idempotency key) and of receipt binding (K7/K5: RECEIPT-AUTH-BIND-1,
RECEIPT-CONSUME-1, RUN-BIND-1). It mentions neither machine integers nor the
extraction: generations and attempts are natural numbers, digests, commands and
runs are arbitrary types compared by equality.

`BindingProof` proves the production cores in
`crates/norn-kernel/src/replay_decision.rs` and `receipt_decision.rs` equal to
these definitions.
-/

namespace Norn.Kernel.Binding

/-! ## K6: idempotency replay -/

/-- The identity an idempotency key binds: the action, the target object, the
    call's input, the actor and the writer-closure binding. -/
structure Call (A O I B : Type) where
  action : A
  object : O
  input : I
  actor : A
  ruleBinding : B
  deriving DecidableEq

/-- What an idempotency key holds. -/
inductive Stored (R : Type) where
  /-- Nothing was committed under the key. -/
  | empty
  /-- A record that does not decode. -/
  | unreadable
  /-- The committed call's record. -/
  | record (recorded : R)

/-- What a repeated idempotency key decides. -/
inductive Replay (R : Type) where
  /-- Nothing was committed under the key: the call runs. -/
  | fresh
  /-- The same call was committed: its recorded outcome is returned. -/
  | replay (recorded : R)
  /-- The key holds another call, or a record that does not decode. -/
  | conflict
  deriving DecidableEq

/-- K6. A key that holds nothing runs the call; a key that holds the same call
    (under `same`) replays the recorded call; anything else under the key,
    including a record that does not decode, is a conflict. -/
def replay {R : Type} (same : R → R → Bool) (stored : Stored R) (call : R) : Replay R :=
  match stored with
  | .empty => .fresh
  | .unreadable => .conflict
  | .record recorded => if same recorded call then .replay recorded else .conflict

/-- The same call, compared component by component with the given equalities. -/
def sameCallWith {A O I B : Type} (eqA : A → A → Bool) (eqO : O → O → Bool)
    (eqI : I → I → Bool) (eqB : B → B → Bool) (recorded call : Call A O I B) : Bool :=
  eqA recorded.action call.action && eqO recorded.object call.object &&
    eqI recorded.input call.input && eqA recorded.actor call.actor &&
    eqB recorded.ruleBinding call.ruleBinding

/-! ## K7: receipt binding -/

/-- Everything a receipt binding compares. `consumed` is the delivery the
    acceptance consumes when it is a command result (`command`, `generation`);
    `accepted` is the command state's accepted `(receipt, resultIdentity,
    generation)`. -/
structure Pins (D C : Type) where
  requested : D
  receiptDigest : D
  receiptResultIdentity : D
  receiptCommand : C
  receiptGeneration : Nat
  receiptAttempt : Nat
  recordReceipt : D
  recordResultIdentity : D
  recordCommand : C
  recordGeneration : Nat
  recordAttempt : Nat
  stateGeneration : Nat
  stateCancelled : Bool
  consumed : Option (C × Nat)
  accepted : Option (D × D × Nat)

/-- RECEIPT-AUTH-BIND-1 and RECEIPT-CONSUME-1. A receipt binds exactly when it
    is the requested receipt everywhere (receipt, result record, accepted
    state), its result identity, command, generation and attempt are the
    record's, the command is not cancelled at that generation, and the delivery
    it consumes is exactly this command's result at this generation. -/
def Binds {D C : Type} (p : Pins D C) : Prop :=
  p.receiptDigest = p.requested ∧
  p.recordReceipt = p.requested ∧
  p.recordResultIdentity = p.receiptResultIdentity ∧
  p.recordCommand = p.receiptCommand ∧
  p.recordGeneration = p.receiptGeneration ∧
  p.recordAttempt = p.receiptAttempt ∧
  p.stateGeneration = p.recordGeneration ∧
  p.consumed = some (p.recordCommand, p.recordGeneration) ∧
  p.stateCancelled = false ∧
  p.accepted = some (p.requested, p.receiptResultIdentity, p.recordGeneration)

/-- The same decision under arbitrary equality functions on digests and
    commands, in the written comparison order. -/
def bindsWith {D C : Type} (eqD : D → D → Bool) (eqC : C → C → Bool) (p : Pins D C) :
    Bool :=
  eqD p.receiptDigest p.requested && eqD p.receiptDigest p.recordReceipt &&
    eqD p.receiptResultIdentity p.recordResultIdentity &&
    eqC p.recordCommand p.receiptCommand &&
    decide (p.recordGeneration = p.receiptGeneration) &&
    decide (p.recordAttempt = p.receiptAttempt) &&
    decide (p.stateGeneration = p.recordGeneration) &&
    (match p.consumed with
      | some (command, generation) =>
          eqC command p.recordCommand && decide (generation = p.recordGeneration)
      | none => false) &&
    !p.stateCancelled &&
    (match p.accepted with
      | some (receipt, resultIdentity, generation) =>
          eqD receipt p.receiptDigest && eqD resultIdentity p.receiptResultIdentity &&
            decide (generation = p.recordGeneration)
      | none => false)

/-! ## RUN-BIND-1: a bound receipt binds exactly its commit -/

/-- The commit an action makes from a bound receipt: the receipt's manifest and
    run, the declared manifest and target run, and the generations. -/
structure Commit (D R : Type) where
  manifest : D
  declaredManifest : D
  run : R
  target : R
  runGeneration : Nat
  recordGeneration : Nat
  stateGeneration : Nat

/-- Why a bound receipt does not bind the commit. -/
inductive CommitRefusal where
  | manifestMismatch
  | runMismatch
  deriving DecidableEq

/-- RUN-BIND-1, in the written check order: the manifest first, then the run,
    then the record's and state's generations. -/
def commitBinds {D R : Type} [DecidableEq D] [DecidableEq R] (c : Commit D R) :
    Except CommitRefusal Unit :=
  if c.manifest ≠ c.declaredManifest then .error .manifestMismatch
  else if c.run ≠ c.target then .error .runMismatch
  else if ¬ (c.recordGeneration = c.runGeneration ∧ c.stateGeneration = c.runGeneration) then
    .error .runMismatch
  else .ok ()

end Norn.Kernel.Binding
