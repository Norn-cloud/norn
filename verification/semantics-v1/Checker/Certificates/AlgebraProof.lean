import AlgebraCore.Funs
import Checker.Certificates.Support
import Checker.Certificates.AlgebraReference

/-!
# Production DBSP algebra check meets the written semantics

`AlgebraCore` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/metric/compute/certificate/decision.rs`.

* `check_correct`: for every operator list and change mode, the production
  `check` returns (never panics or diverges) exactly the written verdicts.
* `check_sound`: at every node it marks `Proved`, incremental maintenance is
  exact for every lawful interpretation, input and change (IVM correctness).
-/

namespace Norn.Checker.Certificates.Algebra
open Aeneas Aeneas.Std Result
open Norn.Checker.Certificates

/-- The written law of a production homomorphism. -/
def lawOf : norn_checker_algebra.Homomorphism → Law
  | .Select => .select
  | .Project => .project
  | .GroupSum => .groupSum
  | .MonoidSum => .monoidSum

/-- The written operator of a production operator. -/
def opOf : norn_checker_algebra.Operator → Op
  | .Input => .input
  | .Homomorphism dep law => .hom dep.val (law.map lawOf)
  | .Join l r c => .join l.val r.val (c.val.getD 0 0#i8).val (c.val.getD 1 0#i8).val
      (c.val.getD 2 0#i8).val
  | .Unknown => .unknown

/-- The written verdict of a production verdict. -/
def verdictOf : norn_checker_algebra.Verdict → Verdict
  | .Proved => .proved
  | .Refuted t => .refuted t.val
  | .OverBudget => .overBudget
  | .UndeclaredFold => .undeclaredFold
  | .MonoidUnderDeletions => .monoidUnderDeletions
  | .Unsupported => .unsupported
  | .InvalidDependency => .invalidDependency

theorem eq_proved (v : norn_checker_algebra.Verdict) :
    ∃ b, norn_checker_algebra.Verdict.Insts.CoreCmpPartialEqVerdict.eq v .Proved = ok b ∧
      (b = true ↔ verdictOf v = .proved) := by
  cases v <;>
    simp (config := { decide := true }) [norn_checker_algebra.Verdict.Insts.CoreCmpPartialEqVerdict.eq,
      norn_checker_algebra.Verdict.read_discriminant, verdictOf]

theorem proved_before_ok (results : Slice norn_checker_algebra.Verdict) (dep : Usize) :
    ∃ b, norn_checker_algebra.proved_before results dep = ok b ∧
      (b = true ↔ ∃ hd : dep.val < results.length,
        verdictOf (results.val[dep.val]'hd) = .proved) := by
  unfold norn_checker_algebra.proved_before
  dsimp only
  split
  · rename_i hd
    simp only [UScalar.lt_equiv, Slice.len_val] at hd
    rw [index_of_lt results dep hd, bind_tc_ok]
    obtain ⟨b, hb, hiff⟩ := eq_proved (results.val[dep.val]'hd)
    exact ⟨b, hb, by rw [hiff]; exact ⟨fun h => ⟨hd, h⟩, fun ⟨_, h⟩ => h⟩⟩
  · rename_i hd
    simp only [UScalar.lt_equiv, Slice.len_val] at hd
    exact ⟨false, rfl, by simp only [Bool.false_eq_true, false_iff]; exact fun ⟨h, _⟩ => hd h⟩

theorem homomorphism_ok (law : Option norn_checker_algebra.Homomorphism) (signed : Bool) :
    ∃ w, norn_checker_algebra.homomorphism law signed = ok w ∧
      verdictOf w = homVerdict (law.map lawOf) signed := by
  rcases law with _ | ⟨_ | _ | _ | _⟩ <;> cases signed <;> exact ⟨_, rfl, rfl⟩

theorem i8_bne_one (x : I8) : (x != 1#i8) = true ↔ x.val ≠ 1 := by
  rw [bne_iff_ne, ne_eq, IScalar.eq_equiv]
  simp

theorem join_ok (c : Std.Array I8 3#usize) :
    ∃ w, norn_checker_algebra.join c = ok w ∧
      verdictOf w = joinVerdict (c.val.getD 0 0#i8).val (c.val.getD 1 0#i8).val
        (c.val.getD 2 0#i8).val := by
  have hlen : c.val.length = 3 := c.property
  unfold norn_checker_algebra.join
  rw [array_index_of_lt c 0#usize (by simp [hlen]), bind_tc_ok]
  have g0 : c.val.getD 0 0#i8 = c.val[(0#usize).val]'(by simp [hlen]) := by
    simp [List.getD_eq_getElem?_getD, hlen]
  have g1 : c.val.getD 1 0#i8 = c.val[(1#usize).val]'(by simp [hlen]) := by
    simp [List.getD_eq_getElem?_getD, hlen]
  have g2 : c.val.getD 2 0#i8 = c.val[(2#usize).val]'(by simp [hlen]) := by
    simp [List.getD_eq_getElem?_getD, hlen]
  rw [g0, g1, g2]
  split
  · rename_i h0
    refine ⟨_, rfl, ?_⟩
    unfold joinVerdict
    rw [if_pos ((i8_bne_one _).mp h0)]
    rfl
  · rename_i h0
    have h0 : (c.val[(0#usize).val]'(by simp [hlen])).val = 1 := by
      by_contra hc; exact h0 ((i8_bne_one _).mpr hc)
    rw [array_index_of_lt c 1#usize (by simp [hlen]), bind_tc_ok]
    split
    · rename_i h1
      refine ⟨_, rfl, ?_⟩
      unfold joinVerdict
      rw [if_neg (not_not.mpr h0), if_pos ((i8_bne_one _).mp h1)]
      rfl
    · rename_i h1
      have h1 : (c.val[(1#usize).val]'(by simp [hlen])).val = 1 := by
        by_contra hc; exact h1 ((i8_bne_one _).mpr hc)
      rw [array_index_of_lt c 2#usize (by simp [hlen]), bind_tc_ok]
      split
      · rename_i h2
        refine ⟨_, rfl, ?_⟩
        unfold joinVerdict
        rw [if_neg (not_not.mpr h0), if_neg (not_not.mpr h1), if_pos ((i8_bne_one _).mp h2)]
        rfl
      · rename_i h2
        have h2 : (c.val[(2#usize).val]'(by simp [hlen])).val = 1 := by
          by_contra hc; exact h2 ((i8_bne_one _).mpr hc)
        refine ⟨_, rfl, ?_⟩
        unfold joinVerdict
        rw [if_neg (not_not.mpr h0), if_neg (not_not.mpr h1), if_neg (not_not.mpr h2)]
        rfl

/-- One node: given the verdicts of every earlier node, `node` returns the
    written verdict of node `i`. -/
theorem node_ok (results : Slice norn_checker_algebra.Verdict)
    (op : norn_checker_algebra.Operator) (signed : Bool) (ops : List Op) (i : Nat)
    (hlen : results.length = i)
    (hres : ∀ k (hk : k < results.length), verdictOf (results.val[k]'hk) = verdictAt ops signed k)
    (hop : ops[i]? = some (opOf op)) :
    ∃ w, norn_checker_algebra.node results op signed = ok w ∧
      verdictOf w = verdictAt ops signed i := by
  have hpb : ∀ dep : Usize, ∃ b, norn_checker_algebra.proved_before results dep = ok b ∧
      (b = true ↔ (dep.val < i ∧ verdictAt ops signed dep.val = .proved)) := by
    intro dep
    obtain ⟨b, hb, hiff⟩ := proved_before_ok results dep
    refine ⟨b, hb, hiff.trans ⟨fun ⟨hd, h⟩ => ⟨hlen ▸ hd, (hres _ hd) ▸ h⟩,
      fun ⟨hd, h⟩ => ⟨hlen ▸ hd, (hres _ (hlen ▸ hd)).trans h⟩⟩⟩
  rw [verdictAt, hop]
  cases op with
  | Input => exact ⟨_, rfl, rfl⟩
  | Unknown => exact ⟨_, rfl, rfl⟩
  | Homomorphism dep law =>
    obtain ⟨b, hb, hiff⟩ := hpb dep
    simp only [opOf]
    unfold norn_checker_algebra.node
    dsimp only
    rw [hb, bind_tc_ok]
    cases b
    · rw [if_neg Bool.false_ne_true]
      refine ⟨_, rfl, ?_⟩
      have hn : ¬ (dep.val < i ∧ verdictAt ops signed dep.val = .proved) :=
        fun h => Bool.false_ne_true (hiff.mpr h)
      by_cases hd : dep.val < i
      · rw [dif_pos hd, if_neg (fun h => hn ⟨hd, h⟩)]; rfl
      · rw [dif_neg hd]; rfl
    · rw [if_pos rfl]
      obtain ⟨hd, hp⟩ := hiff.mp rfl
      rw [dif_pos hd, if_pos hp]
      exact homomorphism_ok law signed
  | Join l r c =>
    obtain ⟨b, hb, hiff⟩ := hpb l
    simp only [opOf]
    unfold norn_checker_algebra.node
    dsimp only
    rw [hb, bind_tc_ok]
    cases b
    · rw [if_neg Bool.false_ne_true]
      refine ⟨_, rfl, ?_⟩
      have hn : ¬ (l.val < i ∧ verdictAt ops signed l.val = .proved) :=
        fun h => Bool.false_ne_true (hiff.mpr h)
      by_cases hd : l.val < i
      · rw [dif_pos hd, if_neg (fun h => hn ⟨hd, h⟩)]; rfl
      · rw [dif_neg hd]; rfl
    · rw [if_pos rfl]
      obtain ⟨hd, hp⟩ := hiff.mp rfl
      rw [dif_pos hd, if_pos hp]
      obtain ⟨b1, hb1, hiff1⟩ := hpb r
      rw [hb1, bind_tc_ok]
      cases b1
      · rw [if_neg Bool.false_ne_true]
        refine ⟨_, rfl, ?_⟩
        have hn : ¬ (r.val < i ∧ verdictAt ops signed r.val = .proved) :=
          fun h => Bool.false_ne_true (hiff1.mpr h)
        by_cases hdr : r.val < i
        · rw [dif_pos hdr, if_neg (fun h => hn ⟨hdr, h⟩)]; rfl
        · rw [dif_neg hdr]; rfl
      · rw [if_pos rfl]
        obtain ⟨hdr, hpr⟩ := hiff1.mp rfl
        rw [dif_pos hdr, if_pos hpr]
        exact join_ok c

theorem over_budget_ok (count : Usize) :
    ∃ v, norn_checker_algebra.over_budget count = ok v ∧
      v.val = List.replicate count.val .OverBudget := by
  let I : alloc.vec.Vec norn_checker_algebra.Verdict × Usize → Prop := fun x =>
    x.2.val ≤ count.val ∧ x.1.val = List.replicate x.2.val .OverBudget
  let Q : alloc.vec.Vec norn_checker_algebra.Verdict → Prop := fun v =>
    v.val = List.replicate count.val .OverBudget
  have H := loop_total (fun x => norn_checker_algebra.over_budget_loop.body count x.1 x.2)
    (fun x => count.val - x.2.val) I Q ?_ (alloc.vec.Vec.new _, 0#usize) ⟨by simp, by simp⟩
  · exact H
  rintro ⟨res, idx⟩ ⟨hle, hres⟩
  unfold norn_checker_algebra.over_budget_loop.body
  dsimp only at hres hle ⊢
  split
  · rename_i hlt
    simp only [UScalar.lt_equiv] at hlt
    have hpush := alloc.vec.Vec.push_spec res .OverBudget
      (by rw [hres, List.length_replicate]; have := count.hBounds; scalar_tac)
    obtain ⟨res1, hres1, hres1v⟩ := WP.spec_imp_exists hpush
    obtain ⟨idx1, hidx1, hidx1v⟩ := usize_succ_ok idx (by have := count.hBounds; scalar_tac)
    rw [hres1, bind_tc_ok, hidx1, bind_tc_ok]
    refine ⟨_, rfl, ⟨by dsimp only; omega, ?_⟩, by dsimp only; omega⟩
    rw [hres1v, hres, hidx1v, List.replicate_succ']
  · rename_i hge
    simp only [UScalar.lt_equiv, not_lt] at hge
    refine ⟨_, rfl, ?_⟩
    show res.val = _
    rw [hres, Nat.le_antisymm hle hge]

/-- Total correctness: for every operator list and change mode, the production
    `check` returns exactly the written verdicts, one per operator. -/
theorem check_correct (operators : Slice norn_checker_algebra.Operator) (signed : Bool) :
    ∃ v, norn_checker_algebra.check operators signed = ok v ∧
      v.val.map verdictOf = verdicts (operators.val.map opOf) signed := by
  let ops := operators.val.map opOf
  unfold norn_checker_algebra.check
  dsimp only
  split
  · rename_i hgt
    have hgt : 256 < operators.length := by
      simpa [norn_checker_algebra.OPERATOR_BUDGET] using hgt
    obtain ⟨v, hv, hvv⟩ := over_budget_ok (Slice.len operators)
    refine ⟨v, hv, ?_⟩
    rw [hvv]
    simp [verdicts, hgt, verdictOf, List.map_replicate]
  · rename_i hle
    have hle : ¬ 256 < operators.length := by
      simpa [norn_checker_algebra.OPERATOR_BUDGET] using hle
    let I : alloc.vec.Vec norn_checker_algebra.Verdict × Usize → Prop := fun x =>
      x.2.val ≤ operators.length ∧ x.1.val.length = x.2.val ∧
        ∀ k (hk : k < x.1.val.length), verdictOf (x.1.val[k]'hk) = verdictAt ops signed k
    let Q : alloc.vec.Vec norn_checker_algebra.Verdict → Prop := fun v =>
      v.val.map verdictOf = verdicts ops signed
    have H := loop_total
      (fun x => norn_checker_algebra.check_loop.body operators signed x.1 x.2)
      (fun x => operators.length - x.2.val) I Q ?_ (alloc.vec.Vec.new _, 0#usize)
      ⟨by simp, by simp, fun k hk => by simp at hk⟩
    · exact H
    rintro ⟨res, idx⟩ ⟨hle', hlen, hres⟩
    unfold norn_checker_algebra.check_loop.body
    dsimp only at hle' hlen hres ⊢
    split
    · rename_i hlt
      simp only [UScalar.lt_equiv, Slice.len_val] at hlt
      rw [index_of_lt operators idx hlt, bind_tc_ok]
      have hop : ops[idx.val]? = some (opOf (operators.val[idx.val]'hlt)) := by
        simp [ops, List.getElem?_map, List.getElem?_eq_getElem hlt]
      obtain ⟨w, hw, hwv⟩ := node_ok (alloc.vec.Vec.deref res) (operators.val[idx.val]'hlt)
        signed ops idx.val hlen (fun k hk => hres k hk) hop
      rw [hw, bind_tc_ok]
      have hpush := alloc.vec.Vec.push_spec res w
        (by have := operators.property; simp only [Slice.length] at hlt; omega)
      obtain ⟨res1, hres1, hres1v⟩ := WP.spec_imp_exists hpush
      obtain ⟨idx1, hidx1, hidx1v⟩ := usize_succ_ok idx
        (by have := operators.property; simp only [Slice.length] at hlt; omega)
      rw [hres1, bind_tc_ok, hidx1, bind_tc_ok]
      refine ⟨_, rfl, ⟨by dsimp only; omega, by simp [hres1v, hlen, hidx1v], fun k hk => ?_⟩,
        by dsimp only; omega⟩
      simp only [hres1v, List.length_append, List.length_singleton] at hk
      by_cases hkl : k < res.val.length
      · simp only [hres1v, List.getElem_append_left hkl]
        exact hres k hkl
      · have hk' : k = res.val.length := by omega
        subst hk'
        simp only [hres1v, List.getElem_append_right (Nat.le_refl _), Nat.sub_self,
          List.getElem_cons_zero]
        rw [hwv, hlen]
    · rename_i hge
      simp only [UScalar.lt_equiv, Slice.len_val, not_lt] at hge
      refine ⟨_, rfl, ?_⟩
      show res.val.map verdictOf = verdicts ops signed
      have hidx : idx.val = operators.length := Nat.le_antisymm hle' hge
      have hops : ops.length = operators.length := by simp [ops]
      unfold verdicts
      rw [if_neg (by rw [hops]; exact hle)]
      apply List.ext_getElem
      · simp [hlen, hidx, hops]
      · intro k h1 h2
        simp only [List.getElem_map, List.getElem_range]
        exact hres k (by simpa using h1)

/-- IVM soundness of the production check: at every node it marks `Proved`,
    for every lawful interpretation, input and change, evaluating on updated
    inputs equals the old value plus the maintained delta. -/
theorem check_sound {V : Type} [AddCommMonoid V]
    {operators : Slice norn_checker_algebra.Operator} {signed : Bool}
    {v : alloc.vec.Vec norn_checker_algebra.Verdict}
    (h : norn_checker_algebra.check operators signed = ok v)
    (I : Interp V) (hI : I.Lawful signed) (inp dinp : Nat → V) (i : Nat)
    (hi : v.val[i]? = some .Proved) :
    evalAt I (operators.val.map opOf) (fun k => inp k + dinp k) i =
      evalAt I (operators.val.map opOf) inp i + deltaAt I (operators.val.map opOf) inp dinp i := by
  obtain ⟨v', hv', hmap⟩ := check_correct operators signed
  rw [h] at hv'
  cases hv'
  apply incremental_sound I hI inp dinp i
  have hm := congrArg (fun l => l[i]?) hmap
  simp only [List.getElem?_map, hi, Option.map_some] at hm
  unfold verdicts at hm
  split at hm
  · simp [List.getElem?_replicate] at hm
    exact absurd hm.2 (by simp [verdictOf])
  · simp only [List.getElem?_map, List.length_map] at hm
    by_cases hi' : i < operators.val.length
    · rw [List.getElem?_range hi'] at hm
      simp only [Option.map_some, Option.some.injEq, verdictOf] at hm
      exact hm.symm
    · rw [List.getElem?_eq_none (by simpa using hi')] at hm
      simp at hm

/-- Concrete verdicts of the loop-free production pieces, evaluated by the
    kernel: a missing join cross term is refuted, the monoid sum is refused
    under deletions, an undeclared fold is never proved, and a dependency must
    be an earlier node. Seeded mutants of these pieces refute it. -/
theorem algebra_witnesses :
    (match norn_checker_algebra.join ⟨[1#i8, 1#i8, 0#i8], by scalar_tac⟩ with
      | ok (.Refuted t) => t.val == 2 | _ => false) = true ∧
    (match norn_checker_algebra.join ⟨[1#i8, 1#i8, 1#i8], by scalar_tac⟩ with
      | ok .Proved => true | _ => false) = true ∧
    (match norn_checker_algebra.homomorphism (some .MonoidSum) true with
      | ok .MonoidUnderDeletions => true | _ => false) = true ∧
    (match norn_checker_algebra.homomorphism (some .MonoidSum) false with
      | ok .Proved => true | _ => false) = true ∧
    (match norn_checker_algebra.homomorphism none false with
      | ok .UndeclaredFold => true | _ => false) = true ∧
    (match norn_checker_algebra.proved_before ⟨[], by scalar_tac⟩ 0#usize with
      | ok false => true | _ => false) = true := by
  decide

end Norn.Checker.Certificates.Algebra

#print axioms Norn.Checker.Certificates.Algebra.check_correct
#print axioms Norn.Checker.Certificates.Algebra.check_sound
#print axioms Norn.Checker.Certificates.Algebra.algebra_witnesses
