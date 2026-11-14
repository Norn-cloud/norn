import Aeneas

/-!
# Reasoning about extracted production code

Generic facts about the Aeneas `Result` monad, the `loop` combinator, slices and
`usize` increments, used by the certificate-checker correspondence proofs. They
are proved here from the Aeneas definitions; nothing is assumed.
-/

namespace Norn.Checker.Certificates
open Aeneas Aeneas.Std Result

theorem bind_eq_ok {α β : Type} {x : Result α} {f : α → Result β} {y : β}
    (h : (x >>= f) = ok y) : ∃ a, x = ok a ∧ f a = ok y := by
  cases x with
  | ok a => exact ⟨a, rfl, h⟩
  | fail e => cases h
  | div => cases h

/-- `peel a ha at h`: split `h : (x >>= f) = ok y` into `ha : x = ok a` and
    `h : f a = ok y`, replacing `h` so no stale copy can be picked up later. -/
macro "peel " a:ident ha:ident " at " h:ident : tactic =>
  `(tactic| (replace $h := bind_eq_ok $h; obtain ⟨$a, $ha, $h⟩ := $h))

/-- `peel_ a at h`: as `peel`, discarding the fact about the first step. -/
macro "peel_ " a:ident " at " h:ident : tactic =>
  `(tactic| (replace $h := bind_eq_ok $h; obtain ⟨$a, -, $h⟩ := $h))

/-- Partial correctness of `loop` with a decreasing measure: if every step
    keeps the invariant and decreases the measure, and every exit from an
    invariant state satisfies `Q`, then every result the loop returns does. -/
theorem loop_ok {α β : Type} (body : α → Result (ControlFlow α β)) (μ : α → ℕ)
    (I : α → Prop) (Q : β → Prop)
    (hstep : ∀ x y, I x → body x = ok (.cont y) → I y ∧ μ y < μ x)
    (hdone : ∀ x r, I x → body x = ok (.done r) → Q r) :
    ∀ x r, I x → loop body x = ok r → Q r := by
  suffices H : ∀ n x, μ x = n → ∀ r, I x → loop body x = ok r → Q r from
    fun x r => H _ x rfl r
  intro n
  induction n using Nat.strong_induction_on with
  | _ n ih =>
    intro x hn r hx hl
    rw [loop.eq_1] at hl
    cases hb : body x with
    | ok c =>
      rw [hb] at hl
      cases c with
      | cont y =>
        obtain ⟨hy, hμ⟩ := hstep x y hx hb
        exact ih (μ y) (hn ▸ hμ) y rfl r hy hl
      | done z =>
        have hz : z = r := by injection hl
        subst hz
        exact hdone x z hx hb
    | fail e => rw [hb] at hl; cases hl
    | div => rw [hb] at hl; cases hl

/-- Total correctness of `loop`: an invariant-preserving, measure-decreasing
    body whose every result is `ok` makes the loop return `ok`, satisfying `Q`. -/
theorem loop_total {α β : Type} (body : α → Result (ControlFlow α β)) (μ : α → ℕ)
    (I : α → Prop) (Q : β → Prop)
    (hbody : ∀ x, I x → ∃ c, body x = ok c ∧
      match c with
      | .cont y => I y ∧ μ y < μ x
      | .done r => Q r) :
    ∀ x, I x → ∃ r, loop body x = ok r ∧ Q r := by
  suffices H : ∀ n x, μ x = n → I x → ∃ r, loop body x = ok r ∧ Q r from
    fun x => H _ x rfl
  intro n
  induction n using Nat.strong_induction_on with
  | _ n ih =>
    intro x hn hx
    obtain ⟨c, hb, hc⟩ := hbody x hx
    rw [loop.eq_1, hb]
    cases c with
    | cont y =>
      obtain ⟨hy, hμ⟩ := hc
      exact ih (μ y) (hn ▸ hμ) y rfl hy
    | done z => exact ⟨z, rfl, hc⟩

theorem index_ok {α : Type} {v : Slice α} {i : Usize} {x : α}
    (h : Slice.index_usize v i = ok x) : ∃ hi : i.val < v.length, v.val[i.val] = x := by
  unfold Slice.index_usize at h
  split at h
  · cases h
  · rename_i y hy
    have hyx : y = x := by injection h
    subst hyx
    have hy' : v.val[i.val]? = some y := hy
    obtain ⟨hi, hx⟩ := List.getElem?_eq_some_iff.mp hy'
    exact ⟨hi, hx⟩

theorem index_of_lt {α : Type} (v : Slice α) (i : Usize) (hi : i.val < v.length) :
    Slice.index_usize v i = ok (v.val[i.val]) := by
  unfold Slice.index_usize
  have hs : v[i]? = some (v.val[i.val]) := List.getElem?_eq_getElem hi
  rw [hs]

theorem array_index_of_lt {α : Type} {n : Usize} (a : Std.Array α n) (i : Usize)
    (hi : i.val < a.val.length) : Std.Array.index_usize a i = ok (a.val[i.val]) := by
  unfold Std.Array.index_usize
  have hs : a[i]? = some (a.val[i.val]) := List.getElem?_eq_getElem hi
  rw [hs]

theorem usize_succ {i j : Usize} (h : i + 1#usize = ok j) : j.val = i.val + 1 := by
  have e := UScalar.add_equiv i 1#usize
  rw [h] at e
  simp at e
  omega

theorem usize_succ_ok (i : Usize) (hi : i.val < Usize.max) :
    ∃ j, i + 1#usize = ok j ∧ j.val = i.val + 1 := by
  have e := UScalar.add_equiv i 1#usize
  cases hres : i + 1#usize with
  | ok j =>
    rw [hres] at e
    simp at e
    exact ⟨j, rfl, by omega⟩
  | fail err =>
    rw [hres] at e
    simp [UScalar.inBounds] at e
    have : i.val + 1 ≤ Usize.max := hi
    scalar_tac
  | div => rw [hres] at e; simp at e

theorem branch_ok {T E : Type} {r : core.result.Result T E}
    {cf : core.ops.control_flow.ControlFlow (core.result.Result core.convert.Infallible E) T}
    (h : core.result.Result.Insts.CoreOpsTry.branch r = ok cf) :
    (∃ v, r = .Ok v ∧ cf = .Continue v) ∨ (∃ e, r = .Err e ∧ cf = .Break (.Err e)) := by
  cases r with
  | Ok v =>
    left
    simp [core.result.Result.Insts.CoreOpsTry.branch] at h
    exact ⟨v, rfl, h.symm⟩
  | Err e =>
    right
    simp [core.result.Result.Insts.CoreOpsTry.branch] at h
    exact ⟨e, rfl, h.symm⟩

theorem from_residual_err {T E : Type}
    {res : core.result.Result core.convert.Infallible E} {x : core.result.Result T E}
    (h : core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual
      T (core.convert.FromSame E) res = ok x) : ∃ e, x = .Err e := by
  cases res with
  | Ok v => cases v
  | Err e =>
    simp [core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual] at h
    exact ⟨e, h.symm⟩

end Norn.Checker.Certificates
