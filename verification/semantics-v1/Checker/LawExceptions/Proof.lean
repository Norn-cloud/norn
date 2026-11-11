import Shape.Funs
import NornSem.Obligation

/-!
# Production `law_exceptions` meets the exception-tree semantics (#718 U1)

`Shape` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/law_exceptions/shape.rs`. It is generic over the
identity type `T` and its `PartialEq` instance; the theorems hold for every `T`
whose `PartialEq` is equality (`Lawful`), as `str`'s is.

`treeWith law ids parents cond` is the `Norn.Sem.Law.Tree` the slices denote:
exception `k` has identity `ids[k]`, target `parents[k]` and condition `cond k`.

* `check_shape_correct`: for all slices of one length, `check_shape` returns
  (without panic or divergence) `Ok` exactly when every declaration is valid and
  the tree is `WellFormed`, for every choice of conditions.
* `lifts_correct`: `lifts` at depth `d` returns the semantic fold `Tree.lifts`
  with fuel `|ids| + 1 - d`, for every input of at most 64 exceptions.
* `check_tree_sound` / `fold_lifted`: an admitted tree is `Unambiguous`, and the
  production fold from the law decides lifting under its unique stable labelling.
-/

namespace Norn.Checker.LawExceptions
open Aeneas Aeneas.Std Result ControlFlow
open norn_checker_law_exceptions
open Norn.Sem.Law

/-- The identity type's `PartialEq` is equality (`str`'s is byte equality). -/
structure Lawful {T : Type} [DecidableEq T] (inst : core.cmp.PartialEq T T) : Prop where
  eq : ∀ a b, inst.eq a b = ok (decide (a = b))
  ne : ∀ a b, inst.ne a b = ok (!decide (a = b))

/-! ## Library glue -/

@[simp] theorem slice_get {α : Type} (s : Slice α) (i : Usize) :
    core.slice.Slice.get (core.slice.index.SliceIndexUsizeSlice α) s i = ok s.val[i.val]? := rfl

@[simp] theorem slice_get_mut {α : Type} (s : Slice α) (i : Usize) :
    core.slice.Slice.get_mut (core.slice.index.SliceIndexUsizeSlice α) s i =
      ok (s.val[i.val]?, s.set_opt i) := rfl

theorem loop_done {α β : Type} {body : α → Result (ControlFlow α β)} {x : α} {y : β}
    (h : body x = ok (done y)) : loop body x = ok y := by
  rw [loop.eq_1, h]

theorem loop_cont {α β : Type} {body : α → Result (ControlFlow α β)} {x x' : α}
    (h : body x = ok (cont x')) : loop body x = loop body x' := by
  rw [loop.eq_1, h]

theorem checked_succ (i : Usize) (h : i.val + 1 ≤ Usize.max) :
    ∃ z, Usize.checked_add i 1#usize = some z ∧ z.val = i.val + 1 := by
  have hs := Usize.checked_add_bv_spec i 1#usize
  cases hc : Usize.checked_add i 1#usize with
  | none =>
    rw [hc] at hs
    simp at hs
    omega
  | some z =>
    rw [hc] at hs
    exact ⟨z, rfl, by simpa using hs.2.1⟩

theorem usize_max_ge : 4294967295 ≤ Usize.max := by
  rcases Usize.bounds_eq with h | h <;> rw [h] <;> simp [U32.max_eq, U64.max_eq]

theorem slice_len_le {α : Type} (s : Slice α) : s.val.length ≤ Usize.max := s.property

/-- `r` is the first index below `n` satisfying `P`, or `none` when none does. -/
def FirstWith (n : ℕ) (P : ℕ → Prop) : Option Usize → Prop
  | some j => j.val < n ∧ P j.val ∧ ∀ k < j.val, ¬ P k
  | none => ∀ k < n, ¬ P k

/-- A search loop that stops at the first index satisfying `P` finds it. -/
theorem find_loop (body : Usize → Result (ControlFlow Usize (Option Usize))) (len : ℕ)
    (P : ℕ → Prop)
    (hstep : ∀ i : Usize, i.val < len → (∀ k < i.val, ¬ P k) →
      (P i.val ∧ body i = ok (done (some i))) ∨
        (¬ P i.val ∧ ∃ z : Usize, z.val = i.val + 1 ∧ body i = ok (cont z)))
    (hend : ∀ i : Usize, len ≤ i.val → body i = ok (done none)) (n : ℕ) :
    ∀ i : Usize, len - i.val = n → (∀ k < i.val, ¬ P k) →
      ∃ r, loop body i = ok r ∧ FirstWith len P r := by
  induction n with
  | zero =>
    intro i hn hpre
    refine ⟨none, loop_done (hend i (by omega)), fun k hk => hpre k ?_⟩
    omega
  | succ n ih =>
    intro i hn hpre
    rcases hstep i (by omega) hpre with ⟨hp, hb⟩ | ⟨hp, z, hz, hb⟩
    · exact ⟨some i, loop_done hb, by omega, hp, hpre⟩
    · obtain ⟨r, hr, hf⟩ := ih z (by omega) (fun k hk => by
        rcases Nat.lt_or_ge k i.val with h | h
        · exact hpre k h
        · have hk : k = i.val := by omega
          subst hk
          exact hp)
      exact ⟨r, (loop_cont hb).trans hr, hf⟩

theorem find_loop_from_zero (body : Usize → Result (ControlFlow Usize (Option Usize)))
    (len : ℕ) (P : ℕ → Prop)
    (hstep : ∀ i : Usize, i.val < len → (∀ k < i.val, ¬ P k) →
      (P i.val ∧ body i = ok (done (some i))) ∨
        (¬ P i.val ∧ ∃ z : Usize, z.val = i.val + 1 ∧ body i = ok (cont z)))
    (hend : ∀ i : Usize, len ≤ i.val → body i = ok (done none)) :
    ∃ r, loop body 0#usize = ok r ∧ FirstWith len P r :=
  find_loop body len P hstep hend _ 0#usize rfl (fun k hk => absurd hk (by simp))

theorem firstWith_none {n : ℕ} {P : ℕ → Prop} {r : Option Usize} (h : FirstWith n P r) :
    r = none ↔ ∀ k < n, ¬ P k := by
  cases r with
  | none => exact ⟨fun _ => h, fun _ => rfl⟩
  | some j =>
    obtain ⟨hj, hp, -⟩ := h
    exact ⟨fun e => (by cases e), fun hall => absurd hp (hall _ hj)⟩

/-! ## The tree the slices denote -/

/-- Exception `k` has identity `ids[k]`, target `parents[k]` and condition `cond k`. -/
def treeWith {σ : Type} (law : T) (ids parents : List T) (cond : ℕ → σ → Bool) : Tree T σ :=
  ⟨law, (ids.zip parents).mapIdx fun k ip => ⟨ip.1, ip.2, cond k⟩⟩

section Tree

variable {T σ : Type} (law : T) (ids parents : List T) (cond : ℕ → σ → Bool)

theorem treeWith_getElem? (k : ℕ) :
    (treeWith law ids parents cond).nodes[k]? =
      (ids[k]?.bind fun a => parents[k]?.map fun p => (⟨a, p, cond k⟩ : Exception T σ)) := by
  simp only [treeWith, List.getElem?_mapIdx, List.zip, List.getElem?_zipWith]
  cases ids[k]? <;> cases parents[k]? <;> rfl

theorem mem_treeWith (hp : parents.length = ids.length) {e : Exception T σ} :
    e ∈ (treeWith law ids parents cond).nodes ↔
      ∃ k, ∃ h : k < ids.length, e = ⟨ids[k], parents[k], cond k⟩ := by
  rw [List.mem_iff_getElem?]
  constructor
  · rintro ⟨k, hk⟩
    rw [treeWith_getElem?] at hk
    have hki : k < ids.length := by
      by_contra hc
      rw [List.getElem?_eq_none (by omega)] at hk
      cases hk
    refine ⟨k, hki, ?_⟩
    rw [List.getElem?_eq_getElem hki, List.getElem?_eq_getElem (by omega)] at hk
    simp at hk
    exact hk.symm
  · rintro ⟨k, hk, rfl⟩
    refine ⟨k, ?_⟩
    rw [treeWith_getElem?, List.getElem?_eq_getElem hk, List.getElem?_eq_getElem (by omega)]
    rfl

theorem treeWith_ids (hp : parents.length = ids.length) :
    (treeWith law ids parents cond).ids = ids := by
  apply List.ext_getElem?
  intro k
  simp only [Tree.ids, List.getElem?_map, treeWith_getElem?]
  cases h : ids[k]? with
  | none => rfl
  | some a =>
    have hk : k < ids.length := by
      by_contra hc
      rw [List.getElem?_eq_none (by omega)] at h
      cases h
    rw [List.getElem?_eq_getElem (show k < parents.length by omega)]
    rfl

theorem treeWith_root : (treeWith law ids parents cond).root = law := rfl

theorem treeWith_length : (treeWith law ids parents cond).nodes.length = min ids.length parents.length := by
  simp [treeWith]

end Tree

variable {T : Type} [DecidableEq T] {inst : core.cmp.PartialEq T T}

/-! ## `position` -/

theorem position_spec (hl : Lawful inst) (ids : Slice T) (a : T) :
    ∃ r, position inst ids a = ok r ∧
      FirstWith ids.val.length (fun k => ids.val[k]? = some a) r := by
  unfold position position_loop
  refine find_loop_from_zero _ _ _ (fun i hi _ => ?_) (fun i hi => ?_)
  · have hx : ids.val[i.val]? = some ids.val[i.val] := List.getElem?_eq_getElem hi
    by_cases hxa : ids.val[i.val] = a
    · exact Or.inl ⟨by rw [hx, hxa], by simp [position_loop.body, hx, hxa, hl.eq]⟩
    · obtain ⟨z, hz, hzv⟩ := checked_succ i (by have := slice_len_le ids; omega)
      refine Or.inr ⟨by rw [hx]; simpa using hxa, z, hzv, ?_⟩
      simp [position_loop.body, hx, hxa, hl.eq, hz, lift]
  · have hnone : ids.val[i.val]? = none := List.getElem?_eq_none hi
    simp [position_loop.body, hnone]

/-- A found element's first occurrence is its own index exactly when no earlier
    index holds it. -/
theorem position_self (hl : Lawful inst) (ids : Slice T) {i : ℕ} {x : T}
    (hx : ids.val[i]? = some x) :
    ∃ r, position inst ids x = ok r ∧ ((∃ j, r = some j ∧ j.val = i) ↔ ∀ k < i, ids.val[k]? ≠ some x) := by
  obtain ⟨r, hr, hf⟩ := position_spec hl ids x
  refine ⟨r, hr, ?_⟩
  cases r with
  | none =>
    have hi : i < ids.val.length := by
      by_contra hc
      rw [List.getElem?_eq_none (by omega)] at hx
      cases hx
    exact absurd hx (hf i hi)
  | some j =>
    obtain ⟨hj, hp, hmin⟩ := hf
    constructor
    · rintro ⟨j', hj', hjv⟩
      cases hj'
      rw [← hjv]
      exact hmin
    · intro hall
      refine ⟨j, rfl, ?_⟩
      rcases Nat.lt_trichotomy j.val i with h | h | h
      · exact absurd hp (hall _ h)
      · exact h
      · exact absurd hx (hmin i h)

/-! ## Slices as lists -/

theorem set_opt_some {α : Type} : ∀ (l : List α) (i : ℕ) (x : α), l.set_opt i (some x) = l.set i x
  | [], _, _ => by simp
  | _ :: _, 0, _ => by simp
  | hd :: tl, i + 1, x => by
    rw [List.set_cons_succ, ← set_opt_some tl i x]
    simp [List.set_opt]

theorem set_opt_none {α : Type} : ∀ (l : List α) (i : ℕ), l.set_opt i none = l
  | [], _ => by simp
  | hd :: tl, i => by
    unfold List.set_opt
    split_ifs <;> simp [set_opt_none tl]

theorem slice_set_opt_none {α : Type} (s : Slice α) (i : Usize) : s.set_opt i none = s :=
  Subtype.ext (by rw [Slice.set_opt_val_eq, set_opt_none])

/-- Writes at `index` and above leave entries below `index` alone. -/
theorem getElem?_set_of_lt {α : Type} (l : List α) {i k : ℕ} (x : α) (h : k < i) :
    (l.set i x)[k]? = l[k]? := by
  rw [List.getElem?_set]
  simp [show i ≠ k by omega]

/-! ## A loop that writes one entry per index -/

/-- A loop over `index` that, for each `index < len`, writes `value index` at
    `index` of its slice and then continues, ends with every entry below `len`
    written and the length kept. -/
theorem write_loop
    (body : Usize → Slice Bool → Result (ControlFlow (Usize × Slice Bool) (Slice Bool)))
    (len : ℕ) (value : ℕ → Bool)
    (hstep : ∀ (i : Usize) (s : Slice Bool), i.val < len → s.val.length = len →
      ∃ z : Usize, ∃ s' : Slice Bool, z.val = i.val + 1 ∧ s'.val = s.val.set i.val (value i.val) ∧
        body i s = ok (cont (z, s')))
    (hend : ∀ (i : Usize) (s : Slice Bool), len ≤ i.val → s.val.length = len →
      body i s = ok (done s)) (n : ℕ) :
    ∀ (i : Usize) (s : Slice Bool), len - i.val = n → s.val.length = len →
      ∃ s' : Slice Bool, loop (fun (i, s) => body i s) (i, s) = ok s' ∧ s'.val.length = len ∧
        ∀ k < len, s'.val[k]? = if k < i.val then s.val[k]? else some (value k) := by
  induction n with
  | zero =>
    intro i s hn hs
    refine ⟨s, loop_done (hend i s (by omega) hs), hs, fun k hk => ?_⟩
    rw [if_pos (by omega)]
  | succ n ih =>
    intro i s hn hs
    obtain ⟨z, s', hz, hs', hb⟩ := hstep i s (by omega) hs
    obtain ⟨t, ht, htl, htv⟩ := ih z s' (by omega) (by rw [hs', List.length_set, hs])
    refine ⟨t, (loop_cont hb).trans ht, htl, fun k hk => ?_⟩
    rw [htv k hk, hs']
    by_cases hki : k < i.val
    · rw [if_pos (by omega), if_pos hki, getElem?_set_of_lt _ _ hki]
    · by_cases hke : k = i.val
      · subst hke
        rw [if_pos (by omega), if_neg hki, List.getElem?_set_self (by omega)]
      · rw [if_neg (by omega), if_neg hki]

/-- `write_loop` for a loop whose state is `(slice, index)`. -/
theorem write_loop'
    (body : Slice Bool → Usize → Result (ControlFlow (Slice Bool × Usize) (Slice Bool)))
    (len : ℕ) (value : ℕ → Bool)
    (hstep : ∀ (i : Usize) (s : Slice Bool), i.val < len → s.val.length = len →
      ∃ z : Usize, ∃ s' : Slice Bool, z.val = i.val + 1 ∧ s'.val = s.val.set i.val (value i.val) ∧
        body s i = ok (cont (s', z)))
    (hend : ∀ (i : Usize) (s : Slice Bool), len ≤ i.val → s.val.length = len →
      body s i = ok (done s)) (n : ℕ) :
    ∀ (i : Usize) (s : Slice Bool), len - i.val = n → s.val.length = len →
      ∃ s' : Slice Bool, loop (fun (s, i) => body s i) (s, i) = ok s' ∧ s'.val.length = len ∧
        ∀ k < len, s'.val[k]? = if k < i.val then s.val[k]? else some (value k) := by
  induction n with
  | zero =>
    intro i s hn hs
    refine ⟨s, loop_done (hend i s (by omega) hs), hs, fun k hk => ?_⟩
    rw [if_pos (by omega)]
  | succ n ih =>
    intro i s hn hs
    obtain ⟨z, s', hz, hs', hb⟩ := hstep i s (by omega) hs
    obtain ⟨t, ht, htl, htv⟩ := ih z s' (by omega) (by rw [hs', List.length_set, hs])
    refine ⟨t, (loop_cont hb).trans ht, htl, fun k hk => ?_⟩
    rw [htv k hk, hs']
    by_cases hki : k < i.val
    · rw [if_pos (by omega), if_pos hki, getElem?_set_of_lt _ _ hki]
    · by_cases hke : k = i.val
      · subst hke
        rw [if_pos (by omega), if_neg hki, List.getElem?_set_self (by omega)]
      · rw [if_neg (by omega), if_neg hki]

/-! ## The fold -/

/-- Some exception overriding `v` has a holding condition and is not lifted in
    `level`. -/
def Over (parents : List T) (matched level : List Bool) (v : T) : Prop :=
  ∃ c, c < parents.length ∧ parents[c]? = some v ∧ matched[c]? = some true ∧ level[c]? = some false

/-- A loop that stops with `true` at the first index satisfying `P`. -/
theorem exists_loop (body : Usize → Result (ControlFlow Usize Bool)) (len : ℕ) (P : ℕ → Prop)
    (hstep : ∀ i : Usize, i.val < len →
      (P i.val ∧ body i = ok (done true)) ∨
        (¬ P i.val ∧ ∃ z : Usize, z.val = i.val + 1 ∧ body i = ok (cont z)))
    (hend : ∀ i : Usize, len ≤ i.val → body i = ok (done false)) (n : ℕ) :
    ∀ i : Usize, len - i.val = n →
      ∃ b, loop body i = ok b ∧ (b = true ↔ ∃ k, i.val ≤ k ∧ k < len ∧ P k) := by
  induction n with
  | zero =>
    intro i hn
    exact ⟨false, loop_done (hend i (by omega)), by simp; intro k h1 h2; omega⟩
  | succ n ih =>
    intro i hn
    rcases hstep i (by omega) with ⟨hp, hb⟩ | ⟨hp, z, hz, hb⟩
    · exact ⟨true, loop_done hb, by simp; exact ⟨i.val, le_rfl, by omega, hp⟩⟩
    · obtain ⟨b, hr, hiff⟩ := ih z (by omega)
      refine ⟨b, (loop_cont hb).trans hr, hiff.trans ?_⟩
      constructor
      · rintro ⟨k, hk1, hk2, hk⟩
        exact ⟨k, by omega, hk2, hk⟩
      · rintro ⟨k, hk1, hk2, hk⟩
        have hki : k ≠ i.val := fun e => hp (e ▸ hk)
        exact ⟨k, by omega, hk2, hk⟩

theorem overridden_spec (hl : Lawful inst) (node : T) (parents : Slice T)
    (matched level : Slice Bool) :
    ∃ b, overridden inst node parents matched level = ok b ∧
      (b = true ↔ Over parents.val matched.val level.val node) := by
  have hstep : ∀ i : Usize, i.val < parents.val.length →
      ((parents.val[i.val]? = some node ∧ matched.val[i.val]? = some true ∧
          level.val[i.val]? = some false) ∧
        overridden_loop.body inst node parents matched level i = ok (done true)) ∨
      (¬ (parents.val[i.val]? = some node ∧ matched.val[i.val]? = some true ∧
          level.val[i.val]? = some false) ∧
        ∃ z : Usize, z.val = i.val + 1 ∧
          overridden_loop.body inst node parents matched level i = ok (cont z)) := by
    intro i hi
    have hx : parents.val[i.val]? = some parents.val[i.val] := List.getElem?_eq_getElem hi
    obtain ⟨z, hz, hzv⟩ := checked_succ i (by have := slice_len_le parents; omega)
    by_cases hpn : parents.val[i.val] = node
    · cases hm : matched.val[i.val]? with
      | none =>
        exact Or.inr ⟨by simp, z, hzv,
          by simp [overridden_loop.body, hx, hpn, hm, hl.eq, hz, lift]⟩
      | some b1 =>
        cases hlv : level.val[i.val]? with
        | none =>
          exact Or.inr ⟨by simp, z, hzv,
            by simp [overridden_loop.body, hx, hpn, hm, hlv, hl.eq, hz, lift]⟩
        | some b2 =>
          cases b1 <;> cases b2
          · exact Or.inr ⟨by simp, z, hzv,
              by simp [overridden_loop.body, hx, hpn, hm, hlv, hl.eq, hz, lift]⟩
          · exact Or.inr ⟨by simp, z, hzv,
              by simp [overridden_loop.body, hx, hpn, hm, hlv, hl.eq, hz, lift]⟩
          · exact Or.inl ⟨⟨by rw [hx, hpn], rfl, rfl⟩,
              by simp [overridden_loop.body, hx, hpn, hm, hlv, hl.eq]⟩
          · exact Or.inr ⟨by simp, z, hzv,
              by simp [overridden_loop.body, hx, hpn, hm, hlv, hl.eq, hz, lift]⟩
    · exact Or.inr ⟨fun h => hpn (by rw [hx] at h; exact Option.some.inj h.1), z, hzv,
        by simp [overridden_loop.body, hx, hpn, hl.eq, hz, lift]⟩
  have hend : ∀ i : Usize, parents.val.length ≤ i.val →
      overridden_loop.body inst node parents matched level i = ok (done false) := by
    intro i hi
    have hnone : parents.val[i.val]? = none := List.getElem?_eq_none hi
    simp [overridden_loop.body, hnone]
  obtain ⟨b, hb, hiff⟩ := exists_loop _ _ (fun k => parents.val[k]? = some node ∧
    matched.val[k]? = some true ∧ level.val[k]? = some false) hstep hend _ 0#usize rfl
  refine ⟨b, hb, hiff.trans ?_⟩
  constructor
  · rintro ⟨k, -, hk, h⟩
    exact ⟨k, hk, h⟩
  · rintro ⟨k, hk, h⟩
    exact ⟨k, by simp, hk, h⟩

theorem next_level_spec (hl : Lawful inst) (ids parents : Slice T) (matched level out : Slice Bool)
    (ho : out.val.length = ids.val.length) :
    ∃ out', next_level inst ids parents matched level out = ok out' ∧
      out'.val.length = ids.val.length ∧
      ∀ k < ids.val.length, ∃ v b, ids.val[k]? = some v ∧ out'.val[k]? = some b ∧
        (b = true ↔ Over parents.val matched.val level.val v) := by
  classical
  let value : ℕ → Bool := fun k =>
    match ids.val[k]? with
    | some v => decide (Over parents.val matched.val level.val v)
    | none => false
  have hstep : ∀ (i : Usize) (s : Slice Bool), i.val < ids.val.length → s.val.length = ids.val.length →
      ∃ z : Usize, ∃ s' : Slice Bool, z.val = i.val + 1 ∧ s'.val = s.val.set i.val (value i.val) ∧
        next_level_loop.body inst ids parents matched level i s = ok (cont (z, s')) := by
    intro i s hi hs
    have hx : ids.val[i.val]? = some ids.val[i.val] := List.getElem?_eq_getElem hi
    have hsx : s.val[i.val]? = some s.val[i.val] := List.getElem?_eq_getElem (by omega)
    obtain ⟨b, hb, hiff⟩ := overridden_spec hl ids.val[i.val] parents matched level
    obtain ⟨z, hz, hzv⟩ := checked_succ i (by have := slice_len_le ids; omega)
    refine ⟨z, s.set_opt i (some b), hzv, ?_, ?_⟩
    · rw [Slice.set_opt_val_eq, set_opt_some]
      congr 1
      simp only [value, hx]
      cases b <;> simp_all
    · simp [next_level_loop.body, hx, hb, hsx, hz, lift]
  have hend : ∀ (i : Usize) (s : Slice Bool), ids.val.length ≤ i.val →
      s.val.length = ids.val.length →
      next_level_loop.body inst ids parents matched level i s = ok (done s) := by
    intro i s hi _
    have hnone : ids.val[i.val]? = none := List.getElem?_eq_none hi
    simp [next_level_loop.body, hnone]
  obtain ⟨s', hs', hlen, hval⟩ := write_loop _ _ value hstep hend _ 0#usize out rfl ho
  refine ⟨s', ?_, hlen, fun k hk => ?_⟩
  · unfold next_level next_level_loop
    exact hs'
  · have hx : ids.val[k]? = some ids.val[k] := List.getElem?_eq_getElem hk
    refine ⟨ids.val[k], value k, hx, ?_, ?_⟩
    · rw [hval k hk, if_neg (by simp)]
    · simp only [value, hx, decide_eq_true_eq]

theorem copy_level_spec (source target : Slice Bool) (h : target.val.length = source.val.length) :
    ∃ t, copy_level source target = ok t ∧ t.val = source.val := by
  let value : ℕ → Bool := fun k => source.val.getD k false
  have hstep : ∀ (i : Usize) (s : Slice Bool), i.val < source.val.length →
      s.val.length = source.val.length →
      ∃ z : Usize, ∃ s' : Slice Bool, z.val = i.val + 1 ∧ s'.val = s.val.set i.val (value i.val) ∧
        copy_level_loop.body source i s = ok (cont (z, s')) := by
    intro i s hi hs
    have hx : source.val[i.val]? = some source.val[i.val] := List.getElem?_eq_getElem hi
    have hsx : s.val[i.val]? = some s.val[i.val] := List.getElem?_eq_getElem (by omega)
    obtain ⟨z, hz, hzv⟩ := checked_succ i (by have := slice_len_le source; omega)
    refine ⟨z, s.set_opt i (some source.val[i.val]), hzv, ?_, ?_⟩
    · rw [Slice.set_opt_val_eq, set_opt_some]
      simp [value, List.getD_eq_getElem?_getD, hx]
    · simp [copy_level_loop.body, hx, hsx, hz, lift]
  have hend : ∀ (i : Usize) (s : Slice Bool), source.val.length ≤ i.val →
      s.val.length = source.val.length → copy_level_loop.body source i s = ok (done s) := by
    intro i s hi _
    have hnone : source.val[i.val]? = none := List.getElem?_eq_none hi
    simp [copy_level_loop.body, hnone]
  obtain ⟨s', hs', hlen, hval⟩ := write_loop _ _ value hstep hend _ 0#usize target rfl h
  refine ⟨s', by unfold copy_level copy_level_loop; exact hs', ?_⟩
  apply List.ext_getElem?
  intro k
  by_cases hk : k < source.val.length
  · rw [hval k hk, if_neg (by simp), List.getElem?_eq_getElem hk]
    simp [value, List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hk]
  · rw [List.getElem?_eq_none (by omega), List.getElem?_eq_none (by omega)]

theorem clear_level_spec (target : Slice Bool) :
    ∃ t, clear_level target = ok t ∧ t.val = List.replicate target.val.length false := by
  have hstep : ∀ (i : Usize) (s : Slice Bool), i.val < target.val.length →
      s.val.length = target.val.length →
      ∃ z : Usize, ∃ s' : Slice Bool, z.val = i.val + 1 ∧ s'.val = s.val.set i.val false ∧
        clear_level_loop.body i s = ok (cont (z, s')) := by
    intro i s hi hs
    have hsx : s.val[i.val]? = some s.val[i.val] := List.getElem?_eq_getElem (by omega)
    obtain ⟨z, hz, hzv⟩ := checked_succ i (by have := slice_len_le target; omega)
    refine ⟨z, s.set_opt i (some false), hzv, by rw [Slice.set_opt_val_eq, set_opt_some], ?_⟩
    simp [clear_level_loop.body, hsx, hz, lift]
  have hend : ∀ (i : Usize) (s : Slice Bool), target.val.length ≤ i.val →
      s.val.length = target.val.length →
      clear_level_loop.body i s = ok (done s) := by
    intro i s hi hs
    have hnone : s.val[i.val]? = none := List.getElem?_eq_none (by omega)
    simp [clear_level_loop.body, hnone, slice_set_opt_none]
  obtain ⟨s', hs', hlen, hval⟩ :=
    write_loop _ _ (fun _ => false) hstep hend _ 0#usize target rfl rfl
  refine ⟨s', by unfold clear_level clear_level_loop; exact hs', ?_⟩
  apply List.ext_getElem?
  intro k
  by_cases hk : k < target.val.length
  · rw [hval k hk, if_neg (by simp), List.getElem?_replicate]
    simp [hk]
  · rw [List.getElem?_eq_none (by omega), List.getElem?_eq_none (by simp; omega)]

/-- The fold's tree: conditions are the evaluated `matched` entries (a missing
    entry is false), at the one point `()`. -/
abbrev foldTree (law : T) (ids parents : Slice T) (matched : Slice Bool) : Tree T Unit :=
  treeWith law ids.val parents.val (fun k _ => matched.val.getD k false)

/-- Over a level holding the fold at fuel `f`, `Over` is the fold at fuel `f + 1`. -/
theorem over_iff (law v : T) (ids parents : Slice T) (matched : Slice Bool)
    (hp : parents.val.length = ids.val.length) (f : ℕ) :
    Over parents.val matched.val
        (ids.val.map fun x => (foldTree law ids parents matched).lifts () f x) v ↔
      (foldTree law ids parents matched).lifts () (f + 1) v = true := by
  rw [Tree.lifts_succ_iff]
  constructor
  · rintro ⟨c, hc, hpc, hmc, hlc⟩
    have hci : c < ids.val.length := by omega
    refine ⟨⟨ids.val[c], parents.val[c], fun _ => matched.val.getD c false⟩,
      (mem_treeWith _ _ _ _ hp).mpr ⟨c, hci, rfl⟩, ?_, ?_, ?_⟩
    · rw [List.getElem?_eq_getElem (by omega)] at hpc
      exact Option.some.inj hpc
    · simp [List.getD_eq_getElem?_getD, hmc]
    · rw [List.getElem?_map, List.getElem?_eq_getElem hci] at hlc
      simpa using hlc
  · rintro ⟨e, he, hov, hcond, hl⟩
    obtain ⟨c, hci, rfl⟩ := (mem_treeWith _ _ _ _ hp).mp he
    refine ⟨c, by omega, ?_, ?_, ?_⟩
    · rw [List.getElem?_eq_getElem (by omega)]
      exact congrArg some hov
    · cases hm : matched.val[c]? <;> simp_all [List.getD_eq_getElem?_getD]
    · rw [List.getElem?_map, List.getElem?_eq_getElem hci]
      simpa using hl

theorem lifted_loop_spec (hl : Lawful inst) (law : T) (ids parents : Slice T) (matched : Slice Bool)
    (hp : parents.val.length = ids.val.length) (m : ℕ) :
    ∀ (level next : Slice Bool) (round : Usize), ids.val.length - round.val = m →
      round.val ≤ ids.val.length →
      level.val = ids.val.map (fun x => (foldTree law ids parents matched).lifts () round.val x) →
      next.val.length = ids.val.length →
      ∃ b l' n', lifted_loop inst law ids parents matched level next round = ok (b, l', n') ∧
        b = (foldTree law ids parents matched).lifts () (ids.val.length + 1) law ∧
        l'.val = ids.val.map
          (fun x => (foldTree law ids parents matched).lifts () ids.val.length x) := by
  induction m with
  | zero =>
    intro level next round hm hr hlev hnext
    have hrn : round.val = ids.val.length := by omega
    obtain ⟨b, hb, hiff⟩ := overridden_spec hl law parents matched level
    refine ⟨b, level, next, ?_, ?_, by rw [hlev, hrn]⟩
    · have hnot : ¬ round < Slice.len ids := fun h => by
        have h' : round.val < (Slice.len ids).val := h
        simp at h'
        omega
      unfold lifted_loop
      rw [loop.eq_1]
      simp [lifted_loop.body, hnot, hb]
    · rw [hlev, hrn, over_iff law law ids parents matched hp] at hiff
      exact Bool.eq_iff_iff.mpr hiff
  | succ m ih =>
    intro level next round hm hr hlev hnext
    obtain ⟨n1, hn1, hn1len, hn1v⟩ := next_level_spec hl ids parents matched level next hnext
    have hlevlen : level.val.length = ids.val.length := by rw [hlev, List.length_map]
    obtain ⟨l1, hl1, hl1v⟩ := copy_level_spec n1 level (by rw [hlevlen, hn1len])
    obtain ⟨z, hz, hzv⟩ := checked_succ round (by have := slice_len_le ids; omega)
    have hnext1 : n1.val = ids.val.map
        (fun x => (foldTree law ids parents matched).lifts () (round.val + 1) x) := by
      apply List.ext_getElem?
      intro k
      by_cases hk : k < ids.val.length
      · obtain ⟨v, b, hv, hb, hiff⟩ := hn1v k hk
        rw [hb, List.getElem?_map, hv]
        simp only [Option.map_some, Option.some.injEq]
        rw [hlev, over_iff law v ids parents matched hp] at hiff
        exact Bool.eq_iff_iff.mpr hiff
      · rw [List.getElem?_eq_none (by omega), List.getElem?_eq_none (by simp; omega)]
    obtain ⟨b, l', n', hrun, hb, hl'⟩ :=
      ih l1 n1 z (by omega) (by omega) (by rw [hl1v, hnext1, hzv]) (by omega)
    refine ⟨b, l', n', ?_, hb, hl'⟩
    have hlt : round < Slice.len ids := by
      show round.val < (Slice.len ids).val
      simp
      omega
    unfold lifted_loop
    rw [loop_cont (x' := (l1, n1, z))]
    · unfold lifted_loop at hrun
      exact hrun
    · simp [lifted_loop.body, hlt, hn1, hl1, hz, lift]

/-- `lifted` returns the semantic fold of the tree at fuel `|ids| + 1`, for
    every input whose slices have one entry per exception. -/
theorem lifted_correct (hl : Lawful inst) (law : T) (ids parents : Slice T)
    (matched level next : Slice Bool) (hp : parents.val.length = ids.val.length)
    (hlv : level.val.length = ids.val.length) (hnx : next.val.length = ids.val.length) :
    ∃ b l' n', lifted inst law ids parents matched level next = ok (b, l', n') ∧
      b = (foldTree law ids parents matched).lifts () (ids.val.length + 1) law ∧
      l'.val = ids.val.map
        (fun x => (foldTree law ids parents matched).lifts () ids.val.length x) := by
  obtain ⟨l0, hl0, hl0v⟩ := clear_level_spec level
  have hz : l0.val = ids.val.map
      (fun x => (foldTree law ids parents matched).lifts () (0#usize).val x) := by
    apply List.ext_getElem?
    intro k
    rw [hl0v, hlv, List.getElem?_replicate, List.getElem?_map]
    by_cases hk : k < ids.val.length
    · rw [if_pos hk, List.getElem?_eq_getElem hk]
      simp [Tree.lifts]
    · rw [if_neg hk, List.getElem?_eq_none (by omega)]
      rfl
  obtain ⟨b, l', n', hrun, hb, hl'⟩ :=
    lifted_loop_spec hl law ids parents matched hp _ l0 next 0#usize rfl (by simp) hz hnx
  exact ⟨b, l', n', by unfold lifted; simp [hl0, hrun], hb, hl'⟩

/-! ## The shape check -/

/-- Declaration `k` is refused: invalid, the law's identity, or a repeat. -/
def Invalid (law : T) (ids : List T) (declared : List Bool) (k : ℕ) : Prop :=
  declared[k]? ≠ some true ∨ ids[k]? = some law ∨ ∃ j < k, ids[j]? = ids[k]?

theorem first_invalid_spec (hl : Lawful inst) (law : T) (ids : Slice T) (declared : Slice Bool) :
    ∃ r, first_invalid inst law ids declared = ok r ∧
      FirstWith ids.val.length (Invalid law ids.val declared.val) r := by
  have hstep : ∀ i : Usize, i.val < ids.val.length →
      (∀ k < i.val, ¬ Invalid law ids.val declared.val k) →
      (Invalid law ids.val declared.val i.val ∧
          first_invalid_loop.body inst law ids declared i = ok (done (some i))) ∨
        (¬ Invalid law ids.val declared.val i.val ∧ ∃ z : Usize, z.val = i.val + 1 ∧
          first_invalid_loop.body inst law ids declared i = ok (cont z)) := by
    intro i hi _
    have hx : ids.val[i.val]? = some ids.val[i.val] := List.getElem?_eq_getElem hi
    obtain ⟨r, hr, hfirst⟩ := position_self hl ids hx
    by_cases hnd : ∀ k < i.val, ids.val[k]? ≠ some ids.val[i.val]
    · obtain ⟨j, hj, hjv⟩ := hfirst.mpr hnd
      have hji : j = i := UScalar.eq_of_val_eq hjv
      subst hji
      cases hd : declared.val[j.val]? with
      | none =>
        exact Or.inl ⟨Or.inl (by simp [hd]), by simp [first_invalid_loop.body, hx, hr, hj, hd]⟩
      | some v =>
        cases v with
        | false =>
          exact Or.inl ⟨Or.inl (by simp [hd]), by simp [first_invalid_loop.body, hx, hr, hj, hd]⟩
        | true =>
          by_cases hxl : ids.val[j.val] = law
          · have hr' : position inst ids law = ok (some j) := by rw [← hxl, hr, hj]
            exact Or.inl ⟨Or.inr (Or.inl (by rw [hx, hxl])),
              by simp [first_invalid_loop.body, hx, hr', hd, hl.eq, hxl]⟩
          · obtain ⟨z, hz, hzv⟩ := checked_succ j (by have := slice_len_le ids; omega)
            refine Or.inr ⟨?_, z, hzv,
              by simp [first_invalid_loop.body, hx, hr, hj, hd, hl.eq, hxl, hz, lift]⟩
            rintro (h | h | ⟨k, hk, he⟩)
            · exact h hd
            · rw [hx] at h
              exact hxl (Option.some.inj h)
            · rw [hx] at he
              exact hnd k hk he
    · have hinv : Invalid law ids.val declared.val i.val := Or.inr (Or.inr (by
        by_contra hc
        exact hnd fun k hk he => hc ⟨k, hk, by rw [he, hx]⟩))
      refine Or.inl ⟨hinv, ?_⟩
      have hfb : ∀ j, r = some j → j ≠ i := fun j hj hji => hnd (hfirst.mp ⟨j, hj, by rw [hji]⟩)
      cases r with
      | none =>
        cases hd : declared.val[i.val]? <;> simp [first_invalid_loop.body, hx, hr, hd, hl.eq]
      | some j =>
        have hne := hfb j rfl
        cases hd : declared.val[i.val]? <;> simp [first_invalid_loop.body, hx, hr, hd, hl.eq, hne]
  have hend : ∀ i : Usize, ids.val.length ≤ i.val →
      first_invalid_loop.body inst law ids declared i = ok (done none) := by
    intro i hi
    have hnone : ids.val[i.val]? = none := List.getElem?_eq_none hi
    simp [first_invalid_loop.body, hnone]
  obtain ⟨r, hr, hf⟩ := find_loop_from_zero _ _ _ hstep hend
  exact ⟨r, by unfold first_invalid first_invalid_loop; exact hr, hf⟩

/-- Exception `k`'s target is neither the law nor a declared identity. -/
def Dangling (law : T) (ids parents : List T) (k : ℕ) : Prop :=
  ∃ p, parents[k]? = some p ∧ p ≠ law ∧ ∀ m < ids.length, ids[m]? ≠ some p

theorem first_dangling_spec (hl : Lawful inst) (law : T) (ids parents : Slice T) :
    ∃ r, first_dangling inst law ids parents = ok r ∧
      FirstWith parents.val.length (Dangling law ids.val parents.val) r := by
  have hstep : ∀ i : Usize, i.val < parents.val.length →
      (∀ k < i.val, ¬ Dangling law ids.val parents.val k) →
      (Dangling law ids.val parents.val i.val ∧
          first_dangling_loop.body inst law ids parents i = ok (done (some i))) ∨
        (¬ Dangling law ids.val parents.val i.val ∧ ∃ z : Usize, z.val = i.val + 1 ∧
          first_dangling_loop.body inst law ids parents i = ok (cont z)) := by
    intro i hi _
    have hx : parents.val[i.val]? = some parents.val[i.val] := List.getElem?_eq_getElem hi
    obtain ⟨z, hz, hzv⟩ := checked_succ i (by have := slice_len_le parents; omega)
    by_cases hpl : parents.val[i.val] = law
    · refine Or.inr ⟨fun ⟨p, hp, hne, _⟩ => hne (by
        rw [hx] at hp
        rw [← Option.some.inj hp]
        exact hpl), z, hzv, ?_⟩
      simp [first_dangling_loop.body, hx, hpl, hl.ne, hz, lift]
    · obtain ⟨r, hr, hf⟩ := position_spec hl ids parents.val[i.val]
      by_cases hin : ∀ m < ids.val.length, ids.val[m]? ≠ some parents.val[i.val]
      · have hrn : r = none := (firstWith_none hf).mpr hin
        subst hrn
        exact Or.inl ⟨⟨_, hx, hpl, hin⟩,
          by simp [first_dangling_loop.body, hx, hpl, hl.ne, hr, core.option.Option.is_none]⟩
      · have hrn : r ≠ none := fun e => hin ((firstWith_none hf).mp e)
        obtain ⟨j, rfl⟩ := Option.ne_none_iff_exists'.mp hrn
        refine Or.inr ⟨fun ⟨p, hp, _, hall⟩ => hin (by
          rw [hx] at hp
          rw [Option.some.inj hp]
          exact hall), z, hzv, ?_⟩
        simp [first_dangling_loop.body, hx, hpl, hl.ne, hr, core.option.Option.is_none, hz, lift]
  have hend : ∀ i : Usize, parents.val.length ≤ i.val →
      first_dangling_loop.body inst law ids parents i = ok (done none) := by
    intro i hi
    have hnone : parents.val[i.val]? = none := List.getElem?_eq_none hi
    simp [first_dangling_loop.body, hnone]
  obtain ⟨r, hr, hf⟩ := find_loop_from_zero _ _ _ hstep hend
  exact ⟨r, by unfold first_dangling first_dangling_loop; exact hr, hf⟩

omit [DecidableEq T] in
theorem nodup_getElem_inj {l : List T} (hnd : l.Nodup) {i j : ℕ} (hi : i < l.length)
    (hj : j < l.length) (h : l[i] = l[j]) : i = j := by
  by_contra hne
  rcases Nat.lt_or_gt_of_ne hne with hlt | hgt
  · exact List.pairwise_iff_getElem.mp hnd i j hi hj hlt h
  · exact List.pairwise_iff_getElem.mp hnd j i hj hi hgt h.symm

section Reach

variable {σ : Type} (law : T) (ids parents : List T) (cond : ℕ → σ → Bool)
  (hp : parents.length = ids.length) (hnd : ids.Nodup) (hroot : law ∉ ids)
include hp hnd hroot

omit [DecidableEq T] in
/-- A declared exception's depth is one more than its target's. -/
theorem reach_step {k : ℕ} (hk : k < ids.length) (d : ℕ) :
    (treeWith law ids parents cond).ReachesIn ids[k] d ↔
      ∃ d', d = d' + 1 ∧ (treeWith law ids parents cond).ReachesIn parents[k] d' := by
  constructor
  · intro h
    rcases h.inv with ⟨he, -⟩ | ⟨e, he, d', hid, hd, hr⟩
    · exact absurd (by
        have hm := List.getElem_mem hk
        rw [he] at hm
        exact hm) hroot
    · obtain ⟨c, hc, rfl⟩ := (mem_treeWith _ _ _ _ hp).mp he
      have hck : k = c := nodup_getElem_inj hnd hk hc hid
      subst hck
      exact ⟨d', hd, hr⟩
  · rintro ⟨d', rfl, hr⟩
    exact .step (e := ⟨ids[k], parents[k], cond k⟩) ((mem_treeWith _ _ _ _ hp).mpr ⟨k, hk, rfl⟩) hr

omit [DecidableEq T] hnd in
/-- The law is at depth zero only. -/
theorem reach_law (d : ℕ) : (treeWith law ids parents cond).ReachesIn law d ↔ d = 0 := by
  constructor
  · intro h
    rcases h.inv with ⟨-, hd⟩ | ⟨e, he, d', hid, -, -⟩
    · exact hd
    · obtain ⟨c, hc, rfl⟩ := (mem_treeWith _ _ _ _ hp).mp he
      exact absurd (by
        rw [hid]
        exact List.getElem_mem hc) hroot
  · rintro rfl
    exact .root

end Reach

theorem reaches_loop_spec (hl : Lawful inst) (law : T) (ids parents : Slice T)
    {σ : Type} (cond : ℕ → σ → Bool)
    (hp : parents.val.length = ids.val.length) (hnd : ids.val.Nodup) (hroot : law ∉ ids.val)
    (hdang : ∀ k < parents.val.length, ¬ Dangling law ids.val parents.val k)
    (hbound : ids.val.length ≤ 64) (m : ℕ) :
    ∀ node steps : Usize, ids.val.length + 1 - steps.val = m → ∀ _hn : node.val < ids.val.length,
      ∃ b, reaches_law_loop inst law ids parents node steps = ok b ∧
        (b = true ↔ ∃ d, (treeWith law ids.val parents.val cond).ReachesIn ids.val[node.val] d ∧
          d + steps.val ≤ ids.val.length + 1) := by
  induction m with
  | zero =>
    intro node steps hm hn
    refine ⟨false, ?_, ?_⟩
    · have hnot : ¬ steps ≤ Slice.len ids := fun h => by
        have h' : steps.val ≤ (Slice.len ids).val := h
        simp at h'
        omega
      unfold reaches_law_loop
      rw [loop.eq_1]
      simp [reaches_law_loop.body, hnot]
    · simp only [Bool.false_eq_true, false_iff, not_exists, not_and]
      intro d hd
      obtain ⟨d', rfl, -⟩ := (reach_step law ids.val parents.val cond hp hnd hroot hn d).mp hd
      omega
  | succ m ih =>
    intro node steps hm hn
    have hle : steps ≤ Slice.len ids := by
      show steps.val ≤ (Slice.len ids).val
      simp
      omega
    have hx : parents.val[node.val]? = some parents.val[node.val] :=
      List.getElem?_eq_getElem (by omega)
    by_cases hpl : parents.val[node.val] = law
    · refine ⟨true, ?_, ?_⟩
      · unfold reaches_law_loop
        rw [loop.eq_1]
        simp [reaches_law_loop.body, hle, hx, hpl, hl.eq]
      · simp only [true_iff]
        refine ⟨1, (reach_step law ids.val parents.val cond hp hnd hroot hn 1).mpr
          ⟨0, rfl, ?_⟩, by omega⟩
        rw [hpl]
        exact .root
    · have hin : ¬ ∀ m < ids.val.length, ids.val[m]? ≠ some parents.val[node.val] :=
        fun hall => hdang node.val (by omega) ⟨_, hx, hpl, hall⟩
      obtain ⟨r, hr, hf⟩ := position_spec hl ids parents.val[node.val]
      have hrn : r ≠ none := fun e => hin ((firstWith_none hf).mp e)
      obtain ⟨j, rfl⟩ := Option.ne_none_iff_exists'.mp hrn
      obtain ⟨hjl, hjv, -⟩ := hf
      obtain ⟨z, hz, hzv⟩ := checked_succ steps (by have := usize_max_ge; omega)
      obtain ⟨b, hb, hiff⟩ := ih j z (by omega) hjl
      refine ⟨b, ?_, hiff.trans ?_⟩
      · unfold reaches_law_loop
        rw [loop_cont (x' := (j, z))]
        · unfold reaches_law_loop at hb
          exact hb
        · simp [reaches_law_loop.body, hle, hx, hpl, hl.eq, hr, hz, lift]
      · have hpj : parents.val[node.val] = ids.val[j.val] := by
          rw [List.getElem?_eq_getElem hjl] at hjv
          exact (Option.some.inj hjv).symm
        rw [hzv]
        constructor
        · rintro ⟨d, hd, hdl⟩
          refine ⟨d + 1, (reach_step law ids.val parents.val cond hp hnd hroot hn _).mpr
            ⟨d, rfl, by rw [hpj]; exact hd⟩, by omega⟩
        · rintro ⟨d, hd, hdl⟩
          obtain ⟨d', rfl, hd'⟩ := (reach_step law ids.val parents.val cond hp hnd hroot hn d).mp hd
          exact ⟨d', by rw [← hpj]; exact hd', by omega⟩

theorem reaches_law_spec (hl : Lawful inst) (law : T) (ids parents : Slice T)
    {σ : Type} (cond : ℕ → σ → Bool)
    (hp : parents.val.length = ids.val.length) (hnd : ids.val.Nodup) (hroot : law ∉ ids.val)
    (hdang : ∀ k < parents.val.length, ¬ Dangling law ids.val parents.val k)
    (hbound : ids.val.length ≤ 64) (start : Usize) (hs : start.val < ids.val.length) :
    ∃ b, reaches_law inst law ids parents start = ok b ∧
      (b = true ↔ ∃ d, (treeWith law ids.val parents.val cond).ReachesIn ids.val[start.val] d) := by
  obtain ⟨b, hb, hiff⟩ :=
    reaches_loop_spec hl law ids parents cond hp hnd hroot hdang hbound _ start 0#usize rfl hs
  refine ⟨b, by unfold reaches_law; exact hb, hiff.trans ?_⟩
  constructor
  · rintro ⟨d, hd, -⟩
    exact ⟨d, hd⟩
  · rintro ⟨d, hd⟩
    have hids := treeWith_ids law ids.val parents.val cond hp
    have hlen := hd.le_length (by rw [hids]; exact hnd) (by rw [hids, treeWith_root]; exact hroot)
    rw [treeWith_length, hp, Nat.min_self] at hlen
    exact ⟨d, hd, by simp; omega⟩

/-- Exception `k` never reaches the law. -/
def Cyclic {σ : Type} (law : T) (ids parents : List T) (cond : ℕ → σ → Bool) (k : ℕ) : Prop :=
  ∀ h : k < ids.length, ¬ ∃ d, (treeWith law ids parents cond).ReachesIn ids[k] d

theorem first_cycle_spec (hl : Lawful inst) (law : T) (ids parents : Slice T)
    {σ : Type} (cond : ℕ → σ → Bool)
    (hp : parents.val.length = ids.val.length) (hnd : ids.val.Nodup) (hroot : law ∉ ids.val)
    (hdang : ∀ k < parents.val.length, ¬ Dangling law ids.val parents.val k)
    (hbound : ids.val.length ≤ 64) :
    ∃ r, first_cycle inst law ids parents = ok r ∧
      FirstWith parents.val.length (Cyclic law ids.val parents.val cond) r := by
  have hstep : ∀ i : Usize, i.val < parents.val.length →
      (∀ k < i.val, ¬ Cyclic law ids.val parents.val cond k) →
      (Cyclic law ids.val parents.val cond i.val ∧
          first_cycle_loop.body inst law ids parents i = ok (done (some i))) ∨
        (¬ Cyclic law ids.val parents.val cond i.val ∧ ∃ z : Usize, z.val = i.val + 1 ∧
          first_cycle_loop.body inst law ids parents i = ok (cont z)) := by
    intro i hi _
    have hlt : i < Slice.len parents := by
      show i.val < (Slice.len parents).val
      simp
      omega
    have hii : i.val < ids.val.length := by omega
    obtain ⟨b, hb, hiff⟩ := reaches_law_spec hl law ids parents cond hp hnd hroot hdang hbound i hii
    cases b with
    | true =>
      obtain ⟨z, hz, hzv⟩ := checked_succ i (by have := slice_len_le parents; omega)
      exact Or.inr ⟨fun hc => hc hii (hiff.mp rfl), z, hzv,
        by simp [first_cycle_loop.body, hlt, hb, hz, lift]⟩
    | false =>
      exact Or.inl ⟨fun _ hr => absurd (hiff.mpr hr) (by simp),
        by simp [first_cycle_loop.body, hlt, hb]⟩
  have hend : ∀ i : Usize, parents.val.length ≤ i.val →
      first_cycle_loop.body inst law ids parents i = ok (done none) := by
    intro i hi
    have hnot : ¬ i < Slice.len parents := fun h => by
      have h' : i.val < (Slice.len parents).val := h
      simp at h'
      omega
    simp [first_cycle_loop.body, hnot]
  obtain ⟨r, hr, hf⟩ := find_loop_from_zero _ _ _ hstep hend
  exact ⟨r, by unfold first_cycle first_cycle_loop; exact hr, hf⟩

/-! ## The shape check decides well-formedness -/

omit [DecidableEq T] in
theorem no_invalid_iff (law : T) (ids : List T) (declared : List Bool)
    (hd : declared.length = ids.length) :
    (∀ k < ids.length, ¬ Invalid law ids declared k) ↔
      (∀ b ∈ declared, b = true) ∧ ids.Nodup ∧ law ∉ ids := by
  constructor
  · intro h
    refine ⟨fun b hb => ?_, ?_, fun hlw => ?_⟩
    · obtain ⟨k, hk⟩ := List.mem_iff_getElem?.mp hb
      have hkl : k < ids.length := by
        by_contra hc
        rw [List.getElem?_eq_none (by omega)] at hk
        cases hk
      by_contra hne
      exact h k hkl (Or.inl (by rw [hk]; simpa using hne))
    · unfold List.Nodup
      rw [List.pairwise_iff_getElem]
      intro i j hi hj hij heq
      exact h j hj (Or.inr (Or.inr ⟨i, hij, by
        rw [List.getElem?_eq_getElem hi, List.getElem?_eq_getElem hj, heq]⟩))
    · obtain ⟨k, hk⟩ := List.mem_iff_getElem?.mp hlw
      have hkl : k < ids.length := by
        by_contra hc
        rw [List.getElem?_eq_none (by omega)] at hk
        cases hk
      exact h k hkl (Or.inr (Or.inl hk))
  · rintro ⟨hall, hnd, hlaw⟩ k hk (h | h | ⟨j, hj, he⟩)
    · apply h
      have hkd : k < declared.length := by omega
      rw [List.getElem?_eq_getElem hkd, hall _ (List.getElem_mem hkd)]
    · exact hlaw (List.mem_of_getElem? h)
    · rw [List.getElem?_eq_getElem (by omega), List.getElem?_eq_getElem hk] at he
      have := nodup_getElem_inj hnd (by omega) hk (Option.some.inj he)
      omega

/-- **Shape-check correspondence.** For every identity type with lawful
    equality and all slices of one length, the production `check_shape`
    returns (without panic or divergence) `Ok` exactly when every declaration is
    valid and the tree the slices denote is `WellFormed`, whatever the
    conditions. -/
theorem check_shape_correct (hl : Lawful inst) (law : T) (ids parents : Slice T)
    (declared : Slice Bool) (hp : parents.val.length = ids.val.length)
    (hd : declared.val.length = ids.val.length) {σ : Type} (cond : ℕ → σ → Bool) :
    ∃ r, check_shape inst law ids parents declared = ok r ∧
      (r = .Ok () ↔ (∀ b ∈ declared.val, b = true) ∧
        (treeWith law ids.val parents.val cond).WellFormed) := by
  have hmax : MAX_LAW_EXCEPTIONS.val = 64 := by
    unfold MAX_LAW_EXCEPTIONS
    simp
  have hids := treeWith_ids law ids.val parents.val cond hp
  have hlen : (treeWith law ids.val parents.val cond).nodes.length = ids.val.length := by
    rw [treeWith_length, hp, Nat.min_self]
  by_cases hbig : 64 < ids.val.length
  · have hgt : Slice.len ids > MAX_LAW_EXCEPTIONS := by
      show MAX_LAW_EXCEPTIONS.val < (Slice.len ids).val
      simp [hmax]
      omega
    refine ⟨.Err .Limit, by unfold check_shape; simp [hgt], ?_⟩
    simp only [reduceCtorEq, false_iff, not_and]
    intro _ wf
    have hb := wf.bounded
    rw [hlen] at hb
    simp [Tree.maxExceptions] at hb
    omega
  · have hsmall : ¬ Slice.len ids > MAX_LAW_EXCEPTIONS := fun h => by
      have h' : MAX_LAW_EXCEPTIONS.val < (Slice.len ids).val := h
      simp [hmax] at h'
      omega
    obtain ⟨ri, hri, hfi⟩ := first_invalid_spec hl law ids declared
    cases ri with
    | some i =>
      refine ⟨.Err (.InvalidDeclaration i), by unfold check_shape; simp [hsmall, hri], ?_⟩
      simp only [reduceCtorEq, false_iff, not_and]
      intro hall wf
      obtain ⟨hi, hinv, -⟩ := hfi
      have hnd := wf.nodup
      have hrf := wf.rootFresh
      rw [hids] at hnd hrf
      exact (no_invalid_iff law ids.val declared.val hd).mpr ⟨hall, hnd, hrf⟩ i hi hinv
    | none =>
      obtain ⟨hall, hnd, hlaw⟩ := (no_invalid_iff law ids.val declared.val hd).mp hfi
      obtain ⟨rd, hrd, hfd⟩ := first_dangling_spec hl law ids parents
      cases rd with
      | some i =>
        refine ⟨.Err (.DanglingOverride i), by unfold check_shape; simp [hsmall, hri, hrd], ?_⟩
        simp only [reduceCtorEq, false_iff, not_and]
        intro _ wf
        obtain ⟨hi, ⟨p, hpi, hpl, hpn⟩, -⟩ := hfd
        have hii : i.val < ids.val.length := by omega
        obtain ⟨d, hd'⟩ := wf.reaches _ ((mem_treeWith _ _ _ cond hp).mpr ⟨i.val, hii, rfl⟩)
        obtain ⟨d', -, hr'⟩ :=
          (reach_step law ids.val parents.val cond hp hnd hlaw hii d).mp hd'
        rw [List.getElem?_eq_getElem (by omega)] at hpi
        have hpe := Option.some.inj hpi
        rcases hr'.inv with ⟨hroot', -⟩ | ⟨e, he, -, hid, -, -⟩
        · exact hpl (by rw [← hpe]; exact hroot')
        · obtain ⟨c, hc, rfl⟩ := (mem_treeWith _ _ _ _ hp).mp he
          exact hpn c hc (by rw [List.getElem?_eq_getElem hc, ← hpe, hid])
      | none =>
        obtain ⟨rc, hrc, hfc⟩ :=
          first_cycle_spec hl law ids parents cond hp hnd hlaw hfd (by omega)
        cases rc with
        | some i =>
          refine ⟨.Err (.Cycle i), by unfold check_shape; simp [hsmall, hri, hrd, hrc], ?_⟩
          simp only [reduceCtorEq, false_iff, not_and]
          intro _ wf
          obtain ⟨hi, hcyc, -⟩ := hfc
          have hii : i.val < ids.val.length := by omega
          exact hcyc hii (wf.reaches _ ((mem_treeWith _ _ _ cond hp).mpr ⟨i.val, hii, rfl⟩))
        | none =>
          refine ⟨.Ok (), by unfold check_shape; simp [hsmall, hri, hrd, hrc], ?_⟩
          simp only [true_iff]
          refine ⟨hall, ⟨?_, by rw [hids]; exact hnd, by rw [hids]; exact hlaw, ?_⟩⟩
          · rw [hlen]
            simp [Tree.maxExceptions]
            omega
          · intro e he
            obtain ⟨c, hc, rfl⟩ := (mem_treeWith _ _ _ _ hp).mp he
            by_contra hno
            exact hfc c (by omega) (fun _ => hno)

theorem firstWith_unique {n : ℕ} {P : ℕ → Prop} {a b : Option Usize}
    (ha : FirstWith n P a) (hb : FirstWith n P b) : a = b := by
  cases a with
  | none =>
    cases b with
    | none => rfl
    | some j => exact absurd hb.2.1 (ha j.val hb.1)
  | some i =>
    cases b with
    | none => exact absurd ha.2.1 (hb i.val ha.1)
    | some j =>
      obtain ⟨hi, hpi, hmi⟩ := ha
      obtain ⟨hj, hpj, hmj⟩ := hb
      rcases Nat.lt_trichotomy i.val j.val with h | h | h
      · exact absurd hpi (hmj _ h)
      · exact congrArg some (UScalar.eq_of_val_eq h)
      · exact absurd hpj (hmi _ h)

/-- **Refusal correspondence.** The production `check_shape` refuses with the
    first failing check in the written order (the bound, then declarations,
    then targets, then cycles), naming that check's first offending
    declaration. -/
theorem check_shape_refusal (hl : Lawful inst) (law : T) (ids parents : Slice T)
    (declared : Slice Bool) (hp : parents.val.length = ids.val.length)
    (hd : declared.val.length = ids.val.length) {σ : Type} (cond : ℕ → σ → Bool) :
    ∃ r, check_shape inst law ids parents declared = ok r ∧
      (r = .Err .Limit ↔ 64 < ids.val.length) ∧
      (∀ i : Usize, r = .Err (.InvalidDeclaration i) ↔ ids.val.length ≤ 64 ∧
        FirstWith ids.val.length (Invalid law ids.val declared.val) (some i)) ∧
      (∀ i : Usize, r = .Err (.DanglingOverride i) ↔ ids.val.length ≤ 64 ∧
        FirstWith ids.val.length (Invalid law ids.val declared.val) none ∧
        FirstWith parents.val.length (Dangling law ids.val parents.val) (some i)) ∧
      (∀ i : Usize, r = .Err (.Cycle i) ↔ ids.val.length ≤ 64 ∧
        FirstWith ids.val.length (Invalid law ids.val declared.val) none ∧
        FirstWith parents.val.length (Dangling law ids.val parents.val) none ∧
        FirstWith parents.val.length (Cyclic law ids.val parents.val cond) (some i)) := by
  have hmax : MAX_LAW_EXCEPTIONS.val = 64 := by
    unfold MAX_LAW_EXCEPTIONS
    simp
  obtain ⟨ri, hri, hfi⟩ := first_invalid_spec hl law ids declared
  by_cases hbig : 64 < ids.val.length
  · have hgt : Slice.len ids > MAX_LAW_EXCEPTIONS := by
      show MAX_LAW_EXCEPTIONS.val < (Slice.len ids).val
      simp [hmax]
      omega
    refine ⟨.Err .Limit, by unfold check_shape; simp [hgt], by simp [hbig], fun i => ?_,
      fun i => ?_, fun i => ?_⟩
    · simp only [core.result.Result.Err.injEq, reduceCtorEq, false_iff, not_and]
      intro h
      exact absurd h (by omega)
    · simp only [core.result.Result.Err.injEq, reduceCtorEq, false_iff, not_and]
      intro h
      exact absurd h (by omega)
    · simp only [core.result.Result.Err.injEq, reduceCtorEq, false_iff, not_and]
      intro h
      exact absurd h (by omega)
  · have hsmall : ¬ Slice.len ids > MAX_LAW_EXCEPTIONS := fun h => by
      have h' : MAX_LAW_EXCEPTIONS.val < (Slice.len ids).val := h
      simp [hmax] at h'
      omega
    have hle : ids.val.length ≤ 64 := by omega
    cases ri with
    | some i0 =>
      refine ⟨.Err (.InvalidDeclaration i0), by unfold check_shape; simp [hsmall, hri],
        by simp [hbig], fun i => ?_, fun i => ?_, fun i => ?_⟩
      · constructor
        · intro h
          cases h
          exact ⟨hle, hfi⟩
        · rintro ⟨-, hf⟩
          rw [Option.some.inj (firstWith_unique hfi hf)]
      · simp only [core.result.Result.Err.injEq, reduceCtorEq, false_iff, not_and]
        intro _ hn
        exact absurd (firstWith_unique hfi hn) (by simp)
      · simp only [core.result.Result.Err.injEq, reduceCtorEq, false_iff, not_and]
        intro _ hn
        exact absurd (firstWith_unique hfi hn) (by simp)
    | none =>
      obtain ⟨-, hnd, hlaw⟩ := (no_invalid_iff law ids.val declared.val hd).mp hfi
      obtain ⟨rd, hrd, hfd⟩ := first_dangling_spec hl law ids parents
      cases rd with
      | some i0 =>
        refine ⟨.Err (.DanglingOverride i0),
          by unfold check_shape; simp [hsmall, hri, hrd], by simp [hbig], fun i => ?_,
          fun i => ?_, fun i => ?_⟩
        · simp only [core.result.Result.Err.injEq, reduceCtorEq, false_iff, not_and]
          intro _ hn
          exact absurd (firstWith_unique hfi hn) (by simp)
        · constructor
          · intro h
            cases h
            exact ⟨hle, hfi, hfd⟩
          · rintro ⟨-, -, hf⟩
            rw [Option.some.inj (firstWith_unique hfd hf)]
        · simp only [core.result.Result.Err.injEq, reduceCtorEq, false_iff, not_and]
          intro _ _ hn
          exact absurd (firstWith_unique hfd hn) (by simp)
      | none =>
        obtain ⟨rc, hrc, hfc⟩ :=
          first_cycle_spec hl law ids parents cond hp hnd hlaw hfd hle
        cases rc with
        | some i0 =>
          refine ⟨.Err (.Cycle i0), by unfold check_shape; simp [hsmall, hri, hrd, hrc],
            by simp [hbig], fun i => ?_, fun i => ?_, fun i => ?_⟩
          · simp only [core.result.Result.Err.injEq, reduceCtorEq, false_iff, not_and]
            intro _ hn
            exact absurd (firstWith_unique hfi hn) (by simp)
          · simp only [core.result.Result.Err.injEq, reduceCtorEq, false_iff, not_and]
            intro _ _ hn
            exact absurd (firstWith_unique hfd hn) (by simp)
          · constructor
            · intro h
              cases h
              exact ⟨hle, hfi, hfd, hfc⟩
            · rintro ⟨-, -, -, hf⟩
              rw [Option.some.inj (firstWith_unique hfc hf)]
        | none =>
          refine ⟨.Ok (), by unfold check_shape; simp [hsmall, hri, hrd, hrc], by simp [hbig],
            fun i => ?_, fun i => ?_, fun i => ?_⟩
          · simp only [reduceCtorEq, false_iff, not_and]
            intro _ hn
            exact absurd (firstWith_unique hfi hn) (by simp)
          · simp only [reduceCtorEq, false_iff, not_and]
            intro _ _ hn
            exact absurd (firstWith_unique hfd hn) (by simp)
          · simp only [reduceCtorEq, false_iff, not_and]
            intro _ _ _ hn
            exact absurd (firstWith_unique hfc hn) (by simp)

/-- **Soundness of the exception-tree admission check.** A tree the production
    `check_shape` admits has one meaning at every point, for any conditions:
    exactly one stable labelling, and the bounded fold decides lifting under it. -/
theorem check_shape_sound (hl : Lawful inst) (law : T) (ids parents : Slice T)
    (declared : Slice Bool) (hp : parents.val.length = ids.val.length)
    (hd : declared.val.length = ids.val.length)
    (hadmit : check_shape inst law ids parents declared = ok (.Ok ()))
    {σ : Type} (cond : ℕ → σ → Bool) :
    (treeWith law ids.val parents.val cond).Unambiguous := by
  obtain ⟨r, hr, hiff⟩ := check_shape_correct hl law ids parents declared hp hd cond
  rw [hadmit] at hr
  exact (hiff.mp (Result.ok.inj hr).symm).2.unambiguous

/-- **Compiled guard = defeasible semantics, for the production fold.** On a
    tree the production `check_shape` admits, the production `lifted` returns
    `true` exactly when the law is lifted under the tree's stable labelling at
    the evaluated point. -/
theorem lifted_sound (hl : Lawful inst) (law : T) (ids parents : Slice T)
    (declared matched level next : Slice Bool) (hp : parents.val.length = ids.val.length)
    (hd : declared.val.length = ids.val.length) (hlv : level.val.length = ids.val.length)
    (hnx : next.val.length = ids.val.length)
    (hadmit : check_shape inst law ids parents declared = ok (.Ok ()))
    {A : T → Prop} (hA : (foldTree law ids parents matched).Stable () A) :
    ∃ b l' n', lifted inst law ids parents matched level next = ok (b, l', n') ∧
      (b = true ↔ (foldTree law ids parents matched).LiftedBy A) := by
  obtain ⟨r, hr, hiff⟩ := check_shape_correct hl law ids parents declared hp hd
    (fun k (_ : Unit) => matched.val.getD k false)
  rw [hadmit] at hr
  have wf := (hiff.mp (Result.ok.inj hr).symm).2
  obtain ⟨b, l', n', hrun, hb, -⟩ :=
    lifted_correct hl law ids parents matched level next hp hlv hnx
  have hlen : (foldTree law ids parents matched).nodes.length = ids.val.length := by
    simp [foldTree, treeWith_length, hp]
  refine ⟨b, l', n', hrun, ?_⟩
  rw [hb, Tree.lifted_iff wf hA, hlen]
  rfl

/-! ## The applied set -/

theorem ancestors_loop_spec (hl : Lawful inst) (law : T) (ids parents : Slice T)
    (matched : Slice Bool) (hp : parents.val.length = ids.val.length) (hnd : ids.val.Nodup)
    (hroot : law ∉ ids.val)
    (hdang : ∀ k < parents.val.length, ¬ Dangling law ids.val parents.val k)
    (hbound : ids.val.length ≤ 64) (d : ℕ) :
    ∀ node steps : Usize, ∀ hn : node.val < ids.val.length,
      (foldTree law ids parents matched).ReachesIn ids.val[node.val] d →
      d + steps.val ≤ ids.val.length + 1 →
      ∃ b, ancestors_hold_loop inst law ids parents matched node steps = ok b ∧
        (b = true ↔ (foldTree law ids parents matched).AboveHolds () ids.val[node.val]) := by
  induction d with
  | zero =>
    intro node steps hn hd _
    obtain ⟨d', hd', -⟩ := (reach_step law ids.val parents.val _ hp hnd hroot hn 0).mp hd
    omega
  | succ d ih =>
    intro node steps hn hd hle
    obtain ⟨d', hdd, hpd⟩ := (reach_step law ids.val parents.val _ hp hnd hroot hn _).mp hd
    have hdd' : d' = d := by omega
    rw [hdd'] at hpd
    have hsle : steps ≤ Slice.len ids := by
      show steps.val ≤ (Slice.len ids).val
      simp
      omega
    have hx : parents.val[node.val]? = some parents.val[node.val] :=
      List.getElem?_eq_getElem (by omega)
    have hmem : (⟨ids.val[node.val], parents.val[node.val],
        fun _ => matched.val.getD node.val false⟩ : Exception T Unit) ∈
          (foldTree law ids parents matched).nodes :=
      (mem_treeWith _ _ _ _ hp).mpr ⟨node.val, hn, rfl⟩
    by_cases hpl : parents.val[node.val] = law
    · refine ⟨true, ?_, ?_⟩
      · unfold ancestors_hold_loop
        rw [loop.eq_1]
        simp [ancestors_hold_loop.body, hsle, hx, hpl, hl.eq]
      · simp only [true_iff]
        exact .top hmem hpl
    · have hin : ¬ ∀ m < ids.val.length, ids.val[m]? ≠ some parents.val[node.val] :=
        fun hall => hdang node.val (by omega) ⟨_, hx, hpl, hall⟩
      obtain ⟨r, hr, hf⟩ := position_spec hl ids parents.val[node.val]
      have hrn : r ≠ none := fun e => hin ((firstWith_none hf).mp e)
      obtain ⟨j, rfl⟩ := Option.ne_none_iff_exists'.mp hrn
      obtain ⟨hjl, hjv, -⟩ := hf
      have hpj : parents.val[node.val] = ids.val[j.val] := by
        rw [List.getElem?_eq_getElem hjl] at hjv
        exact (Option.some.inj hjv).symm
      have hjmem : (⟨ids.val[j.val], parents.val[j.val],
          fun _ => matched.val.getD j.val false⟩ : Exception T Unit) ∈
            (foldTree law ids parents matched).nodes :=
        (mem_treeWith _ _ _ _ hp).mpr ⟨j.val, hjl, rfl⟩
      have habove : (foldTree law ids parents matched).AboveHolds () ids.val[node.val] ↔
          matched.val.getD j.val false = true ∧
            (foldTree law ids parents matched).AboveHolds () ids.val[j.val] := by
        constructor
        · intro h
          rcases h.inv with ⟨e, he, hid, hr'⟩ | ⟨e, he, c, hc, hid, hov, hcond, hab⟩
          · obtain ⟨k, hk, rfl⟩ := (mem_treeWith _ _ _ _ hp).mp he
            have hkn := nodup_getElem_inj hnd hk hn hid
            subst hkn
            exact absurd hr' hpl
          · obtain ⟨k, hk, rfl⟩ := (mem_treeWith _ _ _ _ hp).mp he
            have hkn := nodup_getElem_inj hnd hk hn hid
            subst hkn
            obtain ⟨m, hm, rfl⟩ := (mem_treeWith _ _ _ _ hp).mp hc
            have hmj : m = j.val := nodup_getElem_inj hnd hm hjl (hov.symm.trans hpj)
            subst hmj
            exact ⟨hcond, hab⟩
        · rintro ⟨hc, hab⟩
          exact .up hmem hjmem hpj hc hab
      cases hm : matched.val[j.val]? with
      | none =>
        refine ⟨false, ?_, ?_⟩
        · unfold ancestors_hold_loop
          rw [loop.eq_1]
          simp [ancestors_hold_loop.body, hsle, hx, hpl, hl.eq, hr, hm]
        · rw [habove]
          simp [List.getD_eq_getElem?_getD, hm]
      | some v =>
        cases v with
        | false =>
          refine ⟨false, ?_, ?_⟩
          · unfold ancestors_hold_loop
            rw [loop.eq_1]
            simp [ancestors_hold_loop.body, hsle, hx, hpl, hl.eq, hr, hm]
          · rw [habove]
            simp [List.getD_eq_getElem?_getD, hm]
        | true =>
          obtain ⟨z, hz, hzv⟩ := checked_succ steps (by have := usize_max_ge; omega)
          have hjd : (foldTree law ids parents matched).ReachesIn ids.val[j.val] d := by
            rw [← hpj]
            exact hpd
          obtain ⟨b, hb, hiff⟩ := ih j z hjl hjd (by omega)
          refine ⟨b, ?_, ?_⟩
          · unfold ancestors_hold_loop
            rw [loop_cont (x' := (j, z))]
            · unfold ancestors_hold_loop at hb
              exact hb
            · simp [ancestors_hold_loop.body, hsle, hx, hpl, hl.eq, hr, hm, hz, lift]
          · rw [hiff, habove]
            simp [List.getD_eq_getElem?_getD, hm]

theorem ancestors_hold_spec (hl : Lawful inst) (law : T) (ids parents : Slice T)
    (matched : Slice Bool) (hp : parents.val.length = ids.val.length) (hnd : ids.val.Nodup)
    (hroot : law ∉ ids.val)
    (hdang : ∀ k < parents.val.length, ¬ Dangling law ids.val parents.val k)
    (hbound : ids.val.length ≤ 64) (k : Usize) (hk : k.val < ids.val.length)
    (hreach : ∃ d, (foldTree law ids parents matched).ReachesIn ids.val[k.val] d) :
    ∃ b, ancestors_hold inst law ids parents matched k = ok b ∧
      (b = true ↔ (foldTree law ids parents matched).AboveHolds () ids.val[k.val]) := by
  obtain ⟨d, hd⟩ := hreach
  have hids := treeWith_ids law ids.val parents.val (fun k (_ : Unit) => matched.val.getD k false) hp
  have hlen := hd.le_length (by rw [hids]; exact hnd) (by rw [hids, treeWith_root]; exact hroot)
  rw [treeWith_length, hp, Nat.min_self] at hlen
  obtain ⟨b, hb, hiff⟩ := ancestors_loop_spec hl law ids parents matched hp hnd hroot hdang
    hbound d k 0#usize hk hd (by simp; omega)
  exact ⟨b, by unfold ancestors_hold; exact hb, hiff⟩

/-- Exception `k` holds, is not lifted in `level`, and every exception above it holds. -/
def Marked (law : T) (ids parents : Slice T) (matched level : Slice Bool) (k : ℕ) : Prop :=
  ∃ hk : k < ids.val.length, matched.val[k]? = some true ∧ level.val[k]? = some false ∧
    (foldTree law ids parents matched).AboveHolds () ids.val[k]

theorem applied_exceptions_spec (hl : Lawful inst) (law : T) (ids parents : Slice T)
    (matched level applied : Slice Bool) (hp : parents.val.length = ids.val.length)
    (hnd : ids.val.Nodup) (hroot : law ∉ ids.val)
    (hdang : ∀ k < parents.val.length, ¬ Dangling law ids.val parents.val k)
    (hbound : ids.val.length ≤ 64)
    (hreach : ∀ k (hk : k < ids.val.length),
      ∃ d, (foldTree law ids parents matched).ReachesIn ids.val[k] d)
    (ha : applied.val.length = ids.val.length) :
    ∃ a', applied_exceptions inst law ids parents matched level applied = ok a' ∧
      a'.val.length = ids.val.length ∧
      ∀ k < ids.val.length, ∃ c, a'.val[k]? = some c ∧
        (c = true ↔ Marked law ids parents matched level k) := by
  classical
  let value : ℕ → Bool := fun k => decide (Marked law ids parents matched level k)
  have hstep : ∀ (i : Usize) (s : Slice Bool), i.val < ids.val.length →
      s.val.length = ids.val.length →
      ∃ z : Usize, ∃ s' : Slice Bool, z.val = i.val + 1 ∧
        s'.val = s.val.set i.val (value i.val) ∧
        applied_exceptions_loop.body inst law ids parents matched level s i = ok (cont (s', z)) := by
    intro i s hi hs
    have hlt : i < Slice.len s := by
      show i.val < (Slice.len s).val
      simp
      omega
    have hsx : s.val[i.val]? = some s.val[i.val] := List.getElem?_eq_getElem (by omega)
    obtain ⟨z, hz, hzv⟩ := checked_succ i (by have := slice_len_le ids; omega)
    obtain ⟨b, hb, hiff⟩ := ancestors_hold_spec hl law ids parents matched hp hnd hroot hdang
      hbound i hi (hreach i.val hi)
    by_cases hc : matched.val[i.val]? = some true ∧ level.val[i.val]? = some false
    · obtain ⟨hm, hlv⟩ := hc
      refine ⟨z, s.set_opt i (some b), hzv, ?_, ?_⟩
      · rw [Slice.set_opt_val_eq, set_opt_some]
        congr 1
        apply Bool.eq_iff_iff.mpr
        rw [hiff]
        simp only [value, decide_eq_true_eq, Marked, hm, hlv, true_and]
        exact ⟨fun h => ⟨hi, h⟩, fun ⟨_, h⟩ => h⟩
      · simp [applied_exceptions_loop.body, hlt, hm, hlv, hb, hsx, hz, lift]
    · refine ⟨z, s.set_opt i (some false), hzv, ?_, ?_⟩
      · rw [Slice.set_opt_val_eq, set_opt_some]
        congr 1
        symm
        simp only [value, decide_eq_false_iff_not, Marked]
        rintro ⟨-, h1, h2, -⟩
        exact hc ⟨h1, h2⟩
      · cases hm : matched.val[i.val]? with
        | none => simp [applied_exceptions_loop.body, hlt, hm, hsx, hz, lift]
        | some v =>
          cases hlv : level.val[i.val]? with
          | none => simp [applied_exceptions_loop.body, hlt, hm, hlv, hsx, hz, lift]
          | some w =>
            cases v <;> cases w
            · simp [applied_exceptions_loop.body, hlt, hm, hlv, hsx, hz, lift]
            · simp [applied_exceptions_loop.body, hlt, hm, hlv, hsx, hz, lift]
            · exact absurd ⟨hm, hlv⟩ hc
            · simp [applied_exceptions_loop.body, hlt, hm, hlv, hsx, hz, lift]
  have hend : ∀ (i : Usize) (s : Slice Bool), ids.val.length ≤ i.val →
      s.val.length = ids.val.length →
      applied_exceptions_loop.body inst law ids parents matched level s i = ok (done s) := by
    intro i s hi hs
    have hnot : ¬ i < Slice.len s := fun h => by
      have h' : i.val < (Slice.len s).val := h
      simp at h'
      omega
    simp [applied_exceptions_loop.body, hnot]
  obtain ⟨s', hs', hlen, hval⟩ := write_loop' _ _ value hstep hend _ 0#usize applied rfl ha
  refine ⟨s', by unfold applied_exceptions applied_exceptions_loop; exact hs', hlen,
    fun k hk => ⟨value k, by rw [hval k hk, if_neg (by simp)], ?_⟩⟩
  simp only [value, decide_eq_true_eq]

omit [DecidableEq T] in
theorem wf_no_dangling {σ : Type} (law : T) (ids parents : List T) (cond : ℕ → σ → Bool)
    (hp : parents.length = ids.length) (wf : (treeWith law ids parents cond).WellFormed) :
    ∀ k < parents.length, ¬ Dangling law ids parents k := by
  have hids := treeWith_ids law ids parents cond hp
  have hnd := wf.nodup
  have hroot := wf.rootFresh
  rw [hids] at hnd hroot
  rintro k hk ⟨p, hpk, hpl, hpn⟩
  have hki : k < ids.length := by omega
  obtain ⟨d, hd⟩ := wf.reaches _ ((mem_treeWith _ _ _ cond hp).mpr ⟨k, hki, rfl⟩)
  obtain ⟨d', -, hr'⟩ := (reach_step law ids parents cond hp hnd hroot hki d).mp hd
  rw [List.getElem?_eq_getElem hk] at hpk
  have hpe := Option.some.inj hpk
  rcases hr'.inv with ⟨hroot', -⟩ | ⟨e, he, -, hid, -, -⟩
  · exact hpl (by rw [← hpe]; exact hroot')
  · obtain ⟨c, hc, rfl⟩ := (mem_treeWith _ _ _ _ hp).mp he
    exact hpn c hc (by rw [List.getElem?_eq_getElem hc, ← hpe, hid])

/-- **The applied set.** On a tree the production `check_shape` admits, after
    the production `lifted`, `applied_exceptions` marks exception `k` exactly
    when the tree's stable labelling applies it and every exception above it
    holds (`Tree.Reported`). -/
theorem applied_sound (hl : Lawful inst) (law : T) (ids parents : Slice T)
    (declared matched level next applied : Slice Bool)
    (hp : parents.val.length = ids.val.length) (hd : declared.val.length = ids.val.length)
    (hlv : level.val.length = ids.val.length) (hnx : next.val.length = ids.val.length)
    (ha : applied.val.length = ids.val.length)
    (hadmit : check_shape inst law ids parents declared = ok (.Ok ()))
    {b : Bool} {l' n' : Slice Bool}
    (hrun : lifted inst law ids parents matched level next = ok (b, l', n'))
    {A : T → Prop} (hA : (foldTree law ids parents matched).Stable () A) :
    ∃ a', applied_exceptions inst law ids parents matched l' applied = ok a' ∧
      ∀ k (hk : k < ids.val.length), ∃ c, a'.val[k]? = some c ∧
        (c = true ↔ (foldTree law ids parents matched).Reported () A ids.val[k]) := by
  obtain ⟨r, hr, hiff⟩ := check_shape_correct hl law ids parents declared hp hd
    (fun k (_ : Unit) => matched.val.getD k false)
  rw [hadmit] at hr
  have wf := (hiff.mp (Result.ok.inj hr).symm).2
  have hids := treeWith_ids law ids.val parents.val (fun k (_ : Unit) => matched.val.getD k false) hp
  have hlen : (foldTree law ids parents matched).nodes.length = ids.val.length := by
    simp [foldTree, treeWith_length, hp]
  have hnd := wf.nodup
  have hroot := wf.rootFresh
  rw [hids] at hnd hroot
  have hbound : ids.val.length ≤ 64 := by
    have := wf.bounded
    rw [hlen] at this
    simpa [Tree.maxExceptions] using this
  obtain ⟨b', l'', n'', hrun', -, hl''⟩ :=
    lifted_correct hl law ids parents matched level next hp hlv hnx
  rw [hrun] at hrun'
  have heq := Result.ok.inj hrun'
  simp only [Prod.mk.injEq] at heq
  obtain ⟨-, rfl, -⟩ := heq
  have hreach : ∀ k (hk : k < ids.val.length),
      ∃ d, (foldTree law ids parents matched).ReachesIn ids.val[k] d :=
    fun k hk => wf.reaches _ ((mem_treeWith _ _ _ _ hp).mpr ⟨k, hk, rfl⟩)
  obtain ⟨a', ha', -, hval⟩ := applied_exceptions_spec hl law ids parents matched l' applied hp
    hnd hroot (wf_no_dangling _ _ _ _ hp wf) hbound hreach ha
  refine ⟨a', ha', fun k hk => ?_⟩
  obtain ⟨c, hc, hciff⟩ := hval k hk
  refine ⟨c, hc, hciff.trans ?_⟩
  have hmem : (⟨ids.val[k], parents.val[k], fun _ => matched.val.getD k false⟩ :
      Exception T Unit) ∈ (foldTree law ids parents matched).nodes :=
    (mem_treeWith _ _ _ _ hp).mpr ⟨k, hk, rfl⟩
  have hAk : A ids.val[k] ↔ matched.val.getD k false = true ∧
      (foldTree law ids parents matched).lifts () ids.val.length ids.val[k] = false := by
    have hu := Tree.stable_unique wf hA (Tree.stable_star wf ()) _ hmem
    rw [hu, Tree.star_iff wf () hmem, hlen]
  have hlk : l'.val[k]? =
      some ((foldTree law ids parents matched).lifts () ids.val.length ids.val[k]) := by
    rw [hl'', List.getElem?_map, List.getElem?_eq_getElem hk]
    rfl
  have hmk : matched.val[k]? = some true ↔ matched.val.getD k false = true := by
    rw [List.getD_eq_getElem?_getD]
    cases matched.val[k]? <;> simp
  unfold Marked Tree.Reported
  constructor
  · rintro ⟨_, hm, hlvk, hab⟩
    rw [hlk] at hlvk
    exact ⟨hAk.mpr ⟨hmk.mp hm, Option.some.inj hlvk⟩, hab⟩
  · rintro ⟨hA', hab⟩
    obtain ⟨hm, hlv⟩ := hAk.mp hA'
    exact ⟨hk, hmk.mpr hm, by rw [hlk, hlv], hab⟩

end Norn.Checker.LawExceptions
