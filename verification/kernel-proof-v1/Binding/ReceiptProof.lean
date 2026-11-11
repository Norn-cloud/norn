import Receipt.Funs
import BindingReference

/-!
# Production `pins_bind` and `commit_pins_bind` meet K7/K5

`Receipt` is the fresh Aeneas translation of the production file
`crates/norn-kernel/src/receipt_decision.rs`. For every digest and command
type, every equality implementation and every input, the production binding
predicate is the written one; under equality instances that decide equality it
holds exactly when the receipt binds (RECEIPT-AUTH-BIND-1, RECEIPT-CONSUME-1),
and the commit check refuses exactly as RUN-BIND-1 is written, in its order.
-/

namespace Norn.Kernel.Binding
open Aeneas Aeneas.Std Result
open norn_kernel_receipt

/-- The production pins as the written ones. -/
def pinsOf {D C : Type} (p : BindingPins D C) : Pins D C where
  requested := p.requested
  receiptDigest := p.receipt_digest
  receiptResultIdentity := p.receipt_result_identity
  receiptCommand := p.receipt_command
  receiptGeneration := p.receipt_generation.val
  receiptAttempt := p.receipt_attempt.val
  recordReceipt := p.record_receipt
  recordResultIdentity := p.record_result_identity
  recordCommand := p.record_command
  recordGeneration := p.record_generation.val
  recordAttempt := p.record_attempt.val
  stateGeneration := p.state_generation.val
  stateCancelled := p.state_cancelled
  consumed := p.consumed.map fun c => (c.command, c.generation.val)
  accepted := p.accepted.map fun a => (a.receipt, a.result_identity, a.generation.val)

/-- The production commit pins as the written ones. -/
def commitOf {D R : Type} (c : CommitPins D R) : Commit D R where
  manifest := c.manifest
  declaredManifest := c.declared_manifest
  run := c.run
  target := c.target
  runGeneration := c.run_generation.val
  recordGeneration := c.record_generation.val
  stateGeneration := c.state_generation.val

/-- The written commit refusal as the production result. -/
def toCommitResult : Except CommitRefusal Unit → core.result.Result Unit CommitPinRefusal
  | .ok () => .Ok ()
  | .error .manifestMismatch => .Err .ManifestMismatch
  | .error .runMismatch => .Err .RunMismatch

private theorem u64_eq_iff (a b : U64) : a = b ↔ a.val = b.val := UScalar.eq_equiv a b
theorem u32_eq_iff (a b : U32) : a = b ↔ a.val = b.val := UScalar.eq_equiv a b

/-- For every equality implementation on digests and commands (answering
    `eqD` and `eqC`), production `pins_bind` is exactly the written decision. -/
theorem pins_bind_correspondence {D C : Type} (iD : core.cmp.PartialEq D D)
    (iC : core.cmp.PartialEq C C) (eqD : D → D → Bool) (eqC : C → C → Bool)
    (hD : ∀ a b, iD.eq a b = ok (eqD a b)) (hC : ∀ a b, iC.eq a b = ok (eqC a b))
    (p : BindingPins D C) :
    pins_bind iD iC p = ok (bindsWith eqD eqC (pinsOf p)) := by
  obtain ⟨rq, rd, ri, rc, rg, ra, kd, ki, kc, kg, ka, sg, sc, cons, acc⟩ := p
  have dr : delivery_is_result iC ⟨rq, rd, ri, rc, rg, ra, kd, ki, kc, kg, ka, sg, sc, cons, acc⟩ =
      ok (match cons.map fun c => (c.command, c.generation.val) with
        | some (command, generation) => eqC command kc && decide (generation = kg.val)
        | none => false) := by
    rcases cons with _ | ⟨command, generation⟩
    · rfl
    · simp only [delivery_is_result, hC, bind_tc_ok, Option.map]
      cases eqC command kc <;> simp [u64_eq_iff]
  have sa : state_accepted iD ⟨rq, rd, ri, rc, rg, ra, kd, ki, kc, kg, ka, sg, sc, cons, acc⟩ =
      ok (match acc.map fun a => (a.receipt, a.result_identity, a.generation.val) with
        | some (receipt, resultIdentity, generation) =>
            eqD receipt rd && eqD resultIdentity ri && decide (generation = kg.val)
        | none => false) := by
    rcases acc with _ | ⟨receipt, identity, generation⟩
    · rfl
    · simp only [state_accepted, hD, bind_tc_ok, Option.map]
      cases eqD receipt rd <;> cases eqD identity ri <;> simp [u64_eq_iff]
  simp only [pins_bind, hD, hC, bind_tc_ok, dr, sa, bindsWith, pinsOf]
  cases eqD rd rq <;> simp only [Bool.false_and, Bool.true_and, Bool.false_eq_true,
    if_false, if_true]
  cases eqD rd kd <;> simp only [Bool.false_and, Bool.true_and, Bool.false_eq_true,
    if_false, if_true]
  cases eqD ri ki <;> simp only [Bool.false_and, Bool.true_and, Bool.false_eq_true,
    if_false, if_true]
  cases eqC kc rc <;> simp only [Bool.false_and, Bool.true_and, Bool.false_eq_true,
    if_false, if_true]
  by_cases hg : kg = rg <;> simp only [hg, (u64_eq_iff kg rg).not.mp,
    decide_true, decide_false, Bool.false_and, Bool.true_and, if_false, if_true, not_false_eq_true]
  by_cases ha : ka = ra <;> simp only [ha, (u32_eq_iff ka ra).not.mp,
    decide_true, decide_false, Bool.false_and, Bool.true_and, if_false, if_true, not_false_eq_true]
  by_cases hs : sg = rg <;> simp only [hs, (u64_eq_iff sg rg).not.mp,
    decide_true, decide_false, Bool.false_and, Bool.true_and, if_false, if_true, not_false_eq_true]
  cases sc <;> rcases cons with _ | ⟨command, generation⟩ <;> simp only [Option.map] <;>
    simp
  all_goals
    by_cases e₁ : eqC command kc = true <;> by_cases e₂ : generation.val = rg.val <;>
      simp [e₁, e₂]
  all_goals rcases acc with _ | _ <;> rfl

/-- K7: under equality instances that decide equality, production
    `pins_bind` answers, without failing, exactly whether the receipt binds. -/
theorem pins_bind_binds {D C : Type} [DecidableEq D] [DecidableEq C]
    (iD : core.cmp.PartialEq D D) (iC : core.cmp.PartialEq C C)
    (hD : ∀ a b, iD.eq a b = ok (decide (a = b))) (hC : ∀ a b, iC.eq a b = ok (decide (a = b)))
    (p : BindingPins D C) :
    ∃ b, pins_bind iD iC p = ok b ∧ (b = true ↔ Binds (pinsOf p)) := by
  refine ⟨_, pins_bind_correspondence iD iC _ _ hD hC p, ?_⟩
  obtain ⟨rq, rd, ri, rc, rg, ra, kd, ki, kc, kg, ka, sg, sc, cons, acc⟩ := p
  simp only [bindsWith, Binds, pinsOf]
  rcases cons with _ | ⟨command, generation⟩ <;> rcases acc with _ | ⟨receipt, identity, ag⟩ <;>
    simp only [Option.map, Option.some.injEq, Prod.mk.injEq, reduceCtorEq, and_false,
      false_and, Bool.and_false, Bool.false_eq_true, iff_false] <;>
    first
    | (intro h; simp_all)
    | (constructor
       · intro h; simp only [Bool.and_eq_true, decide_eq_true_eq,
           Bool.not_eq_true'] at h; obtain ⟨⟨⟨⟨⟨⟨⟨⟨⟨h1, h2⟩, h3⟩, h4⟩, h5⟩, h6⟩, h7⟩, h8⟩, h9⟩, h10⟩ := h
         subst_vars; simp_all
       · rintro ⟨h1, h2, h3, h4, h5, h6, h7, h8, h9, h10⟩; subst_vars; simp_all)

/-- RUN-BIND-1: under equality instances that decide equality, production
    `commit_pins_bind` refuses exactly as written, in the written order. -/
theorem commit_pins_bind_correct {D R : Type} [DecidableEq D] [DecidableEq R]
    (iD : core.cmp.PartialEq D D) (iR : core.cmp.PartialEq R R)
    (hD : ∀ a b, iD.eq a b = ok (decide (a = b))) (hR : ∀ a b, iR.eq a b = ok (decide (a = b)))
    (c : CommitPins D R) :
    commit_pins_bind iD iR c = ok (toCommitResult (commitBinds (commitOf c))) := by
  obtain ⟨m, dm, r, t, rg, kg, sg⟩ := c
  simp only [commit_pins_bind, hD, hR, bind_tc_ok, commitBinds, commitOf]
  by_cases h₁ : m = dm <;> by_cases h₂ : r = t <;> by_cases h₃ : kg = rg <;>
    by_cases h₄ : sg = rg <;> simp [h₁, h₂, h₃, h₄, toCommitResult] <;>
    simp_all [u64_eq_iff]

/-- A bound receipt binds its commit exactly when the manifest, the run and
    both generations are the receipt's. -/
theorem commit_pins_bind_ok_iff {D R : Type} [DecidableEq D] [DecidableEq R]
    (iD : core.cmp.PartialEq D D) (iR : core.cmp.PartialEq R R)
    (hD : ∀ a b, iD.eq a b = ok (decide (a = b))) (hR : ∀ a b, iR.eq a b = ok (decide (a = b)))
    (c : CommitPins D R) :
    commit_pins_bind iD iR c = ok (.Ok ()) ↔
      c.manifest = c.declared_manifest ∧ c.run = c.target ∧
        c.record_generation = c.run_generation ∧ c.state_generation = c.run_generation := by
  rw [commit_pins_bind_correct iD iR hD hR]
  obtain ⟨m, dm, r, t, rg, kg, sg⟩ := c
  simp only [commitBinds, commitOf, u64_eq_iff]
  by_cases h₁ : m = dm <;> by_cases h₂ : r = t <;> by_cases h₃ : kg.val = rg.val <;>
    by_cases h₄ : sg.val = rg.val <;> simp [h₁, h₂, h₃, h₄, toCommitResult]

/-! ## Concrete vectors

Closed checks the kernel evaluates on the production code, so a seeded bug is
refuted (`decide` finds the proposition false), not merely left unproved. -/

/-- Decided equality on `Nat`, as the derived `PartialEq` of digests, commands
    and runs. -/
def pinEq : core.cmp.PartialEq Nat Nat := { eq := fun a b => ok (decide (a = b)) }

/-- Whether a production Boolean result is `e`. -/
def boolIs (r : Result Bool) (e : Bool) : Bool :=
  match r with
  | ok b => b == e
  | _ => false

/-- A receipt that binds: every digest, command, generation and attempt agrees. -/
def pins₀ : BindingPins Nat Nat where
  requested := 1
  receipt_digest := 1
  receipt_result_identity := 2
  receipt_command := 3
  receipt_generation := 4#u64
  receipt_attempt := 1#u32
  record_receipt := 1
  record_result_identity := 2
  record_command := 3
  record_generation := 4#u64
  record_attempt := 1#u32
  state_generation := 4#u64
  state_cancelled := false
  consumed := some ⟨3, 4#u64⟩
  accepted := some ⟨1, 2, 4#u64⟩

/-- The binding receipt, and each single pin broken. -/
def receiptVectors : List (BindingPins Nat Nat × Bool) :=
  [ (pins₀, true),
    ({ pins₀ with requested := 9 }, false),
    ({ pins₀ with record_receipt := 9 }, false),
    ({ pins₀ with record_result_identity := 9 }, false),
    ({ pins₀ with receipt_command := 9 }, false),
    ({ pins₀ with receipt_generation := 5#u64 }, false),
    ({ pins₀ with record_attempt := 2#u32 }, false),
    ({ pins₀ with state_generation := 5#u64 }, false),
    ({ pins₀ with state_cancelled := true }, false),
    ({ pins₀ with consumed := none }, false),
    ({ pins₀ with consumed := some ⟨9, 4#u64⟩ }, false),
    ({ pins₀ with consumed := some ⟨3, 5#u64⟩ }, false),
    ({ pins₀ with accepted := none }, false),
    ({ pins₀ with accepted := some ⟨9, 2, 4#u64⟩ }, false),
    ({ pins₀ with accepted := some ⟨1, 9, 4#u64⟩ }, false),
    ({ pins₀ with accepted := some ⟨1, 2, 5#u64⟩ }, false) ]

theorem receipt_vectors :
    receiptVectors.all (fun v => boolIs (pins_bind pinEq pinEq v.1) v.2) = true := by
  decide

/-- Whether a production commit result is the expected written one. -/
def commitIs (r : Result (core.result.Result Unit CommitPinRefusal))
    (e : Except CommitRefusal Unit) : Bool :=
  match r, e with
  | ok (.Ok ()), .ok () => true
  | ok (.Err .ManifestMismatch), .error .manifestMismatch => true
  | ok (.Err .RunMismatch), .error .runMismatch => true
  | _, _ => false

/-- A receipt that binds its commit. -/
def commit₀ : CommitPins Nat Nat where
  manifest := 1
  declared_manifest := 1
  run := 2
  target := 2
  run_generation := 4#u64
  record_generation := 4#u64
  state_generation := 4#u64

/-- The binding commit, each pin broken, and the manifest refusal winning. -/
def commitVectors : List (CommitPins Nat Nat × Except CommitRefusal Unit) :=
  [ (commit₀, .ok ()),
    ({ commit₀ with declared_manifest := 9 }, .error .manifestMismatch),
    ({ commit₀ with declared_manifest := 9, target := 9 }, .error .manifestMismatch),
    ({ commit₀ with declared_manifest := 9, state_generation := 5#u64 },
      .error .manifestMismatch),
    ({ commit₀ with target := 9 }, .error .runMismatch),
    ({ commit₀ with record_generation := 5#u64 }, .error .runMismatch),
    ({ commit₀ with state_generation := 5#u64 }, .error .runMismatch) ]

theorem commit_vectors :
    commitVectors.all (fun v => commitIs (commit_pins_bind pinEq pinEq v.1) v.2) = true := by
  decide

end Norn.Kernel.Binding

#print axioms Norn.Kernel.Binding.pins_bind_correspondence
#print axioms Norn.Kernel.Binding.pins_bind_binds
#print axioms Norn.Kernel.Binding.commit_pins_bind_correct
#print axioms Norn.Kernel.Binding.commit_pins_bind_ok_iff
#print axioms Norn.Kernel.Binding.receipt_vectors
#print axioms Norn.Kernel.Binding.commit_vectors
