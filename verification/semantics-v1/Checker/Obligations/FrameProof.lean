import FrameCore.Funs
import Checker.Certificates.Support
import Checker.Obligations.InductionReference

/-!
# Production frame decision meets the frame lemma

`FrameCore` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/obligations/induction/frame.rs`, generic over the field
type `F` and its `PartialEq` instance, assumed structural (`LawfulEq`), as the
derived `PartialEq` of `String` is.

* `discharges_correct`: for every input, the production decision returns
  (never panics or diverges) exactly: the reads are known, the writer's type is
  the law's subject, and the written field is not read.
* `discharges_sound`: a discharged preservation goal holds for every law that
  depends only on its reads and every step that writes only that field; with
  initialization, inductive safety then covers every reachable state.
-/

namespace Norn.Checker.Obligations
open Aeneas Aeneas.Std Result
open Norn.Checker.Certificates

variable {F : Type}

/-- The instance's `eq` is structural equality and never fails. -/
def LawfulEq (inst : core.cmp.PartialEq F F) : Prop :=
  ∀ a b, ∃ r, inst.eq a b = ok r ∧ (r = true ↔ a = b)

variable {inst : core.cmp.PartialEq F F}

theorem frame_same_ok (hEq : LawfulEq inst) (a b : F) :
    ∃ r, norn_checker_frame.same inst a b = ok r ∧ (r = true ↔ a = b) := by
  obtain ⟨r, hr, hiff⟩ := hEq a b
  exact ⟨r, by simp [norn_checker_frame.same, hr], hiff⟩

theorem reads_field_ok (hEq : LawfulEq inst) (reads : Slice F) (field : F) :
    ∃ b, norn_checker_frame.reads_field inst reads field = ok b ∧ (b = true ↔ field ∈ reads.val) := by
  have H := loop_total (norn_checker_frame.reads_field_loop.body inst reads field)
    (fun i => reads.length - i.val)
    (fun i => i.val ≤ reads.length ∧ ∀ k (hk : k < reads.length), k < i.val → reads.val[k] ≠ field)
    (fun b => b = true ↔ field ∈ reads.val) ?_ 0#usize
    ⟨by simp, fun k _ hk => absurd hk (Nat.not_lt_zero _)⟩
  · exact H
  intro i ⟨hle, hnot⟩
  unfold norn_checker_frame.reads_field_loop.body
  dsimp only
  split
  · rename_i hlt
    simp only [UScalar.lt_equiv, Slice.len_val] at hlt
    rw [index_of_lt reads i hlt, bind_tc_ok]
    obtain ⟨b, hb, hbiff⟩ := frame_same_ok hEq (reads.val[i.val]'hlt) field
    rw [hb, bind_tc_ok]
    cases b
    · rw [if_neg Bool.false_ne_true]
      have hne : reads.val[i.val]'hlt ≠ field := fun e => Bool.false_ne_true (hbiff.mpr e)
      obtain ⟨j, hj, hjv⟩ := usize_succ_ok i (Nat.lt_of_lt_of_le hlt reads.property)
      rw [hj, bind_tc_ok]
      refine ⟨_, rfl, ⟨by omega, fun k hk hkj => ?_⟩, by omega⟩
      by_cases hki : k < i.val
      · exact hnot k hk hki
      · have : k = i.val := by omega
        subst this; exact hne
    · rw [if_pos rfl]
      refine ⟨_, rfl, ?_⟩
      show true = true ↔ field ∈ reads.val
      simp only [true_iff]
      rw [← hbiff.mp rfl]
      exact List.getElem_mem hlt
  · rename_i hge
    simp only [UScalar.lt_equiv, Slice.len_val, not_lt] at hge
    refine ⟨_, rfl, ?_⟩
    show false = true ↔ field ∈ reads.val
    simp only [Bool.false_eq_true, false_iff]
    intro hm
    obtain ⟨k, hk, hkv⟩ := List.getElem_of_mem hm
    exact hnot k hk (Nat.lt_of_lt_of_le hk hge) hkv

/-- Total correctness of the production frame decision. -/
theorem discharges_correct (hEq : LawfulEq inst) (subject object written : F)
    (reads : Option (Slice F)) :
    ∃ b, norn_checker_frame.discharges inst subject object written reads = ok b ∧
      (b = true ↔ ∃ fs, reads = some fs ∧ object = subject ∧ written ∉ fs.val) := by
  unfold norn_checker_frame.discharges
  cases reads with
  | none => exact ⟨false, rfl, by simp⟩
  | some fs =>
    dsimp only
    obtain ⟨b, hb, hbiff⟩ := frame_same_ok hEq object subject
    rw [hb, bind_tc_ok]
    cases b
    · rw [if_neg Bool.false_ne_true]
      refine ⟨false, rfl, ?_⟩
      simp only [Bool.false_eq_true, false_iff, not_exists, not_and]
      intro fs' hfs' hos _
      exact Bool.false_ne_true (hbiff.mpr hos)
    · rw [if_pos rfl]
      obtain ⟨b1, hb1, hb1iff⟩ := reads_field_ok hEq fs written
      rw [hb1, bind_tc_ok]
      refine ⟨_, rfl, ?_⟩
      have hos := hbiff.mp rfl
      cases b1 <;> simp_all

/-- Soundness of a discharged preservation goal: the frame lemma applies. -/
theorem discharges_sound (hEq : LawfulEq inst) {subject object written : F}
    {reads : Option (Slice F)}
    (h : norn_checker_frame.discharges inst subject object written reads = ok true) :
    ∃ fs, reads = some fs ∧ object = subject ∧
      ∀ {V : Type} (law : (F → V) → Prop) (pre post : F → V),
        DependsOnly law fs.val → Frames written pre post → law pre → law post := by
  obtain ⟨b, hb, hiff⟩ := discharges_correct hEq subject object written reads
  rw [h] at hb
  cases hb
  obtain ⟨fs, hfs, hos, hw⟩ := hiff.mp rfl
  exact ⟨fs, hfs, hos, fun law pre post hdep hframe hl => frame_preserves hdep hw hframe hl⟩

/-- Concrete verdicts evaluated by the kernel: unknown reads never discharge a
    goal, whatever the writer. Seeded mutants of that branch refute it. -/
theorem frame_witnesses :
    (match norn_checker_frame.discharges core.cmp.PartialEqBool true true true none with
      | ok false => true | _ => false) = true ∧
    (match norn_checker_frame.discharges core.cmp.PartialEqBool true false true none with
      | ok false => true | _ => false) = true := by
  decide

end Norn.Checker.Obligations

#print axioms Norn.Checker.Obligations.discharges_correct
#print axioms Norn.Checker.Obligations.discharges_sound
#print axioms Norn.Checker.Obligations.frame_witnesses
