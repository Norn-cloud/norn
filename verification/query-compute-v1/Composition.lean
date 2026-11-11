/- COMPUTE-DETERMINISTIC-1: composition of certified one-step maintainers.
   Abstract additive state; each local equation is a separate operator obligation.
   No claim about the Rust lowering or runtime follows from this theorem alone. -/
namespace Norn.IVM

def Correct {A B : Type} (addA : A → A → A) (addB : B → B → B)
    (query : A → B) (delta : A → A → B) : Prop :=
  ∀ old change, query (addA old change) = addB (query old) (delta old change)

theorem composition {A B C : Type}
    (addA : A → A → A) (addB : B → B → B) (addC : C → C → C)
    (f : A → B) (g : B → C) (df : A → A → B) (dg : B → B → C)
    (hf : Correct addA addB f df) (hg : Correct addB addC g dg) :
    Correct addA addC (fun x => g (f x))
      (fun old change => dg (f old) (df old change)) := by
  intro old change
  change g (f (addA old change)) = _
  rw [hf old change]
  exact hg (f old) (df old change)

end Norn.IVM
