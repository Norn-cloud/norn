import DeciderCore.Funs
import Decider.Reference

/-!
# Production `fold_events` and `replay_events` meet the ledger fold

`DeciderCore` is the fresh Aeneas translation of the production file
`crates/norn-kernel/src/decider_fold.rs`. Every theorem here quantifies over
every `Evolve` implementation (every decider) and every ledger slice: the
production fold is exactly the left fold of `evolve`, failure included, and
the decider laws the kernel relies on follow for the production code.
-/

namespace Norn.Kernel.Decider
open Aeneas Aeneas.Std Result
open norn_kernel_decider

section Result

variable {S E : Type}

@[simp] theorem result_pure (x : S) : (pure x : Result S) = ok x := rfl

/-- In `Result`, a fold over an append is the fold of the prefix, then the
    fold of the suffix from where the prefix ended. -/
theorem foldlM_append (f : S → E → Result S) (s : S) (xs ys : List E) :
    List.foldlM f s (xs ++ ys) = (do let s' ← List.foldlM f s xs; List.foldlM f s' ys) := by
  induction xs generalizing s with
  | nil => simp
  | cons x xs ih =>
    simp only [List.cons_append, List.foldlM_cons]
    cases h : f s x <;> simp [ih]

end Result

section Fold

variable {D S E : Type} (inst : Evolve D S E) (d : D) (events : Slice E)

/-- The loop invariant: from index `i`, the loop folds the events from `i` on. -/
private theorem fold_loop (n : Nat) : ∀ (i : Usize) (st : S), events.length - i.val = n →
    i.val ≤ events.length →
    fold_events_loop inst i d events st =
      List.foldlM (inst.evolve d) st (events.val.drop i.val) := by
  induction n with
  | zero =>
    intro i st hn hle
    have hi : i.val = events.length := by omega
    unfold fold_events_loop
    rw [loop.eq_1]
    have hnone : events.val[i.val]? = none := by
      rw [List.getElem?_eq_none_iff]; simp only [Slice.length] at hi; omega
    have hdrop : events.val.drop i.val = [] := by
      rw [List.drop_eq_nil_iff]; simp only [Slice.length] at hi; omega
    simp [fold_events_loop.body, core.slice.Slice.get, Slice.getElem?_Usize_eq, hnone, hdrop]
  | succ n ih =>
    intro i st hn hle
    have hlt : i.val < events.val.length := by simp only [Slice.length] at hn; omega
    have hget : events.val[i.val]? = some (events.val[i.val]'hlt) :=
      List.getElem?_eq_getElem hlt
    have hdrop : events.val.drop i.val =
        events.val[i.val]'hlt :: events.val.drop (i.val + 1) :=
      List.drop_eq_getElem_cons hlt
    have hmax := Slice.length_ineq events
    have hadd := Usize.checked_add_bv_spec i 1#usize
    cases hca : Usize.checked_add i 1#usize with
    | none =>
      rw [hca] at hadd
      simp at hadd
      omega
    | some next =>
      rw [hca] at hadd
      obtain ⟨-, hnext, -⟩ := hadd
      have hnext' : next.val = i.val + 1 := by simpa using hnext
      unfold fold_events_loop
      rw [loop.eq_1]
      rw [hdrop, List.foldlM_cons]
      cases hev : inst.evolve d st (events.val[i.val]'hlt) with
      | ok st' =>
        have hrest := ih next st' (by simp only [Slice.length] at hn ⊢; omega)
          (by simp only [Slice.length] at hlt ⊢; omega)
        have hbody : fold_events_loop.body inst d events i st =
            ok (ControlFlow.cont (next, st')) := by
          simp [fold_events_loop.body, core.slice.Slice.get, Slice.getElem?_Usize_eq, hget,
            hev, hca, lift]
        unfold fold_events_loop at hrest
        rw [hnext'] at hrest
        dsimp only
        rw [hbody]
        simpa using hrest
      | fail e =>
        simp [fold_events_loop.body, core.slice.Slice.get, Slice.getElem?_Usize_eq, hget, hev]
      | div =>
        simp [fold_events_loop.body, core.slice.Slice.get, Slice.getElem?_Usize_eq, hget, hev]

/-- K6 fold correspondence: for every decider and every ledger slice, the
    production `fold_events` is the left fold of `evolve` over the events,
    first event first; a failing `evolve` fails it at that event. -/
theorem fold_events_correspondence (state : S) :
    fold_events inst d state events = foldLedger (inst.evolve d) state events.val := by
  unfold fold_events foldLedger
  have h := fold_loop inst d events events.length 0#usize state (by simp) (by simp)
  simpa using h

/-- K6 replay correspondence: the production `replay_events` is the fold from
    the decider's initial state. -/
theorem replay_events_correspondence :
    replay_events inst d events =
      replayLedger (inst.initial_state d) (inst.evolve d) events.val := by
  unfold replay_events replayLedger
  cases inst.initial_state d <;> simp [fold_events_correspondence]

/-- Folding composes over a split ledger: folding `whole` equals folding
    `prefix` and then `suffix` from where it ended. -/
theorem fold_events_append (state : S) (pre suf whole : Slice E)
    (hsplit : whole.val = pre.val ++ suf.val) :
    fold_events inst d state whole =
      (do let mid ← fold_events inst d state pre; fold_events inst d mid suf) := by
  simp only [fold_events_correspondence, foldLedger, hsplit, foldlM_append]

/-- Replay is determined by the ledger alone, however it was split: resuming
    from the replay of any prefix reaches the replay of the whole ledger. -/
theorem replay_events_split (pre suf whole : Slice E)
    (hsplit : whole.val = pre.val ++ suf.val) :
    (do let mid ← replay_events inst d pre; fold_events inst d mid suf) =
      replay_events inst d whole := by
  unfold replay_events
  cases inst.initial_state d <;> simp [fold_events_append inst d _ pre suf whole hsplit]

end Fold

end Norn.Kernel.Decider
#print axioms Norn.Kernel.Decider.fold_events_correspondence
#print axioms Norn.Kernel.Decider.replay_events_correspondence
#print axioms Norn.Kernel.Decider.fold_events_append
#print axioms Norn.Kernel.Decider.replay_events_split
