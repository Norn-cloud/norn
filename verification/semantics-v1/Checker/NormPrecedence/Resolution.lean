import Checker.NormPrecedence.AdmitsSound

/-!
# The extracted resolution check decides its written conditions

`resolution_holds` re-checks one context's untrusted resolution. Every
function it calls returns (without panic or divergence) a Boolean that is
`true` exactly when its written condition holds, given the shape `admits`
already established.
-/

set_option linter.unusedSimpArgs false
set_option linter.unnecessarySeqFocus false

namespace Norn.Checker.NormPrecedence
open Aeneas Aeneas.Std Result ControlFlow norn_checker_norm_decision

/-- `x` is listed. -/
def Mem (list : Slice Usize) (x : Nat) : Prop := ∃ y ∈ list.val, y.val = x

theorem listed_decides (list : Slice Usize) (x : Usize) :
    Decides (decision.listed list x) (Mem list x.val) := by
  have key := loop_any (body := decision.listed_loop.body list x) (n := list.length)
    (P := fun k => ∃ y ∈ list.val[k]?, y.val = x.val) ?step ?stop 0#usize
  · refine key.congr ⟨?_, ?_⟩
    · rintro ⟨k, -, -, y, hy, hyx⟩; exact ⟨y, List.mem_of_getElem? hy, hyx⟩
    · rintro ⟨y, hy, hyx⟩
      obtain ⟨k, hk⟩ := List.mem_iff_getElem?.mp hy
      exact ⟨k, Nat.zero_le _, (List.getElem?_eq_some_iff.mp hk).1, y, hk, hyx⟩
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i list.length list.property hi
    refine ⟨j, hjv, ?_⟩
    by_cases e : (list.val[i.val]).val = x.val
    · have e' : list.val[i.val] = x := (usize_eq _ _).mpr e
      simp [decision.listed_loop.body, (lt_len i list).mpr hi, sidx list i hi, e', e,
        List.getElem?_eq_getElem hi]
    · have e' : ¬ list.val[i.val] = x := fun h => e (by rw [h])
      simp [decision.listed_loop.body, (lt_len i list).mpr hi, sidx list i hi, e', e, hj,
        List.getElem?_eq_getElem hi]
  case stop =>
    intro i hi
    have : ¬ i < Slice.len list := by rw [lt_len]; omega
    simp [decision.listed_loop.body, this]

theorem indices_in_range_decides (list : Slice Usize) (n : Usize) :
    Decides (decision.indices_in_range list n) (∀ y ∈ list.val, y.val < n.val) := by
  have key := loop_all (body := decision.indices_in_range_loop.body list n) (n := list.length)
    (P := fun k => ∀ y ∈ list.val[k]?, y.val < n.val) ?step ?stop 0#usize
  · exact (key.congr from_zero).congr forall_getElem?
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i list.length list.property hi
    refine ⟨j, hjv, ?_⟩
    by_cases e : n.val ≤ (list.val[i.val]).val
    · simp [decision.indices_in_range_loop.body, (lt_len i list).mpr hi, sidx list i hi, e,
        ge_iff_le, scalar_le, List.getElem?_eq_getElem hi]
    · simp [decision.indices_in_range_loop.body, (lt_len i list).mpr hi, sidx list i hi, e, hj,
        ge_iff_le, scalar_le, List.getElem?_eq_getElem hi]; omega
  case stop =>
    intro i hi
    have : ¬ i < Slice.len list := by rw [lt_len]; omega
    simp [decision.indices_in_range_loop.body, this]

theorem context_in_range_decides (norms : Norms) (c : Usize) :
    Decides (decision.context_in_range norms c)
      (∀ k (hk : k < norms.length), c.val < (norms.val[k]).active.length) := by
  have key := loop_all (body := decision.context_in_range_loop.body norms c) (n := norms.length)
    (P := fun k => ∀ n ∈ norms.val[k]?, c.val < n.active.length) ?step ?stop 0#usize
  · refine (key.congr from_zero).congr ⟨fun h k hk => ?_, fun h k hk n hn => ?_⟩
    · exact h k hk _ (List.getElem?_eq_getElem hk)
    · obtain ⟨_, rfl⟩ := List.getElem?_eq_some_iff.mp hn; exact h k hk
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i norms.length norms.property hi
    refine ⟨j, hjv, ?_⟩
    by_cases e : (norms.val[i.val]).active.length ≤ c.val
    · simp [decision.context_in_range_loop.body, (lt_len i norms).mpr hi, sidx norms i hi, e,
        ge_iff_le, scalar_le, List.getElem?_eq_getElem hi]
    · simp [decision.context_in_range_loop.body, (lt_len i norms).mpr hi, sidx norms i hi, e, hj,
        ge_iff_le, scalar_le, List.getElem?_eq_getElem hi]
      exact Nat.lt_of_not_le e
  case stop =>
    intro i hi
    have : ¬ i < Slice.len norms := by rw [lt_len]; omega
    simp [decision.context_in_range_loop.body, this]

theorem modOf_eq (norms : Norms) {k : Nat} (hk : k < norms.length) :
    modOf norms k = toModality (norms.val[k]).modality := by
  simp [modOf, List.getElem?_eq_getElem hk]

theorem actOf_eq (norms : Norms) {k c : Nat} (hk : k < norms.length)
    (hc : c < (norms.val[k]).active.length) :
    actOf norms k c = (norms.val[k]).active.val[c] := by
  simp [actOf, List.getElem?_eq_getElem hk, List.getElem?_eq_getElem hc]

/-- A listed survivor has a certified priority over `l` and opposes it. -/
def DefeatedBy (norms : Norms) (reach : Reach) (survivors : Slice Usize) (l : Nat) : Prop :=
  ∃ y ∈ survivors.val, R reach y.val l = true ∧ (modOf norms y.val).opposed (modOf norms l)

theorem defeated_by_survivor_decides (norms : Norms) (edges : Edges) (reach : Reach)
    (contexts : Usize) (hS : Shaped norms edges reach contexts) (survivors : Slice Usize)
    (hsurv : ∀ y ∈ survivors.val, y.val < norms.length) (l : Usize) (hl : l.val < norms.length) :
    Decides (decision.defeated_by_survivor norms reach survivors l)
      (DefeatedBy norms reach survivors l.val) := by
  have key := loop_any (body := decision.defeated_by_survivor_loop.body norms reach survivors l)
    (n := survivors.length) (P := fun k => ∃ y ∈ survivors.val[k]?,
      R reach y.val l.val = true ∧ (modOf norms y.val).opposed (modOf norms l.val)) ?step ?stop 0#usize
  · refine key.congr ⟨?_, ?_⟩
    · rintro ⟨k, -, -, y, hy, hp⟩; exact ⟨y, List.mem_of_getElem? hy, hp⟩
    · rintro ⟨y, hy, hp⟩
      obtain ⟨k, hk⟩ := List.mem_iff_getElem?.mp hy
      exact ⟨k, Nat.zero_le _, (List.getElem?_eq_some_iff.mp hk).1, y, hk, hp⟩
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i survivors.length survivors.property hi
    refine ⟨j, hjv, ?_⟩
    have hw := hsurv _ (List.getElem_mem hi)
    simp only [decision.defeated_by_survivor_loop.body, (lt_len i survivors).mpr hi, if_true,
      sidx survivors i hi, bind_tc_ok, List.getElem?_eq_getElem hi, Option.mem_def,
      Option.some.injEq, exists_eq_left']
    generalize survivors.val[i.val] = w at hw ⊢
    have hrw : w.val < reach.length := by rw [hS.reach_len]; exact hw
    have hrl : l.val < reach.length := by rw [hS.reach_len]; exact hl
    have hwl := row_bound norms edges reach contexts hS hrw hrl
    have hmw := modOf_eq norms hw
    have hml := modOf_eq norms hl
    cases e1 : (reach.val[w.val]).val[l.val] <;>
      by_cases e2 : (toModality (norms.val[w.val]).modality).opposed
        (toModality (norms.val[l.val]).modality) <;>
      simp [sidx reach w hrw, vidxu _ l hwl, R_eq reach hrw hwl, e1, e2, sidx norms w hw,
        sidx norms l hl, opposed_eq, hmw, hml, hj]
  case stop =>
    intro i hi
    have : ¬ i < Slice.len survivors := by rw [lt_len]; omega
    simp [decision.defeated_by_survivor_loop.body, this]

/-- `i` is listed exactly when it is active and no listed survivor defeats it. -/
def SurvivesExactly (norms : Norms) (reach : Reach) (survivors : Slice Usize) (c i : Nat) : Prop :=
  Mem survivors i ↔ (actOf norms i c = true ∧ ¬ DefeatedBy norms reach survivors i)

theorem survives_exactly_decides (norms : Norms) (edges : Edges) (reach : Reach)
    (contexts : Usize) (hS : Shaped norms edges reach contexts) (survivors : Slice Usize)
    (hsurv : ∀ y ∈ survivors.val, y.val < norms.length) (c : Usize) (i : Usize)
    (hi : i.val < norms.length) (hc : c.val < (norms.val[i.val]).active.length) :
    Decides (decision.survives_exactly norms reach survivors c i)
      (SurvivesExactly norms reach survivors c.val i.val) := by
  obtain ⟨bd, hd, hbd⟩ :=
    defeated_by_survivor_decides norms edges reach contexts hS survivors hsurv i hi
  obtain ⟨bl, hl, hbl⟩ := listed_decides survivors i
  unfold SurvivesExactly
  rw [actOf_eq norms hi hc]
  cases ea : (norms.val[i.val]).active.val[c.val] <;> cases bd <;> cases bl <;>
    simp only [Bool.false_eq_true, false_iff, true_iff, iff_true, iff_false] at hbd hbl <;>
    simp [decision.survives_exactly, sidx norms i hi, active_in_eq _ _ hc, ea, hd, hl, hbd, hbl]

theorem survivors_stable_decides (norms : Norms) (edges : Edges) (reach : Reach)
    (contexts : Usize) (hS : Shaped norms edges reach contexts) (survivors : Slice Usize)
    (hsurv : ∀ y ∈ survivors.val, y.val < norms.length) (c : Usize)
    (hctx : ∀ k (hk : k < norms.length), c.val < (norms.val[k]).active.length) :
    Decides (decision.survivors_stable norms reach survivors c)
      (∀ k, k < norms.length → SurvivesExactly norms reach survivors c.val k) := by
  have key := loop_all (body := decision.survivors_stable_loop.body norms reach survivors c)
    (n := norms.length) (P := fun k => SurvivesExactly norms reach survivors c.val k)
    ?step ?stop 0#usize
  · exact key.congr from_zero
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i norms.length norms.property hi
    refine ⟨j, hjv, ?_⟩
    obtain ⟨b, hrun, hb⟩ := survives_exactly_decides norms edges reach contexts hS survivors hsurv
      c i hi (hctx _ hi)
    simp only [decision.survivors_stable_loop.body, (lt_len i norms).mpr hi, if_true, hrun,
      bind_tc_ok, hj]
    cases b
    · exact Or.inr ⟨fun h => absurd (hb.mpr h) (by simp), by simp⟩
    · exact Or.inl ⟨hb.mp rfl, by simp⟩
  case stop =>
    intro i hi
    have : ¬ i < Slice.len norms := by rw [lt_len]; omega
    simp [decision.survivors_stable_loop.body, this]

theorem obligations_survive_decides (norms : Norms) (survivors obligations : Slice Usize)
    (hobl : ∀ y ∈ obligations.val, y.val < norms.length) :
    Decides (decision.obligations_survive norms survivors obligations)
      (∀ y ∈ obligations.val, modOf norms y.val = .obligation ∧ Mem survivors y.val) := by
  have key := loop_all (body := decision.obligations_survive_loop.body norms survivors obligations)
    (n := obligations.length)
    (P := fun k => ∀ y ∈ obligations.val[k]?, modOf norms y.val = .obligation ∧ Mem survivors y.val)
    ?step ?stop 0#usize
  · exact (key.congr from_zero).congr forall_getElem?
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i obligations.length obligations.property hi
    refine ⟨j, hjv, ?_⟩
    have hy := hobl _ (List.getElem_mem hi)
    simp only [decision.obligations_survive_loop.body, (lt_len i obligations).mpr hi, if_true,
      sidx obligations i hi, bind_tc_ok, List.getElem?_eq_getElem hi, Option.mem_def,
      Option.some.injEq, forall_eq']
    generalize obligations.val[i.val] = y at hy ⊢
    obtain ⟨bl, hl, hbl⟩ := listed_decides survivors y
    rw [modOf_eq norms hy]
    cases em : (norms.val[y.val]).modality <;> cases bl <;>
      simp only [Bool.false_eq_true, false_iff, true_iff] at hbl <;>
      simp [sidx norms y hy, obliges_eq, em, toModality, hl, hbl, hj]
  case stop =>
    intro i hi
    have : ¬ i < Slice.len obligations := by rw [lt_len]; omega
    simp [decision.obligations_survive_loop.body, this]

theorem obligations_listed_decides (norms : Norms) (survivors obligations : Slice Usize)
    (hsurv : ∀ y ∈ survivors.val, y.val < norms.length) :
    Decides (decision.obligations_listed norms survivors obligations)
      (∀ y ∈ survivors.val, modOf norms y.val = .obligation → Mem obligations y.val) := by
  have key := loop_all (body := decision.obligations_listed_loop.body norms survivors obligations)
    (n := survivors.length)
    (P := fun k => ∀ y ∈ survivors.val[k]?, modOf norms y.val = .obligation → Mem obligations y.val)
    ?step ?stop 0#usize
  · exact (key.congr from_zero).congr forall_getElem?
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i survivors.length survivors.property hi
    refine ⟨j, hjv, ?_⟩
    have hy := hsurv _ (List.getElem_mem hi)
    simp only [decision.obligations_listed_loop.body, (lt_len i survivors).mpr hi, if_true,
      sidx survivors i hi, bind_tc_ok, List.getElem?_eq_getElem hi, Option.mem_def,
      Option.some.injEq, forall_eq']
    generalize survivors.val[i.val] = y at hy ⊢
    obtain ⟨bl, hl, hbl⟩ := listed_decides obligations y
    rw [modOf_eq norms hy]
    cases em : (norms.val[y.val]).modality <;> cases bl <;>
      simp only [Bool.false_eq_true, false_iff, true_iff] at hbl <;>
      simp [sidx norms y hy, obliges_eq, em, toModality, hl, hbl, hj]
  case stop =>
    intro i hi
    have : ¬ i < Slice.len survivors := by rw [lt_len]; omega
    simp [decision.obligations_listed_loop.body, this]

/-- A recorded defeat cites a listed survivor, an active loser and a certified
    priority between opposing norms. -/
def Cited (norms : Norms) (reach : Reach) (survivors : Slice Usize) (c w l : Nat) : Prop :=
  w < norms.length ∧ l < norms.length ∧ Mem survivors w ∧ actOf norms l c = true ∧
    R reach w l = true ∧ (modOf norms w).opposed (modOf norms l)

theorem defeat_cited_decides (norms : Norms) (edges : Edges) (reach : Reach)
    (contexts : Usize) (hS : Shaped norms edges reach contexts) (survivors : Slice Usize)
    (c : Usize) (hctx : ∀ k (hk : k < norms.length), c.val < (norms.val[k]).active.length)
    (w l : Usize) :
    Decides (decision.defeat_cited norms reach survivors c w l)
      (Cited norms reach survivors c.val w.val l.val) := by
  unfold Cited decision.defeat_cited
  by_cases hw : w.val < norms.length
  · by_cases hl : l.val < norms.length
    · have hrw : w.val < reach.length := by rw [hS.reach_len]; exact hw
      have hrl : l.val < reach.length := by rw [hS.reach_len]; exact hl
      have hwl := row_bound norms edges reach contexts hS hrw hrl
      have hc := hctx _ hl
      obtain ⟨bl, hrun, hbl⟩ := listed_decides survivors w
      rw [actOf_eq norms hl hc, R_eq reach hrw hwl, modOf_eq norms hw, modOf_eq norms hl]
      cases bl <;> simp only [Bool.false_eq_true, false_iff, true_iff] at hbl
      · simp [(lt_len w norms).mpr hw, (lt_len l norms).mpr hl, hrun, hbl, hw, hl]
      · cases ea : (norms.val[l.val]).active.val[c.val] <;>
          cases er : (reach.val[w.val]).val[l.val] <;>
          simp [(lt_len w norms).mpr hw, (lt_len l norms).mpr hl, hrun, hbl, hw, hl, ea, er,
            sidx norms l hl, sidx norms w hw, active_in_eq _ _ hc, sidx reach w hrw,
            vidx _ l hwl, vidxu _ l hwl, opposed_eq]
    · have : ¬ l < Slice.len norms := by rw [lt_len]; exact hl
      simp [(lt_len w norms).mpr hw, this, hl]
  · have : ¬ w < Slice.len norms := by rw [lt_len]; exact hw
    simp [this, hw]

theorem defeats_cited_decides (norms : Norms) (edges : Edges) (reach : Reach)
    (contexts : Usize) (hS : Shaped norms edges reach contexts) (survivors : Slice Usize)
    (defeats : Slice (Usize × Usize)) (c : Usize)
    (hctx : ∀ k (hk : k < norms.length), c.val < (norms.val[k]).active.length) :
    Decides (decision.defeats_cited norms reach survivors defeats c)
      (∀ d ∈ defeats.val, Cited norms reach survivors c.val d.1.val d.2.val) := by
  have key := loop_all (body := decision.defeats_cited_loop.body norms reach survivors defeats c)
    (n := defeats.length)
    (P := fun k => ∀ d ∈ defeats.val[k]?, Cited norms reach survivors c.val d.1.val d.2.val)
    ?step ?stop 0#usize
  · exact (key.congr from_zero).congr forall_getElem?
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i defeats.length defeats.property hi
    refine ⟨j, hjv, ?_⟩
    simp only [decision.defeats_cited_loop.body, (lt_len i defeats).mpr hi, if_true,
      sidx defeats i hi, bind_tc_ok, List.getElem?_eq_getElem hi, Option.mem_def,
      Option.some.injEq, forall_eq']
    generalize defeats.val[i.val] = d
    obtain ⟨w, l⟩ := d
    obtain ⟨b, hrun, hb⟩ := defeat_cited_decides norms edges reach contexts hS survivors c hctx w l
    cases b <;> simp only [Bool.false_eq_true, false_iff, true_iff] at hb <;> simp [hrun, hb, hj]
  case stop =>
    intro i hi
    have : ¬ i < Slice.len defeats := by rw [lt_len]; omega
    simp [decision.defeats_cited_loop.body, this]

theorem loser_recorded_decides (defeats : Slice (Usize × Usize)) (l : Usize) :
    Decides (decision.loser_recorded defeats l) (∃ d ∈ defeats.val, d.2.val = l.val) := by
  have key := loop_any (body := decision.loser_recorded_loop.body defeats l) (n := defeats.length)
    (P := fun k => ∃ d ∈ defeats.val[k]?, d.2.val = l.val) ?step ?stop 0#usize
  · refine key.congr ⟨?_, ?_⟩
    · rintro ⟨k, -, -, d, hd, hp⟩; exact ⟨d, List.mem_of_getElem? hd, hp⟩
    · rintro ⟨d, hd, hp⟩
      obtain ⟨k, hk⟩ := List.mem_iff_getElem?.mp hd
      exact ⟨k, Nat.zero_le _, (List.getElem?_eq_some_iff.mp hk).1, d, hk, hp⟩
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i defeats.length defeats.property hi
    refine ⟨j, hjv, ?_⟩
    simp only [decision.loser_recorded_loop.body, (lt_len i defeats).mpr hi, if_true,
      sidx defeats i hi, bind_tc_ok, List.getElem?_eq_getElem hi, Option.mem_def,
      Option.some.injEq, exists_eq_left']
    generalize defeats.val[i.val] = d
    obtain ⟨w, r⟩ := d
    by_cases e : r.val = l.val
    · have e' : r = l := (usize_eq _ _).mpr e
      simp [e', e]
    · have e' : ¬ r = l := fun h => e (by rw [h])
      simp [e', e, hj]
  case stop =>
    intro i hi
    have : ¬ i < Slice.len defeats := by rw [lt_len]; omega
    simp [decision.loser_recorded_loop.body, this]

/-- An active norm that does not survive is recorded as defeated. -/
def Recorded (survivors : Slice Usize) (defeats : Slice (Usize × Usize)) (i : Nat) : Prop :=
  Mem survivors i ∨ ∃ d ∈ defeats.val, d.2.val = i

theorem defeat_recorded_decides (survivors : Slice Usize) (defeats : Slice (Usize × Usize))
    (a : Bool) (i : Usize) :
    Decides (decision.defeat_recorded survivors defeats a i)
      (a = true → Recorded survivors defeats i.val) := by
  obtain ⟨bl, hl, hbl⟩ := listed_decides survivors i
  obtain ⟨br, hr, hbr⟩ := loser_recorded_decides defeats i
  unfold Recorded
  cases a <;> cases bl <;> cases br <;>
    simp only [Bool.false_eq_true, false_iff, true_iff] at hbl hbr <;>
    simp [decision.defeat_recorded, hl, hr, hbl, hbr]

theorem defeats_complete_decides (norms : Norms) (survivors : Slice Usize)
    (defeats : Slice (Usize × Usize)) (c : Usize)
    (hctx : ∀ k (hk : k < norms.length), c.val < (norms.val[k]).active.length) :
    Decides (decision.defeats_complete norms survivors defeats c)
      (∀ k, k < norms.length → actOf norms k c.val = true → Recorded survivors defeats k) := by
  have key := loop_all (body := decision.defeats_complete_loop.body norms survivors defeats c)
    (n := norms.length)
    (P := fun k => actOf norms k c.val = true → Recorded survivors defeats k) ?step ?stop 0#usize
  · exact key.congr from_zero
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i norms.length norms.property hi
    refine ⟨j, hjv, ?_⟩
    have hc := hctx _ hi
    obtain ⟨b, hrun, hb⟩ :=
      defeat_recorded_decides survivors defeats ((norms.val[i.val]).active.val[c.val]) i
    rw [actOf_eq norms hi hc]
    simp only [decision.defeats_complete_loop.body, (lt_len i norms).mpr hi, if_true,
      sidx norms i hi, active_in_eq _ _ hc, bind_tc_ok, hrun, hj]
    cases b
    · exact Or.inr ⟨fun h => absurd (hb.mpr h) (by simp), by simp⟩
    · exact Or.inl ⟨hb.mp rfl, by simp⟩
  case stop =>
    intro i hi
    have : ¬ i < Slice.len norms := by rw [lt_len]; omega
    simp [decision.defeats_complete_loop.body, this]

/-- Everything `resolution_holds` checks, as written conditions. -/
structure ResolutionChecks (norms : Norms) (reach : Reach) (c : Usize) (survivors obligations : Slice Usize)
    (conflicts : Usize) (defeats : Slice (Usize × Usize)) : Prop where
  no_conflicts : conflicts.val = 0
  context : ∀ k (hk : k < norms.length), c.val < (norms.val[k]).active.length
  survivors_in : ∀ y ∈ survivors.val, y.val < norms.length
  obligations_in : ∀ y ∈ obligations.val, y.val < norms.length
  stable : ∀ k, k < norms.length → SurvivesExactly norms reach survivors c.val k
  obligations_survive : ∀ y ∈ obligations.val, modOf norms y.val = .obligation ∧ Mem survivors y.val
  obligations_listed : ∀ y ∈ survivors.val, modOf norms y.val = .obligation → Mem obligations y.val
  cited : ∀ d ∈ defeats.val, Cited norms reach survivors c.val d.1.val d.2.val
  complete : ∀ k, k < norms.length → actOf norms k c.val = true → Recorded survivors defeats k

theorem resolution_holds_decides (norms : Norms) (edges : Edges) (reach : Reach)
    (contexts : Usize) (hS : Shaped norms edges reach contexts) (c : Usize)
    (survivors obligations : Slice Usize) (conflicts : Usize) (defeats : Slice (Usize × Usize)) :
    Decides (decision.resolution_holds norms reach c survivors obligations conflicts defeats)
      (ResolutionChecks norms reach c survivors obligations conflicts defeats) := by
  unfold decision.resolution_holds
  by_cases h0 : conflicts = 0#usize
  · simp only [h0, if_true]
    refine (Decides.and (context_in_range_decides norms c) fun hctx =>
      Decides.and (indices_in_range_decides survivors (Slice.len norms)) fun hsurv =>
      Decides.and (indices_in_range_decides obligations (Slice.len norms)) fun hobl =>
      Decides.and (survivors_stable_decides norms edges reach contexts hS survivors
        (by simpa using hsurv) c hctx) fun _ =>
      Decides.and (obligations_survive_decides norms survivors obligations
        (by simpa using hobl)) fun _ =>
      Decides.and (obligations_listed_decides norms survivors obligations
        (by simpa using hsurv)) fun _ =>
      Decides.and (defeats_cited_decides norms edges reach contexts hS survivors defeats c hctx)
        fun _ => defeats_complete_decides norms survivors defeats c hctx).congr ?_
    simp only [Slice.len_val]
    exact ⟨fun ⟨a, b, c, d, e, f, g, h⟩ => ⟨by simp, a, b, c, d, e, f, g, h⟩,
      fun ⟨_, a, b, c, d, e, f, g, h⟩ => ⟨a, b, c, d, e, f, g, h⟩⟩
  · simp only [h0, if_false]
    refine ⟨false, rfl, by simp; intro h; apply h0; rw [usize_eq]; exact h.no_conflicts⟩

end Norn.Checker.NormPrecedence
