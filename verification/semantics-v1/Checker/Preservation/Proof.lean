import Check.Funs
import NornSem.Exchange

/-!
# Production preservation check meets `NornSem.Exchange.check` (#718 U7b)

`Check` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/consistency/preservation/check.rs`. The translation of
its data into the written semantics is `atomsSem`, `depSem`, `ruleSem` and
`stepSem`; a production chase state denotes `stateSem`.

* `check_correct`: whenever the production `check` returns `true` (without
  panic or divergence), `Norn.Sem.Exchange.check` accepts the translated
  certificate.
* `check_sound`: hence the translated path mapping preserves the target EGD on
  every source instance over every domain (`Norn.Sem.Exchange.check_sound`).

Only acceptance is related: a refusal (including any overflow the production
code guards against) withholds `certified` and needs no proof.
-/

namespace Norn.Checker.Preservation
open Aeneas Aeneas.Std Result ControlFlow
open Norn.Sem
open norn_checker_preservation

/-! ## Library glue -/

@[simp] theorem slice_get {α : Type} (s : Slice α) (i : Usize) :
    core.slice.Slice.get (core.slice.index.SliceIndexUsizeSlice α) s i = ok s.val[i.val]? := rfl

@[simp] theorem slice_get_mut {α : Type} (s : Slice α) (i : Usize) :
    core.slice.Slice.get_mut (core.slice.index.SliceIndexUsizeSlice α) s i =
      ok (s.val[i.val]?, s.set_opt i) := rfl

@[simp] theorem deref_val {T : Type} (v : alloc.vec.Vec T) :
    (alloc.vec.Vec.deref v).val = v.val := rfl

theorem bind_inv {α β : Type} {x : Result α} {f : α → Result β} {y : β}
    (h : Bind.bind x f = ok y) : ∃ a, x = ok a ∧ f a = ok y := by
  cases x with
  | ok a => exact ⟨a, rfl, h⟩
  | fail e => simp at h
  | div => simp at h

theorem push_ok {α : Type} {v v' : alloc.vec.Vec α} {x : α}
    (h : alloc.vec.Vec.push v x = ok v') : v'.val = v.val ++ [x] := by
  unfold alloc.vec.Vec.push at h
  dsimp only at h
  split at h
  · cases h
    simp
  · cases h

theorem add_some {x y z : Usize} (h : Usize.checked_add x y = some z) :
    z.val = x.val + y.val := by
  have hs := Usize.checked_add_bv_spec x y
  rw [h] at hs
  exact hs.2.1

theorem add_none {x y : Usize} (h : Usize.checked_add x y = none) :
    Usize.max < x.val + y.val := by
  have hs := Usize.checked_add_bv_spec x y
  rw [h] at hs
  exact hs

theorem mul_some {x y z : Usize} (h : Usize.checked_mul x y = some z) :
    z.val = x.val * y.val := by
  have hs := Usize.checked_mul_bv_spec x y
  rw [h] at hs
  exact hs.2.1

theorem sub_some {x y z : Usize} (h : Usize.checked_sub x y = some z) :
    y.val ≤ x.val ∧ z.val = x.val - y.val := by
  have hs := Usize.checked_sub_bv_spec x y
  rw [h] at hs
  exact ⟨hs.1, hs.2.1⟩

/-- Stepping an index that is below some list's length never overflows. -/
theorem succ_some {i z : Usize} (h : Usize.checked_add i 1#usize = some z) :
    z.val = i.val + 1 := by
  simpa using add_some h

theorem succ_none {i : Usize} {n : ℕ} (hi : i.val < n) (hn : n ≤ Usize.max)
    (h : Usize.checked_add i 1#usize = none) : False := by
  have := add_none h
  simp at this
  omega

theorem slice_le {α : Type} (s : Slice α) : s.val.length ≤ Usize.max := s.property

theorem vec_le {α : Type} (s : alloc.vec.Vec α) : s.val.length ≤ Usize.max := s.property

theorem get_some {α : Type} {l : List α} {i : ℕ} {x : α} (h : l[i]? = some x) :
    ∃ hi : i < l.length, l[i] = x :=
  List.getElem?_eq_some_iff.mp h

theorem get_none {α : Type} {l : List α} {i : ℕ} (h : l[i]? = none) : l.length ≤ i :=
  List.getElem?_eq_none_iff.mp h

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

/-! ## Partial correctness of a loop

Only acceptance is related, so every loop is reasoned about partially: if it
returns `ok`, its result satisfies the postcondition. Each continuing step
decreases a measure. -/

theorem loop_partial {α β : Type} (body : α → Result (ControlFlow α β)) (μ : α → ℕ)
    (I : α → Prop) (Q : β → Prop)
    (hdone : ∀ x y, I x → body x = ok (done y) → Q y)
    (hcont : ∀ x x', I x → body x = ok (cont x') → I x' ∧ μ x' < μ x) :
    ∀ x, I x → ∀ y, loop body x = ok y → Q y := by
  suffices h : ∀ n x, μ x = n → I x → ∀ y, loop body x = ok y → Q y from
    fun x => h (μ x) x rfl
  intro n
  induction n using Nat.strong_induction_on with
  | _ n ih =>
    intro x hn hx y hl
    rw [loop.eq_1] at hl
    cases hb : body x with
    | ok r =>
      rw [hb] at hl
      cases r with
      | done y' =>
        have hy : y' = y := by simpa using hl
        subst hy
        exact hdone x y' hx hb
      | cont x' =>
        obtain ⟨hi, hm⟩ := hcont x x' hx hb
        exact ih (μ x') (hn ▸ hm) x' rfl hi y (by simpa using hl)
    | fail e =>
      rw [hb] at hl
      simp at hl
    | div =>
      rw [hb] at hl
      simp at hl

/-! ## The denotation of production data -/

/-- The naturals of a list of `usize`. -/
def vals (l : List Usize) : List ℕ := l.map (·.val)

/-- An atom. -/
def atomSem (a : Atom) : Exchange.Atom := ⟨a.rel.val, vals a.args.val⟩

/-- A list of atoms. -/
def atomsSem (l : List Atom) : List Exchange.Atom := l.map atomSem

/-- A dependency. -/
def depSem : Dep → Exchange.Dep
  | .Tgd body head => .tgd (atomsSem body.val) (atomsSem head.val)
  | .Egd body l r => .egd (atomsSem body.val) l.val r.val

/-- A mapping rule. -/
def ruleSem (m : Rule) : Exchange.Rule := ⟨atomsSem m.body.val, atomSem m.head⟩

/-- A chase step. -/
def stepSem (s : Step) : Exchange.Step := ⟨s.dep.val, vals s.asg.val⟩

/-- The class of `x` under a representative list: its entry, or itself beyond
    the list. -/
def repN (rep : List ℕ) (x : ℕ) : ℕ := rep[x]?.getD x

/-- A state atom. -/
def factSem (a : Atom) : ℕ × List ℕ := (a.rel.val, vals a.args.val)

/-- A production chase state. -/
def stateSem (s : State) : Exchange.State :=
  ⟨s.atoms.val.map factSem, repN (vals s.rep.val), s.rep.val.length⟩

/-- Every class is an element. -/
def Inv (rep : List Usize) : Prop := ∀ c ∈ rep, c.val < rep.length

@[simp] theorem vals_length (l : List Usize) : (vals l).length = l.length := by simp [vals]

@[simp] theorem vals_append (l m : List Usize) : vals (l ++ m) = vals l ++ vals m := by
  simp [vals]

@[simp] theorem vals_nil : vals [] = [] := rfl

@[simp] theorem vals_cons (x : Usize) (l : List Usize) : vals (x :: l) = x.val :: vals l := rfl

theorem vals_getElem? (l : List Usize) (i : ℕ) : (vals l)[i]? = l[i]?.map (·.val) := by
  simp [vals]

theorem vals_take (l : List Usize) (n : ℕ) : vals (l.take n) = (vals l).take n := by
  simp [vals, List.map_take]

/-! ## Elements and classes -/

theorem rep_of_ok {rep : Slice Usize} {x y : Usize} (h : rep_of rep x = ok y) :
    y.val = repN (vals rep.val) x.val := by
  simp only [rep_of, slice_get, bind_tc_ok] at h
  split at h
  · rename_i heq
    cases h
    simp [repN, vals_getElem?, heq]
  · rename_i e heq
    cases h
    simp [repN, vals_getElem?, heq]

theorem value_at_ok {asg : Slice Usize} {v y : Usize} (h : value_at asg v = ok y) :
    y.val = Exchange.at' (vals asg.val) v.val := by
  simp only [value_at, slice_get, bind_tc_ok] at h
  split at h
  · rename_i heq
    cases h
    simp [Exchange.at', List.getD_eq_getElem?_getD, vals_getElem?, heq]
  · rename_i e heq
    cases h
    simp [Exchange.at', List.getD_eq_getElem?_getD, vals_getElem?, heq]

/-- `same` accepts only equal class lists. -/
theorem same_ok {rep xs ys : Slice Usize} (h : same rep xs ys = ok true) :
    (vals xs.val).map (repN (vals rep.val)) = (vals ys.val).map (repN (vals rep.val)) := by
  unfold same at h
  dsimp only at h
  split at h
  · cases h
  · rename_i hne
    have hl : xs.val.length = ys.val.length := by
      simpa using hne
    let P : ℕ → Prop := fun n => ∀ k (hk : k < n) (hx : k < xs.val.length) (hy : k < ys.val.length),
      repN (vals rep.val) xs.val[k].val = repN (vals rep.val) ys.val[k].val
    have key := loop_partial (fun i => same_loop.body rep xs ys i)
      (fun i => xs.val.length - i.val) (fun i => P i.val) (fun b => b = true → P xs.val.length)
      ?_ ?_ 0#usize (fun k hk => absurd hk (by simp)) true h rfl
    · apply List.ext_getElem (by simp [hl])
      intro k h1 h2
      simp only [List.getElem_map, vals]
      exact key k (by simpa [vals] using h1) (by simpa [vals] using h1) (by simpa [vals] using h2)
    · intro i b hi hb hbt
      subst hbt
      simp only [same_loop.body, slice_get, bind_tc_ok, lift] at hb
      split at hb
      · rename_i hx
        have := get_none hx
        exact fun k hk => hi k (by omega)
      · split at hb
        · cases hb
        · obtain ⟨a, -, hb⟩ := bind_inv hb
          obtain ⟨c, -, hb⟩ := bind_inv hb
          split at hb
          · cases hb
          · split at hb <;> cases hb
    · intro i i' hi hb
      simp only [same_loop.body, slice_get, bind_tc_ok, lift] at hb
      split at hb
      · cases hb
      · rename_i x hx
        obtain ⟨hix, hxe⟩ := get_some hx
        split at hb
        · cases hb
        · rename_i y hy
          obtain ⟨hiy, hye⟩ := get_some hy
          obtain ⟨a, ha, hb⟩ := bind_inv hb
          obtain ⟨c, hc, hb⟩ := bind_inv hb
          split at hb
          · cases hb
          · rename_i hac
            have hac' : a = c := UScalar.eq_of_val_eq (by simpa using hac)
            split at hb
            · cases hb
            · rename_i z hz
              cases hb
              have hzv := succ_some hz
              refine ⟨fun k hk h1 h2 => ?_, by omega⟩
              rcases Nat.lt_or_ge k i.val with hki | hki
              · exact hi k hki h1 h2
              · have hke : k = i.val := by omega
                subst hke
                rw [hxe, hye, ← rep_of_ok ha, ← rep_of_ok hc, hac']

theorem has_ok {st : State} {rel : Usize} {es : Slice Usize} (h : has st rel es = ok true) :
    (stateSem st).has rel.val (vals es.val) = true := by
  unfold has has_loop at h
  refine loop_partial _ (fun i : Usize => st.atoms.val.length - i.val) (fun _ => True)
    (fun b => b = true → (stateSem st).has rel.val (vals es.val) = true) ?_ ?_ _ trivial _ h rfl
  · intro i b _ hb hbt
    subst hbt
    simp only [has_loop.body, slice_get, bind_tc_ok, lift, deref_val] at hb
    split at hb
    · cases hb
    · rename_i atom hat
      obtain ⟨hia, hae⟩ := get_some hat
      split at hb
      · rename_i hrel
        obtain ⟨b, hs, hb⟩ := bind_inv hb
        split at hb
        · rename_i hbt
          subst hbt
          have hsame := same_ok hs
          simp only [deref_val] at hsame
          simp only [Exchange.State.has, stateSem, List.any_eq_true]
          refine ⟨factSem atom, List.mem_map.mpr ⟨atom, hae ▸ List.getElem_mem hia, rfl⟩, ?_⟩
          simp [factSem, hrel, hsame]
        · split at hb <;> cases hb
      · split at hb <;> cases hb
  · intro i i' _ hb
    simp only [has_loop.body, slice_get, bind_tc_ok, lift, deref_val] at hb
    split at hb
    · cases hb
    · rename_i atom hat
      obtain ⟨hia, -⟩ := get_some hat
      split at hb
      · obtain ⟨b, -, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · split at hb
          · cases hb
          · rename_i z hz
            cases hb
            have := succ_some hz
            exact ⟨trivial, by omega⟩
      · split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have := succ_some hz
          exact ⟨trivial, by omega⟩

theorem mem_vals {l : List Usize} {v : Usize} : v.val ∈ vals l ↔ v ∈ l := by
  simp only [vals, List.mem_map]
  constructor
  · rintro ⟨w, hw, hwv⟩
    rwa [← UScalar.eq_of_val_eq hwv]
  · intro hv
    exact ⟨v, hv, rfl⟩

theorem mem_vars_sem {l : List Atom} {v : ℕ} :
    v ∈ Exchange.vars (atomsSem l) ↔ ∃ a ∈ l, v ∈ vals a.args.val := by
  simp [Exchange.vars, atomsSem, atomSem, List.mem_flatMap]

theorem contains_ok {args : Slice Usize} {v : Usize} {b : Bool} (h : contains args v = ok b) :
    (b = true ↔ v.val ∈ vals args.val) := by
  unfold contains contains_loop at h
  refine loop_partial _ (fun i : Usize => args.val.length - i.val)
    (fun i => ∀ k (hk : k < i.val) (hl : k < args.val.length), args.val[k] ≠ v)
    (fun b => b = true ↔ v.val ∈ vals args.val) ?_ ?_ _ (fun k hk => absurd hk (by simp)) _ h
  · intro i b hi hb
    simp only [contains_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      have hlen := get_none hn
      cases hb
      simp only [Bool.false_eq_true, false_iff, mem_vals]
      intro hmem
      obtain ⟨k, hk, hkv⟩ := List.getElem_of_mem hmem
      exact hi k (by omega) hk hkv
    · rename_i w hw
      obtain ⟨hiw, hwe⟩ := get_some hw
      split at hb
      · rename_i hwv
        cases hb
        simp only [true_iff, mem_vals]
        rw [← hwv, ← hwe]
        exact List.getElem_mem hiw
      · split at hb
        · rename_i hz
          exact (succ_none hiw (slice_le args) hz).elim
        · cases hb
  · intro i i' hi hb
    simp only [contains_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i w hw
      obtain ⟨hiw, hwe⟩ := get_some hw
      split at hb
      · cases hb
      · rename_i hwv
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨fun k hk hl => ?_, by omega⟩
          rcases Nat.lt_or_ge k i.val with hki | hki
          · exact hi k hki hl
          · have hke : k = i.val := by omega
            subst hke
            rw [hwe]
            exact hwv

theorem contains_var_ok {atoms : Slice Atom} {v : Usize} {b : Bool}
    (h : contains_var atoms v = ok b) :
    (b = true ↔ v.val ∈ Exchange.vars (atomsSem atoms.val)) := by
  unfold contains_var contains_var_loop at h
  refine loop_partial _ (fun i : Usize => atoms.val.length - i.val)
    (fun i => ∀ k (hk : k < i.val) (hl : k < atoms.val.length), v.val ∉ vals atoms.val[k].args.val)
    (fun b => b = true ↔ v.val ∈ Exchange.vars (atomsSem atoms.val)) ?_ ?_ _
    (fun k hk => absurd hk (by simp)) _ h
  · intro i b hi hb
    simp only [contains_var_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      have hlen := get_none hn
      cases hb
      simp only [Bool.false_eq_true, false_iff, mem_vars_sem]
      rintro ⟨a, ha, hva⟩
      obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem ha
      exact hi k (by omega) hk hva
    · rename_i atom hat
      obtain ⟨hia, hae⟩ := get_some hat
      obtain ⟨c, hc, hb⟩ := bind_inv hb
      have hc' := contains_ok hc
      simp only [deref_val] at hc'
      split at hb
      · rename_i hct
        cases hb
        simp only [true_iff, mem_vars_sem]
        exact ⟨atom, hae ▸ List.getElem_mem hia, hc'.mp hct⟩
      · split at hb
        · rename_i hz
          exact (succ_none hia (slice_le atoms) hz).elim
        · cases hb
  · intro i i' hi hb
    simp only [contains_var_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i atom hat
      obtain ⟨hia, hae⟩ := get_some hat
      obtain ⟨c, hc, hb⟩ := bind_inv hb
      have hc' := contains_ok hc
      simp only [deref_val] at hc'
      split at hb
      · cases hb
      · rename_i hct
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨fun k hk hl => ?_, by omega⟩
          rcases Nat.lt_or_ge k i.val with hki | hki
          · exact hi k hki hl
          · have hke : k = i.val := by omega
            subst hke
            rw [hae]
            intro hm
            exact hct (hc'.mpr hm)

/-! ## Widths -/

theorem foldr_max_append (l m : List ℕ) :
    (l ++ m).foldr max 0 = max (l.foldr max 0) (m.foldr max 0) := by
  induction l with
  | nil => simp
  | cons a l ih => simp [ih, Nat.max_assoc]

theorem take_succ_of {α : Type} {l : List α} {i : ℕ} {x : α} (h : l[i]? = some x) :
    l.take (i + 1) = l.take i ++ [x] := by
  rw [List.take_add_one, h]
  rfl

theorem vals_take_succ {l : List Usize} {i : ℕ} {x : Usize} (h : l[i]? = some x) :
    (vals l).take (i + 1) = (vals l).take i ++ [x.val] := by
  rw [← vals_take, ← vals_take, take_succ_of h, vals_append]
  rfl

theorem max_arg_ok {args : Slice Usize} {acc y : Usize} (h : max_arg args acc = ok y) :
    y.val = max acc.val ((vals args.val).foldr max 0) := by
  unfold max_arg max_arg_loop at h
  refine loop_partial _ (fun p : Usize × Usize => args.val.length - p.1.val)
    (fun p => p.2.val = max acc.val (((vals args.val).take p.1.val).foldr max 0))
    (fun y : Usize => y.val = max acc.val ((vals args.val).foldr max 0)) ?_ ?_ _ (by simp) _ h
  · rintro ⟨i, most⟩ y hi hb
    simp only [max_arg_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hi, List.take_of_length_le (by simpa using get_none hn)]
    · rename_i v hv
      obtain ⟨hiv, -⟩ := get_some hv
      obtain ⟨m, -, hb⟩ := bind_inv hb
      split at hb
      · rename_i hz
        exact (succ_none hiv (slice_le args) hz).elim
      · cases hb
  · rintro ⟨i, most⟩ x' hi hb
    simp only [max_arg_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i v hv
      obtain ⟨hiv, -⟩ := get_some hv
      obtain ⟨m, hm, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i z hz
        cases hb
        have hzv := succ_some hz
        refine ⟨?_, by simp only; omega⟩
        have hmv : m.val = max most.val v.val := by
          split at hm <;> cases hm <;> rename_i hc
          · simp at hc
            omega
          · simp at hc
            omega
        simp only at hi ⊢
        rw [hmv, hi, hzv, vals_take_succ hv, foldr_max_append]
        simp [Nat.max_assoc]

theorem vars_take_succ {l : List Atom} {i : ℕ} {a : Atom} (h : l[i]? = some a) :
    Exchange.vars (atomsSem (l.take (i + 1))) =
      Exchange.vars (atomsSem (l.take i)) ++ vals a.args.val := by
  rw [take_succ_of h]
  simp [Exchange.vars, atomsSem, atomSem]

theorem max_var_ok {atoms : Slice Atom} {acc y : Usize} (h : max_var atoms acc = ok y) :
    y.val = max acc.val ((Exchange.vars (atomsSem atoms.val)).foldr max 0) := by
  unfold max_var max_var_loop at h
  refine loop_partial _ (fun p : Usize × Usize => atoms.val.length - p.1.val)
    (fun p => p.2.val = max acc.val ((Exchange.vars (atomsSem (atoms.val.take p.1.val))).foldr max 0))
    (fun y : Usize => y.val = max acc.val ((Exchange.vars (atomsSem atoms.val)).foldr max 0))
    ?_ ?_ _ (by simp [Exchange.vars, atomsSem]) _ h
  · rintro ⟨i, most⟩ y hi hb
    simp only [max_var_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hi, List.take_of_length_le (get_none hn)]
    · rename_i a ha
      obtain ⟨hia, -⟩ := get_some ha
      obtain ⟨m, -, hb⟩ := bind_inv hb
      split at hb
      · rename_i hz
        exact (succ_none hia (slice_le atoms) hz).elim
      · cases hb
  · rintro ⟨i, most⟩ x' hi hb
    simp only [max_var_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i a ha
      obtain ⟨hia, -⟩ := get_some ha
      obtain ⟨m, hm, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i z hz
        cases hb
        have hzv := succ_some hz
        refine ⟨?_, by simp only; omega⟩
        have hmv := max_arg_ok hm
        simp only [deref_val] at hmv
        simp only at hi ⊢
        rw [hmv, hi, hzv, vars_take_succ ha, foldr_max_append, Nat.max_assoc]

theorem width_ok {atoms : Slice Atom} {w : Usize} (h : width atoms = ok (some w)) :
    w.val = Exchange.width (atomsSem atoms.val) := by
  unfold width at h
  obtain ⟨m, hm, h⟩ := bind_inv h
  have hmv := max_var_ok hm
  have hw : Usize.checked_add m 1#usize = some w := by simpa using h
  rw [Exchange.width, add_some hw, hmv]
  simp

/-! ## Matching a body -/

theorem args_below_ok {args asg : Slice Usize} {next : Usize}
    (h : args_below args asg next = ok true) :
    ∀ v ∈ vals args.val, Exchange.at' (vals asg.val) v < next.val := by
  unfold args_below args_below_loop at h
  refine loop_partial _ (fun i : Usize => args.val.length - i.val)
    (fun i => ∀ k (hk : k < i.val) (hl : k < args.val.length),
      Exchange.at' (vals asg.val) args.val[k].val < next.val)
    (fun b => b = true → ∀ v ∈ vals args.val, Exchange.at' (vals asg.val) v < next.val)
    ?_ ?_ _ (fun k hk => absurd hk (by simp)) _ h rfl
  · intro i b hi hb hbt
    subst hbt
    simp only [args_below_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      have hlen := get_none hn
      intro v hv
      simp only [vals, List.mem_map] at hv
      obtain ⟨w, hw, rfl⟩ := hv
      obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hw
      exact hi k (by omega) hk
    · obtain ⟨c, -, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · split at hb <;> cases hb
  · intro i i' hi hb
    simp only [args_below_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i v hv
      obtain ⟨hiv, hve⟩ := get_some hv
      obtain ⟨c, hc, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i hlt
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨fun k hk hl => ?_, by omega⟩
          rcases Nat.lt_or_ge k i.val with hki | hki
          · exact hi k hki hl
          · have hke : k = i.val := by omega
            subst hke
            rw [hve, ← value_at_ok hc]
            simp at hlt
            exact hlt

theorem vars_below_ok {atoms : Slice Atom} {asg : Slice Usize} {next : Usize}
    (h : vars_below atoms asg next = ok true) :
    ∀ v ∈ Exchange.vars (atomsSem atoms.val), Exchange.at' (vals asg.val) v < next.val := by
  unfold vars_below vars_below_loop at h
  refine loop_partial _ (fun i : Usize => atoms.val.length - i.val)
    (fun i => ∀ k (hk : k < i.val) (hl : k < atoms.val.length),
      ∀ v ∈ vals atoms.val[k].args.val, Exchange.at' (vals asg.val) v < next.val)
    (fun b => b = true → ∀ v ∈ Exchange.vars (atomsSem atoms.val), Exchange.at' (vals asg.val) v < next.val)
    ?_ ?_ _ (fun k hk => absurd hk (by simp)) _ h rfl
  · intro i b hi hb hbt
    subst hbt
    simp only [vars_below_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      have hlen := get_none hn
      intro v hv
      obtain ⟨a, ha, hva⟩ := mem_vars_sem.mp hv
      obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem ha
      exact hi k (by omega) hk v hva
    · obtain ⟨c, -, hb⟩ := bind_inv hb
      split at hb
      · split at hb <;> cases hb
      · cases hb
  · intro i i' hi hb
    simp only [vars_below_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i a ha
      obtain ⟨hia, hae⟩ := get_some ha
      obtain ⟨c, hc, hb⟩ := bind_inv hb
      split at hb
      · rename_i hct
        subst hct
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨fun k hk hl => ?_, by omega⟩
          rcases Nat.lt_or_ge k i.val with hki | hki
          · exact hi k hki hl
          · have hke : k = i.val := by omega
            subst hke
            rw [hae]
            have := args_below_ok hc
            simpa using this
      · cases hb

theorem elements_ok {args asg : Slice Usize} {out : alloc.vec.Vec Usize}
    (h : elements args asg = ok out) :
    vals out.val = (vals args.val).map (Exchange.at' (vals asg.val)) := by
  unfold elements elements_loop at h
  refine loop_partial _ (fun p : Usize × alloc.vec.Vec Usize => args.val.length - p.1.val)
    (fun p => vals p.2.val = ((vals args.val).take p.1.val).map (Exchange.at' (vals asg.val)))
    (fun out : alloc.vec.Vec Usize => vals out.val = (vals args.val).map (Exchange.at' (vals asg.val)))
    ?_ ?_ _ (by simp) _ h
  · rintro ⟨i, o⟩ y hi hb
    simp only [elements_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hi, List.take_of_length_le (by simpa using get_none hn)]
    · rename_i v hv
      obtain ⟨hiv, -⟩ := get_some hv
      obtain ⟨c, -, hb⟩ := bind_inv hb
      obtain ⟨o', -, hb⟩ := bind_inv hb
      split at hb
      · rename_i hz
        exact (succ_none hiv (slice_le args) hz).elim
      · cases hb
  · rintro ⟨i, o⟩ x' hi hb
    simp only [elements_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i v hv
      obtain ⟨hiv, -⟩ := get_some hv
      obtain ⟨c, hc, hb⟩ := bind_inv hb
      obtain ⟨o1, ho, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i z hz
        cases hb
        have hzv := succ_some hz
        refine ⟨?_, by simp only; omega⟩
        simp only at hi ⊢
        rw [push_ok ho, vals_append, hi, hzv, vals_take_succ hv, List.map_append]
        simp [value_at_ok hc]

theorem atoms_held_ok {st : State} {body : Slice Atom} {asg : Slice Usize}
    (h : atoms_held st body asg = ok true) :
    ∀ a ∈ atomsSem body.val,
      (stateSem st).has a.rel (a.args.map (Exchange.at' (vals asg.val))) = true := by
  unfold atoms_held atoms_held_loop at h
  refine loop_partial _ (fun i : Usize => body.val.length - i.val)
    (fun i => ∀ k (hk : k < i.val) (hl : k < body.val.length),
      (stateSem st).has (atomSem body.val[k]).rel
        ((atomSem body.val[k]).args.map (Exchange.at' (vals asg.val))) = true)
    (fun b => b = true → ∀ a ∈ atomsSem body.val,
      (stateSem st).has a.rel (a.args.map (Exchange.at' (vals asg.val))) = true)
    ?_ ?_ _ (fun k hk => absurd hk (by simp)) _ h rfl
  · intro i b hi hb hbt
    subst hbt
    simp only [atoms_held_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      have hlen := get_none hn
      intro a ha
      simp only [atomsSem, List.mem_map] at ha
      obtain ⟨x, hx, rfl⟩ := ha
      obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hx
      exact hi k (by omega) hk
    · obtain ⟨e, -, hb⟩ := bind_inv hb
      obtain ⟨c, -, hb⟩ := bind_inv hb
      split at hb
      · split at hb <;> cases hb
      · cases hb
  · intro i i' hi hb
    simp only [atoms_held_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i a ha
      obtain ⟨hia, hae⟩ := get_some ha
      obtain ⟨e, he, hb⟩ := bind_inv hb
      obtain ⟨c, hc, hb⟩ := bind_inv hb
      split at hb
      · rename_i hct
        subst hct
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨fun k hk hl => ?_, by omega⟩
          rcases Nat.lt_or_ge k i.val with hki | hki
          · exact hi k hki hl
          · have hke : k = i.val := by omega
            subst hke
            have hhas := has_ok hc
            have hel := elements_ok he
            simp only [deref_val] at hhas hel
            rw [hae]
            simp only [atomSem]
            rw [← hel]
            exact hhas
      · cases hb

theorem body_matches_ok {st : State} {body : Slice Atom} {asg : Slice Usize}
    (h : body_matches st body asg = ok true) :
    Exchange.bodyMatches (stateSem st) (atomsSem body.val) (vals asg.val) = true := by
  unfold body_matches at h
  obtain ⟨c, hc, h⟩ := bind_inv h
  split at h
  · rename_i hct
    subst hct
    have hb := vars_below_ok hc
    have ha := atoms_held_ok h
    simp only [Exchange.bodyMatches, Bool.and_eq_true, List.all_eq_true, decide_eq_true_eq]
    refine ⟨fun v hv => ?_, ha⟩
    have := hb v hv
    simpa [stateSem] using this
  · cases h

/-! ## Merging classes -/

theorem repN_lt {rep : List Usize} (hinv : Inv rep) {x : ℕ} (hx : x < rep.length) :
    repN (vals rep) x < rep.length := by
  simp only [repN, vals_getElem?, List.getElem?_eq_getElem hx, Option.map_some, Option.getD_some]
  exact hinv _ (List.getElem_mem hx)

theorem repN_ge {rep : List ℕ} {x : ℕ} (hx : rep.length ≤ x) : repN rep x = x := by
  simp [repN, List.getElem?_eq_none hx]

theorem merged_ok {rep : Slice Usize} {out : alloc.vec.Vec Usize} {a b : Usize}
    (h : merged rep a b = ok out)
    (ha : a.val < rep.val.length) (hb : b.val < rep.val.length) (hinv : Inv rep.val) :
    out.val.length = rep.val.length ∧ Inv out.val ∧
      repN (vals out.val) = fun x => if repN (vals rep.val) x = repN (vals rep.val) b.val
        then repN (vals rep.val) a.val else repN (vals rep.val) x := by
  unfold merged at h
  obtain ⟨into, hi, h⟩ := bind_inv h
  obtain ⟨frm, hf, h⟩ := bind_inv h
  have hiv := rep_of_ok hi
  have hfv := rep_of_ok hf
  let f : Usize → Usize := fun c => if c = frm then into else c
  have hloop : out.val = rep.val.map f := by
    unfold merged_loop at h
    refine loop_partial _ (fun p : Usize × alloc.vec.Vec Usize => rep.val.length - p.1.val)
      (fun p => p.2.val = (rep.val.take p.1.val).map f)
      (fun o : alloc.vec.Vec Usize => o.val = rep.val.map f) ?_ ?_ _ (by simp) _ h
    · rintro ⟨i, o⟩ y hI hb
      simp only [merged_loop.body, slice_get, bind_tc_ok, lift] at hb
      split at hb
      · rename_i hn
        cases hb
        rw [hI, List.take_of_length_le (get_none hn)]
      · rename_i e he
        obtain ⟨hie, -⟩ := get_some he
        obtain ⟨o1, -, hb⟩ := bind_inv hb
        split at hb
        · rename_i hz
          exact (succ_none hie (slice_le rep) hz).elim
        · cases hb
    · rintro ⟨i, o⟩ x' hI hb
      simp only [merged_loop.body, slice_get, bind_tc_ok, lift] at hb
      split at hb
      · cases hb
      · rename_i e he
        obtain ⟨hie, -⟩ := get_some he
        obtain ⟨o1, ho, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨?_, by simp only; omega⟩
          simp only at hI ⊢
          have ho1 : o1.val = o.val ++ [f e] := by
            split at ho <;> rename_i hc <;> rw [push_ok ho] <;> simp [f, hc]
          rw [ho1, hI, hzv, take_succ_of he, List.map_append]
          rfl
  have hir : into.val < rep.val.length := by
    rw [hiv]
    exact repN_lt hinv ha
  have hlen : out.val.length = rep.val.length := by
    rw [hloop]
    exact List.length_map _
  refine ⟨hlen, ?_, ?_⟩
  · intro c hc
    rw [hlen]
    rw [hloop] at hc
    obtain ⟨d, hd, hdc⟩ := List.mem_map.mp hc
    rw [← hdc]
    by_cases hdf : d = frm
    · have hfd : f d = into := if_pos hdf
      rw [hfd]
      exact hir
    · have hfd : f d = d := if_neg hdf
      rw [hfd]
      exact hinv d hd
  · funext x
    rw [hloop]
    rcases Nat.lt_or_ge x rep.val.length with hx | hx
    · have h1 : repN (vals (rep.val.map f)) x = (f rep.val[x]).val := by
        simp [repN, vals, hx]
      have h2 : repN (vals rep.val) x = rep.val[x].val := by
        simp [repN, vals, hx]
      rw [h1, h2, ← hiv, ← hfv]
      by_cases hc : rep.val[x] = frm
      · rw [if_pos (congrArg UScalar.val hc)]
        simp only [f, if_pos hc]
      · rw [if_neg (fun h' => hc (UScalar.eq_of_val_eq h'))]
        simp only [f, if_neg hc]
    · rw [repN_ge (rep := vals (rep.val.map f)) (x := x) (by simpa using hx),
        repN_ge (rep := vals rep.val) (x := x) (by simpa using hx)]
      rw [if_neg]
      have := repN_lt hinv hb
      omega

/-! ## Steps -/

theorem head_args_ok {body : Slice Atom} {asg : Slice Usize} {base : Usize} {args : Slice Usize}
    {out : alloc.vec.Vec Usize} (h : head_args body asg base args = ok (some out)) :
    vals out.val = (vals args.val).map (fun v => if v ∈ Exchange.vars (atomsSem body.val)
      then Exchange.at' (vals asg.val) v else base.val + v) := by
  let g : ℕ → ℕ := fun v => if v ∈ Exchange.vars (atomsSem body.val)
    then Exchange.at' (vals asg.val) v else base.val + v
  unfold head_args head_args_loop at h
  refine loop_partial _ (fun p : Usize × alloc.vec.Vec Usize => args.val.length - p.1.val)
    (fun p => vals p.2.val = ((vals args.val).take p.1.val).map g)
    (fun r => ∀ out : alloc.vec.Vec Usize, r = some out → vals out.val = (vals args.val).map g)
    ?_ ?_ _ (by simp) _ h out rfl
  · rintro ⟨i, o⟩ y hi hb out' hy
    subst hy
    simp only [head_args_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hi, List.take_of_length_le (by simpa using get_none hn)]
    · rename_i v hv
      obtain ⟨hiv, -⟩ := get_some hv
      obtain ⟨c, -, hb⟩ := bind_inv hb
      split at hb
      · obtain ⟨e, -, hb⟩ := bind_inv hb
        obtain ⟨o', -, hb⟩ := bind_inv hb
        split at hb <;> cases hb
      · split at hb
        · cases hb
        · obtain ⟨o', -, hb⟩ := bind_inv hb
          split at hb <;> cases hb
  · rintro ⟨i, o⟩ x' hi hb
    simp only [head_args_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i v hv
      obtain ⟨hiv, -⟩ := get_some hv
      obtain ⟨c, hc, hb⟩ := bind_inv hb
      have hc' := contains_var_ok hc
      split at hb
      · rename_i hct
        obtain ⟨e, he, hb⟩ := bind_inv hb
        obtain ⟨o1, ho, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨?_, by simp only; omega⟩
          simp only at hi ⊢
          rw [push_ok ho, vals_append, hi, hzv, vals_take_succ hv, List.map_append]
          simp [g, value_at_ok he, hc'.mp hct]
      · rename_i hct
        split at hb
        · cases hb
        · rename_i fresh hfr
          obtain ⟨o1, ho, hb⟩ := bind_inv hb
          split at hb
          · cases hb
          · rename_i z hz
            cases hb
            have hzv := succ_some hz
            refine ⟨?_, by simp only; omega⟩
            simp only at hi ⊢
            rw [push_ok ho, vals_append, hi, hzv, vals_take_succ hv, List.map_append]
            have hn : v.val ∉ Exchange.vars (atomsSem body.val) := fun hm => hct (hc'.mpr hm)
            simp [g, add_some hfr, hn]

theorem with_heads_ok {atoms out : alloc.vec.Vec Atom} {body head : Slice Atom}
    {asg : Slice Usize} {base : Usize}
    (h : with_heads atoms body head asg base = ok (some out)) :
    out.val.map factSem = atoms.val.map factSem ++ (atomsSem head.val).map (fun a =>
      (a.rel, a.args.map (fun v => if v ∈ Exchange.vars (atomsSem body.val)
        then Exchange.at' (vals asg.val) v else base.val + v))) := by
  let g : ℕ → ℕ := fun v => if v ∈ Exchange.vars (atomsSem body.val)
    then Exchange.at' (vals asg.val) v else base.val + v
  let F : Exchange.Atom → ℕ × List ℕ := fun a => (a.rel, a.args.map g)
  unfold with_heads with_heads_loop at h
  refine loop_partial _ (fun p : Usize × alloc.vec.Vec Atom => head.val.length - p.1.val)
    (fun p => p.2.val.map factSem = atoms.val.map factSem ++ (atomsSem (head.val.take p.1.val)).map F)
    (fun r => ∀ o : alloc.vec.Vec Atom, r = some o →
      o.val.map factSem = atoms.val.map factSem ++ (atomsSem head.val).map F)
    ?_ ?_ _ (by simp [atomsSem]) _ h out rfl
  · rintro ⟨i, o⟩ y hi hb o' hy
    subst hy
    simp only [with_heads_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hi, List.take_of_length_le (get_none hn)]
    · rename_i a ha
      obtain ⟨hia, -⟩ := get_some ha
      obtain ⟨r, -, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · obtain ⟨o1, -, hb⟩ := bind_inv hb
        split at hb <;> cases hb
  · rintro ⟨i, o⟩ x' hi hb
    simp only [with_heads_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i a ha
      obtain ⟨hia, -⟩ := get_some ha
      obtain ⟨r, hr, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i args
        obtain ⟨o1, ho, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨?_, by simp only; omega⟩
          simp only at hi ⊢
          have hargs := head_args_ok hr
          simp only [deref_val] at hargs
          rw [push_ok ho, List.map_append, hi, hzv, take_succ_of ha]
          simp only [atomsSem, List.map_append, List.map_cons, List.map_nil, List.append_assoc]
          congr 2
          simp only [factSem, F, atomSem, hargs]
          rfl

theorem range'_snoc (s n : ℕ) : List.range' s n ++ [s + n] = List.range' s (n + 1) := by
  rw [List.range'_concat, Nat.one_mul]

theorem extended_ok {rep out : alloc.vec.Vec Usize} {size : Usize}
    (h : extended rep size = ok out) :
    ∃ n : List Usize, out.val = rep.val ++ n ∧
      vals n = List.range' rep.val.length n.length ∧
      out.val.length = max rep.val.length size.val := by
  unfold extended extended_loop at h
  refine loop_partial _ (fun r : alloc.vec.Vec Usize => size.val - r.val.length)
    (fun r => ∃ n : List Usize, r.val = rep.val ++ n ∧ vals n = List.range' rep.val.length n.length ∧
      r.val.length ≤ max rep.val.length size.val)
    (fun r : alloc.vec.Vec Usize => ∃ n : List Usize, r.val = rep.val ++ n ∧
      vals n = List.range' rep.val.length n.length ∧
      r.val.length = max rep.val.length size.val)
    ?_ ?_ _ ⟨[], by simp, by simp, by simp⟩ _ h
  · intro r y hI hb
    simp only [extended_loop.body] at hb
    split at hb
    · obtain ⟨o, -, hb⟩ := bind_inv hb
      cases hb
    · rename_i hge
      cases hb
      obtain ⟨n, hn, hv, hl⟩ := hI
      refine ⟨n, hn, hv, ?_⟩
      have h1 : size.val ≤ r.val.length := by simpa using hge
      have h2 : rep.val.length ≤ r.val.length := by rw [hn]; simp
      omega
  · intro r r' hI hb
    simp only [extended_loop.body] at hb
    split at hb
    · rename_i hlt
      have hlt' : r.val.length < size.val := by simpa using hlt
      obtain ⟨o, ho, hb⟩ := bind_inv hb
      cases hb
      obtain ⟨n, hn, hv, hl⟩ := hI
      have hpush := push_ok ho
      have hrl : r.val.length = rep.val.length + n.length := by rw [hn]; simp
      have hlv : (alloc.vec.Vec.len r).val = rep.val.length + n.length := by
        rw [alloc.vec.Vec.len_val]
        exact hrl
      refine ⟨⟨n ++ [alloc.vec.Vec.len r], by rw [hpush, hn, List.append_assoc], ?_, ?_⟩, ?_⟩
      · rw [vals_append, hv]
        simp only [vals_cons, vals_nil, hlv, List.length_append, List.length_singleton]
        exact range'_snoc _ _
      · rw [hpush]
        simp
        omega
      · rw [hpush]
        simp
        omega
    · cases hb

theorem repN_extend (l : List Usize) (n : List Usize)
    (hv : vals n = List.range' l.length n.length) : repN (vals (l ++ n)) = repN (vals l) := by
  funext x
  rw [vals_append]
  rcases Nat.lt_or_ge x l.length with hx | hx
  · simp only [repN]
    rw [List.getElem?_append_left (by simpa using hx)]
  · rw [repN_ge (rep := vals l) (x := x) (by simpa using hx)]
    simp only [repN]
    rw [List.getElem?_append_right (by simpa using hx), hv]
    by_cases hlt : x - (vals l).length < n.length
    · rw [List.getElem?_eq_getElem (by simpa using hlt), List.getElem_range']
      simp only [Option.getD_some, vals_length, Nat.one_mul]
      omega
    · rw [List.getElem?_eq_none (by simp only [List.length_range', vals_length] at hlt ⊢; omega)]
      rfl

theorem inv_extend {l n : List Usize} (hinv : Inv l)
    (hv : vals n = List.range' l.length n.length) : Inv (l ++ n) := by
  intro c hc
  rw [List.mem_append] at hc
  simp only [List.length_append]
  rcases hc with hc | hc
  · have := hinv c hc
    omega
  · have hm : c.val ∈ vals n := List.mem_map.mpr ⟨c, hc, rfl⟩
    rw [hv, List.mem_range'] at hm
    omega

theorem state_eq {s t : Exchange.State} (ha : s.atoms = t.atoms) (hr : s.rep = t.rep)
    (hn : s.next = t.next) : s = t := by
  cases s
  cases t
  simp_all

theorem apply_ok {theory : Slice Dep} {st st' : State} {step : Step}
    (h : apply theory st step = ok (some st')) (hinv : Inv st.rep.val) :
    Inv st'.rep.val ∧
      Exchange.apply (theory.val.map depSem) (stateSem st) (stepSem step) = some (stateSem st') := by
  unfold apply at h
  simp only [slice_get, bind_tc_ok, lift] at h
  split at h
  · cases h
  · rename_i d hd
    have hT : (theory.val.map depSem)[(stepSem step).dep]? = some (depSem d) := by
      simp [stepSem, List.getElem?_map, hd]
    split at h
    · rename_i body head
      obtain ⟨bm, hbm, h⟩ := bind_inv h
      split at h
      · rename_i hbt
        subst hbt
        have hm := body_matches_ok hbm
        obtain ⟨wo, hwo, h⟩ := bind_inv h
        split at h
        · cases h
        · rename_i w
          have hw := width_ok hwo
          split at h
          · cases h
          · rename_i size hsize
            obtain ⟨o3, ho3, h⟩ := bind_inv h
            split at h
            · cases h
            · rename_i atoms
              obtain ⟨v, hv, h⟩ := bind_inv h
              cases h
              have hheads := with_heads_ok ho3
              obtain ⟨n, hn, hvn, hlen⟩ := extended_ok hv
              have hsz := add_some hsize
              simp only [deref_val] at hm hw hheads
              refine ⟨by simp only; rw [hn]; exact inv_extend hinv hvn, ?_⟩
              rw [Exchange.apply, hT]
              simp only [depSem]
              rw [if_pos (by simpa [stepSem] using hm)]
              congr 1
              apply state_eq
              · simp only [stateSem]
                rw [hheads]
                simp [Exchange.headElem, stepSem]
              · simp only [stateSem]
                rw [hn, repN_extend _ _ hvn]
              · simp only [stateSem]
                rw [hlen, hsz, hw]
                simp
      · cases h
    · rename_i body l r
      obtain ⟨a, ha, h⟩ := bind_inv h
      obtain ⟨b, hb, h⟩ := bind_inv h
      obtain ⟨bm, hbm, h⟩ := bind_inv h
      split at h
      · rename_i hbt
        subst hbt
        have hm := body_matches_ok hbm
        split at h
        · rename_i hal
          split at h
          · rename_i hbl
            obtain ⟨rep5, hr5, h⟩ := bind_inv h
            cases h
            have hal' : a.val < st.rep.val.length := by simpa using hal
            have hbl' : b.val < st.rep.val.length := by simpa using hbl
            obtain ⟨hl5, hinv5, hrep5⟩ := merged_ok hr5 hal' hbl' hinv
            have hav := value_at_ok ha
            have hbv := value_at_ok hb
            simp only [deref_val] at hav hbv hm hl5 hinv5 hrep5
            refine ⟨hinv5, ?_⟩
            rw [Exchange.apply, hT]
            simp only [depSem]
            have hcond : (Exchange.bodyMatches (stateSem st) (atomsSem body.val) (stepSem step).asg &&
                decide (Exchange.at' (stepSem step).asg l.val < (stateSem st).next) &&
                decide (Exchange.at' (stepSem step).asg r.val < (stateSem st).next)) = true := by
              simp only [Bool.and_eq_true, decide_eq_true_eq, stepSem, stateSem]
              refine ⟨⟨hm, ?_⟩, ?_⟩
              · rw [← hav]
                exact hal'
              · rw [← hbv]
                exact hbl'
            rw [if_pos hcond]
            congr 1
            apply state_eq
            · rfl
            · simp only [Exchange.State.merge, stateSem, stepSem]
              rw [hrep5, ← hav, ← hbv]
            · simp only [Exchange.State.merge, stateSem]
              exact hl5.symm
          · cases h
        · cases h
      · cases h

theorem run_append (T : List Exchange.Dep) :
    ∀ (s : Exchange.State) (l m : List Exchange.Step),
      Exchange.run T s (l ++ m) = (Exchange.run T s l).bind fun s' => Exchange.run T s' m
  | s, [], m => by simp [Exchange.run]
  | s, x :: l, m => by
    simp only [List.cons_append, Exchange.run]
    cases Exchange.apply T s x with
    | none => rfl
    | some s' => exact run_append T s' l m

theorem run_ok {theory : Slice Dep} {st st' : State} {trace : Slice Step}
    (h : run theory st trace = ok (some st')) (hinv : Inv st.rep.val) :
    Exchange.run (theory.val.map depSem) (stateSem st) (trace.val.map stepSem) =
      some (stateSem st') := by
  unfold run run_loop at h
  refine loop_partial _ (fun p : Usize × State => trace.val.length - p.1.val)
    (fun p => Inv p.2.rep.val ∧ Exchange.run (theory.val.map depSem) (stateSem st)
      ((trace.val.take p.1.val).map stepSem) = some (stateSem p.2))
    (fun r => ∀ s', r = some s' → Exchange.run (theory.val.map depSem) (stateSem st)
      (trace.val.map stepSem) = some (stateSem s'))
    ?_ ?_ _ ⟨hinv, by simp [Exchange.run]⟩ _ h st' rfl
  · rintro ⟨i, s⟩ y hI hb s' hy
    subst hy
    simp only [run_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [← hI.2, List.take_of_length_le (get_none hn)]
    · obtain ⟨o, -, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · split at hb
        · cases hb
        · cases hb
  · rintro ⟨i, s⟩ x' hI hb
    simp only [run_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i x hx
      obtain ⟨hix, -⟩ := get_some hx
      obtain ⟨o, ho, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i nxt
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          obtain ⟨hinv', happ⟩ := apply_ok ho hI.1
          refine ⟨⟨hinv', ?_⟩, by simp only; omega⟩
          simp only
          rw [hzv, take_succ_of hx, List.map_append, run_append, hI.2]
          simp [Exchange.run, happ]

/-! ## The canonical state of a case -/

theorem width_of_ok {rule : Rule} {w : Usize} (h : width_of rule = ok (some w)) :
    w.val = Exchange.width ((ruleSem rule).head :: (ruleSem rule).body) := by
  unfold width_of at h
  obtain ⟨m, hm, h⟩ := bind_inv h
  obtain ⟨m', hm', h⟩ := bind_inv h
  have hw : Usize.checked_add m' 1#usize = some w := by simpa using h
  have h1 := max_arg_ok hm
  have h2 := max_var_ok hm'
  simp only [deref_val] at h1 h2
  rw [add_some hw, h2, h1]
  simp only [Exchange.width, Exchange.vars, ruleSem, atomSem, List.flatMap_cons]
  rw [foldr_max_append]
  simp [atomsSem]

theorem ruleWidth_snoc (l : List Exchange.Rule) (m : Exchange.Rule) :
    Exchange.ruleWidth (l ++ [m]) = max (Exchange.ruleWidth l) (Exchange.width (m.head :: m.body)) := by
  induction l with
  | nil => simp [Exchange.ruleWidth, Nat.max_comm]
  | cons a l ih =>
    simp only [List.cons_append, Exchange.ruleWidth, List.foldr_cons] at ih ⊢
    rw [ih, Nat.max_assoc]

theorem rule_width_ok {rules : Slice Rule} {w : Usize} (h : rule_width rules = ok (some w)) :
    w.val = Exchange.ruleWidth (rules.val.map ruleSem) := by
  unfold rule_width rule_width_loop at h
  refine loop_partial _ (fun p : Usize × Usize => rules.val.length - p.1.val)
    (fun p => p.2.val = Exchange.ruleWidth ((rules.val.take p.1.val).map ruleSem))
    (fun r => ∀ w : Usize, r = some w → w.val = Exchange.ruleWidth (rules.val.map ruleSem))
    ?_ ?_ _ (by simp [Exchange.ruleWidth]) _ h w rfl
  · rintro ⟨i, wd⟩ y hi hb w' hy
    subst hy
    simp only [rule_width_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hi, List.take_of_length_le (get_none hn)]
    · obtain ⟨o, -, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · obtain ⟨m, -, hb⟩ := bind_inv hb
        split at hb <;> cases hb
  · rintro ⟨i, wd⟩ x' hi hb
    simp only [rule_width_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i rule hr
      obtain ⟨hir, -⟩ := get_some hr
      obtain ⟨o, ho, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i wr
        obtain ⟨m, hm, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨?_, by simp only; omega⟩
          have hmv : m.val = max wd.val wr.val := by
            split at hm
            · rename_i hc
              cases hm
              have : wd.val < wr.val := hc
              omega
            · rename_i hc
              cases hm
              have : ¬ wd.val < wr.val := hc
              omega
          have hwr := width_of_ok ho
          simp only at hi ⊢
          rw [hmv, hi, hzv, take_succ_of hr, List.map_append, List.map_singleton, ruleWidth_snoc, hwr]

theorem case_elem_ok {nb w i v e : Usize} (h : case_elem nb w i v = ok (some e)) :
    e.val = Exchange.caseElem nb.val w.val i.val v.val := by
  unfold case_elem at h
  simp only [lift, bind_tc_ok] at h
  split at h
  · cases h
  · rename_i off hoff
    split at h
    · cases h
    · rename_i st hst
      have he : Usize.checked_add st v = some e := by simpa using h
      rw [add_some he, add_some hst, mul_some hoff, Exchange.caseElem]

theorem case_args_ok {args : Slice Usize} {nb w i : Usize} {out : alloc.vec.Vec Usize}
    (h : case_args args nb w i = ok (some out)) :
    vals out.val = (vals args.val).map (Exchange.caseElem nb.val w.val i.val) := by
  unfold case_args case_args_loop at h
  refine loop_partial _ (fun p : Usize × alloc.vec.Vec Usize => args.val.length - p.1.val)
    (fun p => vals p.2.val = ((vals args.val).take p.1.val).map (Exchange.caseElem nb.val w.val i.val))
    (fun r => ∀ o : alloc.vec.Vec Usize, r = some o →
      vals o.val = (vals args.val).map (Exchange.caseElem nb.val w.val i.val))
    ?_ ?_ _ (by simp) _ h out rfl
  · rintro ⟨j, o⟩ y hj hb o' hy
    subst hy
    simp only [case_args_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hj, List.take_of_length_le (by simpa using get_none hn)]
    · obtain ⟨c, -, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · obtain ⟨o1, -, hb⟩ := bind_inv hb
        split at hb <;> cases hb
  · rintro ⟨j, o⟩ x' hj hb
    simp only [case_args_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i v hv
      obtain ⟨c, hc, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i e
        obtain ⟨o1, ho, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          obtain ⟨hjv, -⟩ := get_some hv
          refine ⟨?_, by simp only; omega⟩
          simp only at hj ⊢
          rw [push_ok ho, vals_append, hj, hzv, vals_take_succ hv, List.map_append]
          simp [case_elem_ok hc]

theorem with_case_body_ok {atoms out : alloc.vec.Vec Atom} {body : Slice Atom} {nb w i : Usize}
    (h : with_case_body atoms body nb w i = ok (some out)) :
    out.val.map factSem = atoms.val.map factSem ++ (atomsSem body.val).map (fun a =>
      (a.rel, a.args.map (Exchange.caseElem nb.val w.val i.val))) := by
  let F : Exchange.Atom → ℕ × List ℕ := fun a => (a.rel, a.args.map (Exchange.caseElem nb.val w.val i.val))
  unfold with_case_body with_case_body_loop at h
  refine loop_partial _ (fun p : Usize × alloc.vec.Vec Atom => body.val.length - p.1.val)
    (fun p => p.2.val.map factSem = atoms.val.map factSem ++ (atomsSem (body.val.take p.1.val)).map F)
    (fun r => ∀ o : alloc.vec.Vec Atom, r = some o →
      o.val.map factSem = atoms.val.map factSem ++ (atomsSem body.val).map F)
    ?_ ?_ _ (by simp [atomsSem]) _ h out rfl
  · rintro ⟨j, o⟩ y hj hb o' hy
    subst hy
    simp only [with_case_body_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hj, List.take_of_length_le (get_none hn)]
    · obtain ⟨r, -, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · obtain ⟨o1, -, hb⟩ := bind_inv hb
        split at hb <;> cases hb
  · rintro ⟨j, o⟩ x' hj hb
    simp only [with_case_body_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i a ha
      obtain ⟨r, hr, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i args
        obtain ⟨o1, ho, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · rename_i z hz
          cases hb
          have hzv := succ_some hz
          refine ⟨?_, by simp only; obtain ⟨hja, -⟩ := get_some ha; omega⟩
          simp only at hj ⊢
          have hargs := case_args_ok hr
          simp only [deref_val] at hargs
          rw [push_ok ho, List.map_append, hj, hzv, take_succ_of ha]
          simp only [atomsSem, List.map_append, List.map_cons, List.map_nil, List.append_assoc]
          congr 2
          simp only [factSem, F, atomSem, hargs]

theorem chosen_eq {rules : Slice Rule} {choice : Slice Usize} {i : ℕ} {index : Usize} {rule : Rule}
    (hc : choice.val[i]? = some index) (hr : rules.val[index.val]? = some rule) :
    Exchange.chosen (rules.val.map ruleSem) (vals choice.val) i = ruleSem rule := by
  simp [Exchange.chosen, List.getD_eq_getElem?_getD, vals_getElem?, hc, List.getElem?_map, hr]

theorem atomAt_eq {body : Slice Atom} {i : ℕ} {target : Atom} (hb : body.val[i]? = some target) :
    Exchange.atomAt (atomsSem body.val) i = atomSem target := by
  simp [Exchange.atomAt, List.getD_eq_getElem?_getD, atomsSem, List.getElem?_map, hb]

theorem range_succ_flatMap {β : Type} (n : ℕ) (f : ℕ → List β) :
    (List.range (n + 1)).flatMap f = (List.range n).flatMap f ++ f n := by
  rw [List.range_succ, List.flatMap_append]
  simp

theorem case_bodies_ok {rules : Slice Rule} {choice : Slice Usize} {nb w : Usize}
    {out : alloc.vec.Vec Atom} (h : case_bodies rules choice nb w = ok (some out)) :
    out.val.map factSem = (List.range choice.val.length).flatMap fun i =>
      (Exchange.chosen (rules.val.map ruleSem) (vals choice.val) i).body.map fun a =>
        (a.rel, a.args.map (Exchange.caseElem nb.val w.val i)) := by
  let G : ℕ → List (ℕ × List ℕ) := fun i =>
    (Exchange.chosen (rules.val.map ruleSem) (vals choice.val) i).body.map fun a =>
      (a.rel, a.args.map (Exchange.caseElem nb.val w.val i))
  unfold case_bodies case_bodies_loop at h
  refine loop_partial _ (fun p : Usize × alloc.vec.Vec Atom => choice.val.length - p.1.val)
    (fun p => p.1.val ≤ choice.val.length ∧ p.2.val.map factSem = (List.range p.1.val).flatMap G)
    (fun r => ∀ o : alloc.vec.Vec Atom, r = some o →
      o.val.map factSem = (List.range choice.val.length).flatMap G)
    ?_ ?_ _ (by simp) _ h out rfl
  · rintro ⟨i, o⟩ y hi hb o' hy
    subst hy
    simp only [case_bodies_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      have := get_none hn
      have hie : i.val = choice.val.length := by
        have h1 := hi.1
        simp only at h1
        omega
      rw [hi.2, hie]
    · split at hb
      · cases hb
      · obtain ⟨r, -, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · split at hb <;> cases hb
  · rintro ⟨i, o⟩ x' hi hb
    simp only [case_bodies_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i index hc
      obtain ⟨hic, -⟩ := get_some hc
      split at hb
      · cases hb
      · rename_i rule hr
        obtain ⟨r, hrr, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · rename_i o1
          split at hb
          · cases hb
          · rename_i z hz
            cases hb
            have hzv := succ_some hz
            refine ⟨⟨by simp only; omega, ?_⟩, by simp only; omega⟩
            simp only
            have hbody := with_case_body_ok hrr
            simp only [deref_val] at hbody
            rw [hbody, hi.2, hzv, range_succ_flatMap]
            congr 1
            simp only [G, chosen_eq hc hr, ruleSem]

/-! Merging every head into its target, as one function on classes. -/

/-- One merge on a class function. -/
def mergeRep (f : ℕ → ℕ) (a b : ℕ) : ℕ → ℕ := fun x => if f x = f b then f a else f x

/-- The merges of `pairs`, in order. -/
def mergeAll (f : ℕ → ℕ) : List (ℕ × ℕ) → ℕ → ℕ
  | [] => f
  | p :: ps => mergeAll (mergeRep f p.1 p.2) ps

theorem mergeAll_append (f : ℕ → ℕ) (P Q : List (ℕ × ℕ)) :
    mergeAll f (P ++ Q) = mergeAll (mergeAll f P) Q := by
  induction P generalizing f with
  | nil => rfl
  | cons p P ih => exact ih _

theorem fold_merge (P : List (ℕ × ℕ)) :
    ∀ s : Exchange.State, P.foldl (fun s p => s.merge p.1 p.2) s = ⟨s.atoms, mergeAll s.rep P, s.next⟩ := by
  induction P with
  | nil => intro s; rfl
  | cons p P ih =>
    intro s
    rw [List.foldl_cons, ih]
    rfl

theorem take_zip_succ {α β : Type} {l : List α} {m : List β} {j : ℕ} {x : α} {y : β}
    (hx : l[j]? = some x) (hy : m[j]? = some y) :
    (l.zip m).take (j + 1) = (l.zip m).take j ++ [(x, y)] := by
  have : (l.zip m)[j]? = some (x, y) := by
    rw [List.getElem?_zip_eq_some]
    exact ⟨hx, hy⟩
  exact take_succ_of this

theorem merged_pairs_ok {rep out : alloc.vec.Vec Usize} {head target : Slice Usize}
    {nb w i : Usize} (h : merged_pairs rep head target nb w i = ok (some out)) (hinv : Inv rep.val) :
    Inv out.val ∧ out.val.length = rep.val.length ∧
      repN (vals out.val) = mergeAll (repN (vals rep.val))
        (((vals head.val).zip (vals target.val)).map fun p =>
          (Exchange.caseElem nb.val w.val i.val p.1, p.2)) := by
  let P := ((vals head.val).zip (vals target.val)).map fun p =>
    (Exchange.caseElem nb.val w.val i.val p.1, p.2)
  unfold merged_pairs merged_pairs_loop at h
  refine loop_partial _ (fun p : alloc.vec.Vec Usize × Usize => head.val.length - p.2.val)
    (fun p => Inv p.1.val ∧ p.1.val.length = rep.val.length ∧
      repN (vals p.1.val) = mergeAll (repN (vals rep.val)) (P.take p.2.val))
    (fun r => ∀ o : alloc.vec.Vec Usize, r = some o →
      Inv o.val ∧ o.val.length = rep.val.length ∧
        repN (vals o.val) = mergeAll (repN (vals rep.val)) P)
    ?_ ?_ _ ⟨hinv, rfl, by simp [mergeAll]⟩ _ h out rfl
  · rintro ⟨o, j⟩ y hj hb o' hy
    subst hy
    simp only [merged_pairs_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      refine ⟨hj.1, hj.2.1, ?_⟩
      rw [hj.2.2, List.take_of_length_le]
      simp only [P, List.length_map, List.length_zip, vals_length]
      have := get_none hn
      omega
    · split at hb
      · rename_i hn
        cases hb
        refine ⟨hj.1, hj.2.1, ?_⟩
        rw [hj.2.2, List.take_of_length_le]
        simp only [P, List.length_map, List.length_zip, vals_length]
        have := get_none hn
        omega
      · obtain ⟨c, -, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · split at hb
          · cases hb
          · split at hb
            · cases hb
            · obtain ⟨o1, -, hb⟩ := bind_inv hb
              split at hb <;> cases hb
  · rintro ⟨o, j⟩ x' hj hb
    simp only [merged_pairs_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i hv hhv
      split at hb
      · cases hb
      · rename_i bv hbv
        obtain ⟨c, hc, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · rename_i a
          split at hb
          · cases hb
          · rename_i hal
            split at hb
            · cases hb
            · rename_i hbl
              obtain ⟨o1, ho1, hb⟩ := bind_inv hb
              split at hb
              · cases hb
              · rename_i z hz
                cases hb
                have hzv := succ_some hz
                obtain ⟨hjh, -⟩ := get_some hhv
                have hal' : a.val < o.val.length := by simpa using hal
                have hbl' : bv.val < o.val.length := by simpa using hbl
                obtain ⟨hl1, hinv1, hrep1⟩ := merged_ok ho1 hal' hbl' hj.1
                simp only [deref_val] at hl1 hinv1 hrep1
                refine ⟨⟨hinv1, by rw [hl1, hj.2.1], ?_⟩, by simp only; omega⟩
                rw [hrep1, hzv]
                have hP : P.take (j.val + 1) = P.take j.val ++
                    [(Exchange.caseElem nb.val w.val i.val hv.val, bv.val)] := by
                  simp only [P, ← List.map_take]
                  rw [take_zip_succ (by rw [vals_getElem?, hhv]; rfl)
                    (by rw [vals_getElem?, hbv]; rfl)]
                  simp
                rw [hP, mergeAll_append, ← hj.2.2, ← case_elem_ok hc]
                rfl

theorem merged_heads_ok {rep out : alloc.vec.Vec Usize} {rules : Slice Rule} {body : Slice Atom}
    {choice : Slice Usize} {nb w : Usize}
    (h : merged_heads rep rules body choice nb w = ok (some out)) (hinv : Inv rep.val) :
    Inv out.val ∧ out.val.length = rep.val.length ∧
      repN (vals out.val) = mergeAll (repN (vals rep.val))
        ((List.range choice.val.length).flatMap fun i =>
          ((Exchange.chosen (rules.val.map ruleSem) (vals choice.val) i).head.args.zip
            (Exchange.atomAt (atomsSem body.val) i).args).map fun p =>
              (Exchange.caseElem nb.val w.val i p.1, p.2)) := by
  let G : ℕ → List (ℕ × ℕ) := fun i =>
    ((Exchange.chosen (rules.val.map ruleSem) (vals choice.val) i).head.args.zip
      (Exchange.atomAt (atomsSem body.val) i).args).map fun p => (Exchange.caseElem nb.val w.val i p.1, p.2)
  unfold merged_heads merged_heads_loop at h
  refine loop_partial _ (fun p : Usize × alloc.vec.Vec Usize => choice.val.length - p.1.val)
    (fun p => p.1.val ≤ choice.val.length ∧ Inv p.2.val ∧ p.2.val.length = rep.val.length ∧
      repN (vals p.2.val) = mergeAll (repN (vals rep.val)) ((List.range p.1.val).flatMap G))
    (fun r => ∀ o : alloc.vec.Vec Usize, r = some o →
      Inv o.val ∧ o.val.length = rep.val.length ∧
        repN (vals o.val) = mergeAll (repN (vals rep.val)) ((List.range choice.val.length).flatMap G))
    ?_ ?_ _ ⟨by simp, hinv, rfl, by simp [mergeAll]⟩ _ h out rfl
  · rintro ⟨i, o⟩ y hi hb o' hy
    subst hy
    simp only [merged_heads_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      have := get_none hn
      have hie : i.val = choice.val.length := by
        have h1 := hi.1
        simp only at h1
        omega
      refine ⟨hi.2.1, hi.2.2.1, ?_⟩
      rw [hi.2.2.2, hie]
    · split at hb
      · cases hb
      · split at hb
        · cases hb
        · obtain ⟨r, -, hb⟩ := bind_inv hb
          split at hb
          · cases hb
          · split at hb <;> cases hb
  · rintro ⟨i, o⟩ x' hi hb
    simp only [merged_heads_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i index hc
      obtain ⟨hic, -⟩ := get_some hc
      split at hb
      · cases hb
      · rename_i rule hr
        split at hb
        · cases hb
        · rename_i target ht
          obtain ⟨r, hrr, hb⟩ := bind_inv hb
          split at hb
          · cases hb
          · rename_i o1
            split at hb
            · cases hb
            · rename_i z hz
              cases hb
              have hzv := succ_some hz
              obtain ⟨hinv1, hl1, hrep1⟩ := merged_pairs_ok hrr hi.2.1
              simp only [deref_val] at hrep1
              refine ⟨⟨by simp only; omega, hinv1, by rw [hl1, hi.2.2.1], ?_⟩, by simp only; omega⟩
              simp only
              rw [hrep1, hi.2.2.2, hzv, range_succ_flatMap, mergeAll_append]
              congr 1
              simp only [G, chosen_eq hc hr, atomAt_eq ht, ruleSem, atomSem]

theorem repN_range (n : List Usize) (hv : vals n = List.range' 0 n.length) :
    repN (vals n) = id := by
  have := repN_extend [] n (by simpa using hv)
  simp only [List.nil_append] at this
  rw [this]
  funext x
  simp [repN]

theorem initial_ok {rules : Slice Rule} {body : Slice Atom} {choice : Slice Usize} {st : State}
    (h : initial rules body choice = ok (some st)) :
    Inv st.rep.val ∧
      Exchange.initial (rules.val.map ruleSem) (atomsSem body.val) (vals choice.val) = stateSem st := by
  unfold initial at h
  simp only [lift, bind_tc_ok] at h
  obtain ⟨o, ho, h⟩ := bind_inv h
  split at h
  · cases h
  · rename_i nb
    obtain ⟨o1, ho1, h⟩ := bind_inv h
    split at h
    · cases h
    · rename_i w
      split at h
      · cases h
      · rename_i size hsize
        split at h
        · cases h
        · rename_i size1 hsize1
          obtain ⟨o4, ho4, h⟩ := bind_inv h
          split at h
          · cases h
          · rename_i atoms
            obtain ⟨v, hv, h⟩ := bind_inv h
            obtain ⟨o5, ho5, h⟩ := bind_inv h
            split at h
            · cases h
            · rename_i rep
              cases h
              have hnb := width_ok ho
              have hw := rule_width_ok ho1
              have hbodies := case_bodies_ok ho4
              obtain ⟨n, hn, hvn, hlen⟩ := extended_ok hv
              simp only [List.nil_append] at hn
              have hvinv : Inv v.val := by
                rw [hn]
                exact inv_extend (l := []) (fun c hc => by cases hc) (by simpa using hvn)
              obtain ⟨hinv, hl, hrep⟩ := merged_heads_ok ho5 hvinv
              refine ⟨hinv, ?_⟩
              have hid : repN (vals v.val) = id := by
                rw [hn]
                exact repN_range n (by simpa using hvn)
              simp only [Exchange.initial]
              rw [fold_merge]
              apply state_eq
              · simp only [stateSem]
                rw [hbodies, hnb, hw]
                simp
              · simp only [stateSem]
                rw [hrep, hid, hnb, hw]
                simp only [vals_length]
              · simp only [stateSem]
                rw [hl, hlen, add_some hsize1, mul_some hsize, hnb, hw]
                simp

/-! ## The cases -/

/-- A list of rule choices. -/
def choicesSem (l : List (alloc.vec.Vec Usize)) : List (List ℕ) := l.map fun c => vals c.val

@[simp] theorem choicesSem_append (l m : List (alloc.vec.Vec Usize)) :
    choicesSem (l ++ m) = choicesSem l ++ choicesSem m := by
  simp [choicesSem]

theorem choicesSem_take (l : List (alloc.vec.Vec Usize)) (n : ℕ) :
    choicesSem (l.take n) = (choicesSem l).take n := by
  simp [choicesSem, List.map_take]

theorem choicesSem_take_succ {l : List (alloc.vec.Vec Usize)} {i : ℕ} {x : alloc.vec.Vec Usize}
    (h : l[i]? = some x) : (choicesSem l).take (i + 1) = (choicesSem l).take i ++ [vals x.val] := by
  rw [← choicesSem_take, ← choicesSem_take, take_succ_of h, choicesSem_append]
  rfl

theorem prepend_ok {index : Usize} {tail : Slice Usize} {out : alloc.vec.Vec Usize}
    (h : prepend index tail = ok out) : vals out.val = index.val :: vals tail.val := by
  unfold prepend prepend_loop at h
  obtain ⟨o, ho, h⟩ := bind_inv h
  have ho' := push_ok ho
  refine loop_partial _ (fun p : Usize × alloc.vec.Vec Usize => tail.val.length - p.1.val)
    (fun p => vals p.2.val = index.val :: (vals tail.val).take p.1.val)
    (fun o : alloc.vec.Vec Usize => vals o.val = index.val :: vals tail.val)
    ?_ ?_ _ (by simp [ho']) _ h
  · rintro ⟨i, o⟩ y hi hb
    simp only [prepend_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hi, List.take_of_length_le (by simpa using get_none hn)]
    · rename_i v hv
      obtain ⟨hiv, -⟩ := get_some hv
      obtain ⟨o1, -, hb⟩ := bind_inv hb
      split at hb
      · rename_i hz
        exact (succ_none hiv (slice_le tail) hz).elim
      · cases hb
  · rintro ⟨i, o⟩ x' hi hb
    simp only [prepend_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i v hv
      obtain ⟨hiv, -⟩ := get_some hv
      obtain ⟨o1, ho1, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i z hz
        cases hb
        have hzv := succ_some hz
        refine ⟨?_, by simp only; omega⟩
        simp only at hi ⊢
        rw [push_ok ho1, vals_append, hi, hzv, vals_take_succ hv]
        simp

theorem with_cases_ok {out all : alloc.vec.Vec (alloc.vec.Vec Usize)} {index : Usize}
    {tails : Slice (alloc.vec.Vec Usize)} (h : with_cases out index tails = ok all) :
    choicesSem all.val = choicesSem out.val ++ (choicesSem tails.val).map (index.val :: ·) := by
  unfold with_cases with_cases_loop at h
  refine loop_partial _
    (fun p : Usize × alloc.vec.Vec (alloc.vec.Vec Usize) => tails.val.length - p.1.val)
    (fun p => choicesSem p.2.val =
      choicesSem out.val ++ ((choicesSem tails.val).take p.1.val).map (index.val :: ·))
    (fun a : alloc.vec.Vec (alloc.vec.Vec Usize) =>
      choicesSem a.val = choicesSem out.val ++ (choicesSem tails.val).map (index.val :: ·))
    ?_ ?_ _ (by simp) _ h
  · rintro ⟨i, a⟩ y hi hb
    simp only [with_cases_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      rw [hi, List.take_of_length_le (by simpa [choicesSem] using get_none hn)]
    · rename_i t ht
      obtain ⟨hit, -⟩ := get_some ht
      obtain ⟨v, -, hb⟩ := bind_inv hb
      obtain ⟨a1, -, hb⟩ := bind_inv hb
      split at hb
      · rename_i hz
        exact (succ_none hit (slice_le tails) hz).elim
      · cases hb
  · rintro ⟨i, a⟩ x' hi hb
    simp only [with_cases_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i t ht
      obtain ⟨hit, -⟩ := get_some ht
      obtain ⟨v, hv, hb⟩ := bind_inv hb
      obtain ⟨a1, ha1, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i z hz
        cases hb
        have hzv := succ_some hz
        refine ⟨?_, by simp only; omega⟩
        simp only at hi ⊢
        have hpre := prepend_ok hv
        simp only [deref_val] at hpre
        rw [push_ok ha1, choicesSem_append, hi, hzv, choicesSem_take_succ ht]
        simp [choicesSem, hpre]

/-- The filter of `Norn.Sem.Exchange.cases`. -/
def fits (rules : List Exchange.Rule) (b : Exchange.Atom) (m : ℕ) : Bool :=
  match rules[m]? with
  | some r => decide (r.head.rel = b.rel ∧ r.head.args.length = b.args.length)
  | none => false

theorem cases_cons (rules : List Exchange.Rule) (b : Exchange.Atom) (bs : List Exchange.Atom) :
    Exchange.cases rules (b :: bs) =
      ((List.range rules.length).filter (fits rules b)).flatMap fun m =>
        (Exchange.cases rules bs).map (m :: ·) := rfl

theorem produces_ok {rule : Rule} {atom : Atom} {b : Bool} (h : produces rule atom = ok b) :
    (b = true ↔ (ruleSem rule).head.rel = (atomSem atom).rel ∧
      (ruleSem rule).head.args.length = (atomSem atom).args.length) := by
  unfold produces at h
  split at h
  · rename_i hrel
    cases h
    simp only [decide_eq_true_eq, ruleSem, atomSem, hrel, vals_length, true_and]
    constructor
    · intro he
      simpa using congrArg UScalar.val he
    · intro he
      exact UScalar.eq_of_val_eq (by simpa using he)
  · rename_i hrel
    cases h
    simp only [Bool.false_eq_true, false_iff, ruleSem, atomSem, not_and]
    intro hv
    exact absurd (UScalar.eq_of_val_eq hv) hrel

theorem cases_for_ok {rules : Slice Rule} {atom : Atom} {tails : Slice (alloc.vec.Vec Usize)}
    {out : alloc.vec.Vec (alloc.vec.Vec Usize)} (h : cases_for rules atom tails = ok out) :
    choicesSem out.val = ((List.range rules.val.length).filter
      (fits (rules.val.map ruleSem) (atomSem atom))).flatMap fun m =>
        (choicesSem tails.val).map (m :: ·) := by
  let F : ℕ → List (List ℕ) := fun m => (choicesSem tails.val).map (m :: ·)
  let p := fits (rules.val.map ruleSem) (atomSem atom)
  unfold cases_for cases_for_loop at h
  refine loop_partial _
    (fun q : Usize × alloc.vec.Vec (alloc.vec.Vec Usize) => rules.val.length - q.1.val)
    (fun q => q.1.val ≤ rules.val.length ∧
      choicesSem q.2.val = ((List.range q.1.val).filter p).flatMap F)
    (fun o : alloc.vec.Vec (alloc.vec.Vec Usize) =>
      choicesSem o.val = ((List.range rules.val.length).filter p).flatMap F)
    ?_ ?_ _ (by simp [choicesSem]) _ h
  · rintro ⟨i, o⟩ y hi hb
    simp only [cases_for_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      cases hb
      have := get_none hn
      have hie : i.val = rules.val.length := by
        have h1 := hi.1
        simp only at h1
        omega
      rw [hi.2, hie]
    · rename_i rule hr
      obtain ⟨hir, -⟩ := get_some hr
      obtain ⟨b, -, hb⟩ := bind_inv hb
      obtain ⟨o1, -, hb⟩ := bind_inv hb
      split at hb
      · rename_i hz
        exact (succ_none hir (slice_le rules) hz).elim
      · cases hb
  · rintro ⟨i, o⟩ x' hi hb
    simp only [cases_for_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i rule hr
      obtain ⟨hir, hre⟩ := get_some hr
      obtain ⟨b, hbp, hb⟩ := bind_inv hb
      obtain ⟨o1, ho1, hb⟩ := bind_inv hb
      split at hb
      · cases hb
      · rename_i z hz
        cases hb
        have hzv := succ_some hz
        refine ⟨⟨by simp only; omega, ?_⟩, by simp only; omega⟩
        simp only
        have hfit : p i.val = b := by
          have hp := produces_ok hbp
          simp only [p, fits, List.getElem?_map, hr, Option.map_some]
          cases b
          · simp only [decide_eq_false_iff_not]
            exact fun hc => absurd (hp.mpr hc) (by simp)
          · simp only [decide_eq_true_eq]
            exact hp.mp rfl
        rw [hzv, List.range_succ, List.filter_append, List.flatMap_append]
        simp only [List.filter_cons, List.filter_nil, hfit]
        split at ho1
        · rename_i hbt
          subst hbt
          rw [with_cases_ok ho1, hi.2]
          simp [F]
        · rename_i hbt
          cases ho1
          simp only [Bool.not_eq_true] at hbt
          subst hbt
          simp [hi.2]

theorem drop_cons_of {α : Type} {l : List α} {i : ℕ} {x : α} (h : l[i]? = some x) :
    l.drop i = x :: l.drop (i + 1) := by
  obtain ⟨hi, rfl⟩ := get_some h
  exact List.drop_eq_getElem_cons hi

theorem case_choices_ok {rules : Slice Rule} {body : Slice Atom}
    {all : alloc.vec.Vec (alloc.vec.Vec Usize)} (h : case_choices rules body = ok all) :
    choicesSem all.val = Exchange.cases (rules.val.map ruleSem) (atomsSem body.val) := by
  unfold case_choices case_choices_loop at h
  obtain ⟨o, ho, h⟩ := bind_inv h
  have ho' := push_ok ho
  refine loop_partial _ (fun q : alloc.vec.Vec (alloc.vec.Vec Usize) × Usize => q.2.val)
    (fun q => q.2.val ≤ body.val.length ∧ choicesSem q.1.val =
      Exchange.cases (rules.val.map ruleSem) (atomsSem (body.val.drop q.2.val)))
    (fun a : alloc.vec.Vec (alloc.vec.Vec Usize) =>
      choicesSem a.val = Exchange.cases (rules.val.map ruleSem) (atomsSem body.val))
    ?_ ?_ _ ⟨by simp, by simp [ho', choicesSem, atomsSem, Exchange.cases]⟩ _ h
  · rintro ⟨a, r⟩ y hI hb
    simp only [case_choices_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hs
      cases hb
      have hr0 : r.val = 0 := by
        have := Usize.checked_sub_bv_spec r 1#usize
        rw [hs] at this
        simp at this
        omega
      rw [hI.2, hr0, List.drop_zero]
    · rename_i idx hidx
      split at hb
      · rename_i hn
        have h1 := get_none hn
        have h2 := (sub_some hidx).2
        have h3 := hI.1
        have h4 := (sub_some hidx).1
        simp only at h3
        simp at h2 h4
        omega
      · obtain ⟨o1, -, hb⟩ := bind_inv hb
        cases hb
  · rintro ⟨a, r⟩ x' hI hb
    simp only [case_choices_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i idx hidx
      split at hb
      · cases hb
      · rename_i atom hat
        obtain ⟨o1, ho1, hb⟩ := bind_inv hb
        cases hb
        obtain ⟨hle, hiv⟩ := sub_some hidx
        simp only at hle hiv hI ⊢
        refine ⟨⟨by obtain ⟨hia, -⟩ := get_some hat; omega, ?_⟩, by simp at hle hiv; omega⟩
        have hcf := cases_for_ok ho1
        simp only [deref_val] at hcf
        rw [hcf, drop_cons_of hat]
        have hr : idx.val + 1 = r.val := by simp at hle hiv; omega
        rw [hr, show atomsSem (atom :: body.val.drop r.val) =
          atomSem atom :: atomsSem (body.val.drop r.val) from rfl, cases_cons, ← hI.2]
        rw [List.length_map]

/-! ## Every case's trace -/

theorem zip_all_of {α β : Type} {l : List α} {m : List β} {p : α × β → Bool}
    (h : ∀ k (h1 : k < l.length) (h2 : k < m.length), p (l[k], m[k]) = true) :
    (l.zip m).all p = true := by
  rw [List.all_eq_true]
  intro x hx
  obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hx
  rw [List.getElem_zip]
  simp only [List.length_zip] at hk
  exact h k (by omega) (by omega)

/-- A trace. -/
def traceSem (t : alloc.vec.Vec Step) : List Exchange.Step := t.val.map stepSem

theorem traces_merge_ok {theory : Slice Dep} {rules : Slice Rule} {body : Slice Atom}
    {l r : Usize} {cases : Slice (alloc.vec.Vec Usize)} {traces : Slice (alloc.vec.Vec Step)}
    (h : traces_merge theory rules body l r cases traces = ok true) :
    ∀ k (h1 : k < cases.val.length) (h2 : k < traces.val.length),
      (match Exchange.run (theory.val.map depSem)
          (Exchange.initial (rules.val.map ruleSem) (atomsSem body.val) (vals cases.val[k].val))
          (traceSem traces.val[k]) with
        | some s => decide (s.rep l.val = s.rep r.val)
        | none => false) = true := by
  let P : ℕ → Prop := fun k => ∀ (h1 : k < cases.val.length) (h2 : k < traces.val.length),
    (match Exchange.run (theory.val.map depSem)
        (Exchange.initial (rules.val.map ruleSem) (atomsSem body.val) (vals cases.val[k].val))
        (traceSem traces.val[k]) with
      | some s => decide (s.rep l.val = s.rep r.val)
      | none => false) = true
  unfold traces_merge traces_merge_loop at h
  refine loop_partial _ (fun i : Usize => cases.val.length - i.val)
    (fun i => ∀ k < i.val, P k) (fun b => b = true → ∀ k, P k)
    ?_ ?_ _ (fun k hk => absurd hk (by simp)) _ h rfl
  · intro i b hi hb hbt
    subst hbt
    simp only [traces_merge_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · rename_i hn
      have := get_none hn
      intro k h1 h2
      exact hi k (by omega) h1 h2
    · split at hb
      · cases hb
      · obtain ⟨o, -, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · obtain ⟨o', -, hb⟩ := bind_inv hb
          split at hb
          · cases hb
          · obtain ⟨x, -, hb⟩ := bind_inv hb
            obtain ⟨y, -, hb⟩ := bind_inv hb
            split at hb
            · cases hb
            · split at hb <;> cases hb
  · intro i i' hi hb
    simp only [traces_merge_loop.body, slice_get, bind_tc_ok, lift] at hb
    split at hb
    · cases hb
    · rename_i choice hc
      obtain ⟨hic, hce⟩ := get_some hc
      split at hb
      · cases hb
      · rename_i trace ht
        obtain ⟨hit, hte⟩ := get_some ht
        obtain ⟨o, ho, hb⟩ := bind_inv hb
        split at hb
        · cases hb
        · rename_i start
          obtain ⟨o', ho', hb⟩ := bind_inv hb
          split at hb
          · cases hb
          · rename_i last
            obtain ⟨x, hx, hb⟩ := bind_inv hb
            obtain ⟨y, hy, hb⟩ := bind_inv hb
            split at hb
            · cases hb
            · rename_i hxy
              split at hb
              · cases hb
              · rename_i z hz
                cases hb
                have hzv := succ_some hz
                refine ⟨fun k hk => ?_, by omega⟩
                rcases Nat.lt_or_ge k i.val with hki | hki
                · exact hi k hki
                · have hke : k = i.val := by omega
                  subst hke
                  intro h1 h2
                  obtain ⟨hinv, hinit⟩ := initial_ok ho
                  have hrun := run_ok ho' hinv
                  simp only [deref_val] at hinit hrun
                  rw [hce, hte]
                  simp only [traceSem]
                  rw [hinit, hrun]
                  simp only [decide_eq_true_eq, stateSem]
                  have hx' := rep_of_ok hx
                  have hy' := rep_of_ok hy
                  simp only [deref_val] at hx' hy'
                  have hxy' : x = y := UScalar.eq_of_val_eq (by simpa using hxy)
                  rw [← hx', ← hy', hxy']

/-! ## The check -/

/-- **The production check accepts only what `Norn.Sem.Exchange.check` accepts.** -/
theorem check_correct (theory : Slice Dep) (rules : Slice Rule) (body : Slice Atom)
    (l r : Usize) (traces : Slice (alloc.vec.Vec Step))
    (h : check theory rules body l r traces = ok true) :
    Exchange.check (theory.val.map depSem) (rules.val.map ruleSem) (atomsSem body.val) l.val r.val
      (traces.val.map traceSem) = true := by
  unfold check at h
  obtain ⟨b, hb, h⟩ := bind_inv h
  split at h
  · rename_i hbt
    subst hbt
    obtain ⟨b1, hb1, h⟩ := bind_inv h
    split at h
    · rename_i hbt1
      subst hbt1
      obtain ⟨all, hall, h⟩ := bind_inv h
      dsimp only at h
      split at h
      · rename_i hlen
        have hcs := case_choices_ok hall
        have htm := traces_merge_ok h
        simp only [deref_val] at htm
        have hlen' : all.val.length = traces.val.length := by
          simpa using congrArg UScalar.val hlen
        simp only [Exchange.check, Bool.and_eq_true, decide_eq_true_eq]
        refine ⟨⟨⟨(contains_var_ok hb).mp rfl, (contains_var_ok hb1).mp rfl⟩, ?_⟩, ?_⟩
        · rw [← hcs]
          simp [choicesSem, hlen']
        · rw [← hcs]
          apply zip_all_of
          intro k h1 h2
          simp only [choicesSem, List.getElem_map]
          exact htm k (by simpa [choicesSem] using h1) (by simpa using h2)
      · cases h
    · cases h
  · cases h

/-- **Soundness of the production check.** Whenever it accepts, the translated
    path mapping preserves the target EGD on every source instance over every
    domain. -/
theorem check_sound (theory : Slice Dep) (rules : Slice Rule) (body : Slice Atom)
    (l r : Usize) (traces : Slice (alloc.vec.Vec Step))
    (h : check theory rules body l r traces = ok true) :
    Exchange.Preserves (theory.val.map depSem) (rules.val.map ruleSem) (atomsSem body.val)
      l.val r.val :=
  Exchange.check_sound _ _ _ _ _ _ (check_correct theory rules body l r traces h)

end Norn.Checker.Preservation
