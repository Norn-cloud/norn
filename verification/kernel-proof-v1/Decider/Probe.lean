import DeciderCore.Funs
import Decider.Reference

/-!
# Concrete probes of the production decider core

Each theorem runs the extracted production code on one concrete turn or ledger
and states the result the written semantics gives. On the production code they
hold by evaluation. A seeded mutant of `decider_fold.rs` must make one of them
false, and the closing `decide` then proves that proposition false: the gate
counts a mutant as detected only on that refutation, never on a proof that
merely fails to go through. This file imports only the extraction and the
written semantics, never the correspondence proofs.
-/

namespace Norn.Kernel.Decider.Probe
open Aeneas Aeneas.Std Result norn_kernel_decider

def okVal {α : Type} : Result α → Option α
  | .ok a => some a
  | _ => none

private theorem small_bound (n : Nat) (h : n < 16) : n < 2 ^ UScalarTy.Usize.numBits := by
  simp only [UScalarTy.Usize_numBits_eq]
  cases System.Platform.numBits_eq with
  | inl h32 => rw [h32]; omega
  | inr h64 => rw [h64]; omega

/-- A small index sum, as a value: lets `simp` run the production loop on a
    concrete ledger. -/
theorem checked_add_small (i j : Usize) (h : i.val + j.val < 16) :
    Usize.checked_add i j = some (Usize.ofNatCore (i.val + j.val) (small_bound _ h)) := by
  have hspec := Usize.checked_add_bv_spec i j
  cases hca : Usize.checked_add i j with
  | none =>
    rw [hca] at hspec
    rcases Usize.bounds_eq with hm | hm <;> rw [hm] at hspec <;> simp [U32.max_eq, U64.max_eq] at hspec <;> omega
  | some z =>
    rw [hca] at hspec
    obtain ⟨-, hz, -⟩ := hspec
    congr 1
    rw [UScalar.eq_equiv]
    simp [Usize.ofNatCore_val_eq, hz]

/-- A `u64` sum as a value: lets `simp` run the production envelope checks on
    concrete counts. -/
theorem u64_checked_add (x y : U64) :
    U64.checked_add x y =
      if h : x.val + y.val ≤ U64.max then
        some (U64.ofNatCore (x.val + y.val) (by simp [U64.max_eq] at h ⊢; omega))
      else none := by
  have hs := U64.checked_add_bv_spec x y
  cases hca : U64.checked_add x y with
  | none =>
    rw [hca] at hs
    rw [dif_neg (by omega)]
  | some z =>
    rw [hca] at hs
    obtain ⟨hle, hz, -⟩ := hs
    rw [dif_pos hle]
    congr 1
    rw [UScalar.eq_equiv]
    simp [U64.ofNatCore_val_eq, hz]

theorem u64_max_val : core.num.U64.MAX.val = 2 ^ 64 - 1 := by
  simp [core.num.U64.MAX, U64.rMax]

def trace : Slice Nat := ⟨[1, 2, 3], by scalar_tac⟩

/-- The decider every probe uses: its state records the events it evolved. -/
def recorderAt (D : Type) : Evolve D (List Nat) Nat where
  initial_state _ := ok []
  evolve _ s e := ok (s ++ [e])

/-- A ledger fold probe's decider. -/
abbrev recorder : Evolve Unit (List Nat) Nat := recorderAt Unit

/-- One concrete turn: the world, the decision, its counts and the caps. -/
structure Case where
  writable : Bool
  mayPerform : Bool
  decision : core.result.Result (Slice Nat) Nat
  counts : Counts
  caps : Counts

/-- The step driver's view of a concrete turn. -/
def stepper : StepTurn Case (Slice Nat) Nat (List Nat) Nat where
  EvolveInst := recorderAt Case
  writable c := ok c.writable
  may_perform c := ok c.mayPerform
  decide c _ := ok c.decision
  counts c _ := ok c.counts
  caps c := ok c.caps
  events _ d := ok d
  clone_state _ s := ok s

/-- Counts from three small literals. -/
def tally (events commands timers : U64) : Counts := ⟨events, commands, timers⟩

def capName : EnvelopeCap → Cap
  | .Events => .events
  | .Commands => .commands
  | .Timers => .timers

/-- A step outcome as the written semantics states it: the evolved state, or
    the refusal. -/
def project : Result (core.result.Result (List Nat × Slice Nat) (StepRefusal Nat)) →
    Option (Except (Refusal Nat) (List Nat))
  | ok (.Ok (s, _)) => some (.ok s)
  | ok (.Err .ReadOnlyWorld) => some (.error .readOnlyWorld)
  | ok (.Err (.Rejected r)) => some (.error (.rejected r))
  | ok (.Err (.EnvelopeExceeded cap n)) => some (.error (.envelopeExceeded (capName cap) n.val))
  | ok (.Err (.PerformOutsideMain n)) => some (.error (.performOutsideMain n.val))
  | _ => none

/-- Runs the production code on a concrete probe, then decides the equation it
    reaches. -/
macro "evaluate" : tactic => `(tactic| (
  simp only [tally, fold_events, replay_events, run_step, count_of, exceeded_cap,
    exceeded_turn_cap, turn_overflow, fold_events_loop, recorder, recorderAt, stepper, trace]
  repeat (simp (config := { failIfUnchanged := false }) [okVal, project, capName, lift,
      fold_events_loop.body, core.slice.Slice.get, Slice.getElem?_Usize_eq,
      checked_add_small, Usize.ofNatCore_val_eq]; rw [loop.eq_1])
  all_goals (simp (config := { failIfUnchanged := false }) [okVal, project, capName, lift,
    fold_events_loop.body, core.slice.Slice.get, Slice.getElem?_Usize_eq, checked_add_small,
    Usize.ofNatCore_val_eq, u64Max, u64_checked_add, u64_max_val, U64.ofNatCore_val_eq,
    U64.max_eq, U64.rMax])
  all_goals decide))

theorem fold_probe : okVal (fold_events recorder () [0] trace) = some [0, 1, 2, 3] := by
  evaluate

theorem replay_probe : okVal (replay_events recorder () trace) = some [1, 2, 3] := by
  evaluate

theorem step_world_probe :
    project (run_step stepper ⟨false, false, .Err 7, tally 9#u64 9#u64 9#u64,
      tally 1#u64 1#u64 1#u64⟩ [9]) = some (.error .readOnlyWorld) := by
  evaluate

theorem step_rejection_probe :
    project (run_step stepper ⟨true, false, .Err 7, tally 9#u64 9#u64 9#u64,
      tally 1#u64 1#u64 1#u64⟩ [9]) = some (.error (.rejected 7)) := by
  evaluate

theorem step_envelope_probe :
    project (run_step stepper ⟨true, false, .Ok trace, tally 2#u64 1#u64 0#u64,
      tally 1#u64 1#u64 1#u64⟩ [9]) = some (.error (.envelopeExceeded .events 2)) := by
  evaluate

theorem step_perform_probe :
    project (run_step stepper ⟨true, false, .Ok trace, tally 1#u64 1#u64 0#u64,
      tally 1#u64 1#u64 1#u64⟩ [9]) = some (.error (.performOutsideMain 1)) := by
  evaluate

theorem step_accept_probe :
    project (run_step stepper ⟨true, false, .Ok trace, tally 3#u64 0#u64 0#u64,
      tally 3#u64 3#u64 3#u64⟩ [9]) = some (.ok [9, 1, 2, 3]) := by
  evaluate

theorem cap_inclusive_probe :
    okVal (exceeded_cap (tally 1#u64 1#u64 1#u64) (tally 1#u64 1#u64 1#u64)) = some none := by
  evaluate

theorem cap_order_probe :
    (okVal (exceeded_cap (tally 1#u64 1#u64 1#u64) (tally 2#u64 2#u64 0#u64))).map
      (Option.map capName) = some (some .events) := by
  evaluate

theorem turn_overflow_probe :
    (okVal (exceeded_turn_cap (tally core.num.U64.MAX 0#u64 0#u64)
      (tally core.num.U64.MAX 0#u64 0#u64) (tally 1#u64 0#u64 0#u64))).map
      (Option.map fun e => (capName e.cap, e.actual.val)) = some (some (.events, u64Max)) := by
  evaluate

theorem turn_inclusive_probe :
    (okVal (exceeded_turn_cap (tally 1#u64 0#u64 0#u64) (tally 1#u64 0#u64 0#u64)
      (tally 0#u64 0#u64 0#u64))).map (Option.map fun e => (capName e.cap, e.actual.val)) =
      some none := by
  evaluate

end Norn.Kernel.Decider.Probe
