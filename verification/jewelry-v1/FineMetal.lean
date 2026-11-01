import Std

namespace Norn.Jewelry

/- Reference semantics for one metal and one fixed custody owner. Fine amounts
are exact integer ticks after purity normalization. This does not prove the
normalizer, runtime posting, valuation, persistence or checked inheritance. -/
structure Position where
  received : Nat
  stock : Nat
  delivered : Nat := 0
  loss : Nat := 0
  deriving DecidableEq

structure State where
  company : Position
  custody : Position
  ledgerVersion : Nat
  events : Nat
  deriving DecidableEq

inductive Action where
  | refine (custody : Bool) (input output loss : Nat)
  | sell (custody : Bool) (amount : Nat)
  | returnCustody (amount : Nat)

def conserved (p : Position) : Prop :=
  p.received = p.stock + p.delivered + p.loss

def valid (s : State) : Prop := conserved s.company ∧ conserved s.custody

def refined (p : Position) (input output loss : Nat) : Position :=
  { p with stock := p.stock - input + output, loss := p.loss + loss }

def delivered (p : Position) (amount : Nat) : Position :=
  { p with stock := p.stock - amount, delivered := p.delivered + amount }

/- Returning none has no candidate writes; publication keeps the entire state.
Loss is mandatory input (zero is explicit), attributed to the same partition. -/
def apply (s : State) : Action → Option State
  | .refine customer input output loss =>
      let p := if customer then s.custody else s.company
      if input > 0 ∧ input ≤ p.stock ∧ input = output + loss then
        if customer then
          some { s with custody := refined p input output loss, events := s.events + 1 }
        else
          some { s with company := refined p input output loss,
                        ledgerVersion := s.ledgerVersion + 1, events := s.events + 1 }
      else none
  | .sell customer amount =>
      if customer = false ∧ amount > 0 ∧ amount ≤ s.company.stock then
        some { s with company := delivered s.company amount,
                      ledgerVersion := s.ledgerVersion + 1, events := s.events + 1 }
      else none
  | .returnCustody amount =>
      if amount > 0 ∧ amount ≤ s.custody.stock then
        some { s with custody := delivered s.custody amount, events := s.events + 1 }
      else none

def publish (s : State) (a : Action) : State := (apply s a).getD s

theorem refining_conserves (p : Position) (input output loss : Nat)
    (old : conserved p) (available : input ≤ p.stock)
    (balanced : input = output + loss) :
    conserved (refined p input output loss) := by
  simp only [conserved, refined] at *
  omega

theorem delivery_conserves (p : Position) (amount : Nat)
    (old : conserved p) (available : amount ≤ p.stock) :
    conserved (delivered p amount) := by
  simp only [conserved, delivered] at *
  omega

theorem accepted_preserves (s next : State) (a : Action)
    (old : valid s) (accepted : apply s a = some next) : valid next := by
  rcases old with ⟨own, held⟩
  cases a with
  | refine customer input output loss =>
      cases customer <;> simp only [apply, Bool.false_eq_true, ↓reduceIte] at accepted
      all_goals
        split at accepted
        · rename_i guard
          simp only [Option.some.injEq] at accepted
          subst next
          simp only [valid]
          constructor
          · first | exact own | exact refining_conserves _ _ _ _ own guard.2.1 guard.2.2
          · first | exact held | exact refining_conserves _ _ _ _ held guard.2.1 guard.2.2
        · simp at accepted
  | sell customer amount =>
      simp only [apply] at accepted
      split at accepted
      · rename_i guard
        simp only [Option.some.injEq] at accepted
        subst next
        exact ⟨delivery_conserves _ _ own guard.2.2, held⟩
      · simp at accepted
  | returnCustody amount =>
      simp only [apply] at accepted
      split at accepted
      · rename_i guard
        simp only [Option.some.injEq] at accepted
        subst next
        exact ⟨own, delivery_conserves _ _ held guard.2⟩
      · simp at accepted

theorem refused_is_noop (s : State) (a : Action) (h : apply s a = none) :
    publish s a = s := by simp [publish, h]

theorem customer_sale_refused (s : State) (amount : Nat) :
    apply s (.sell true amount) = none := by simp [apply]

theorem custody_refining_preserves_assets (s : State) (input output loss : Nat) :
    (publish s (.refine true input output loss)).company = s.company ∧
    (publish s (.refine true input output loss)).ledgerVersion = s.ledgerVersion := by
  simp only [publish, apply, ↓reduceIte]
  split <;> simp

theorem custody_return_preserves_assets (s : State) (amount : Nat) :
    (publish s (.returnCustody amount)).company = s.company ∧
    (publish s (.returnCustody amount)).ledgerVersion = s.ledgerVersion := by
  simp only [publish, apply]
  split <;> simp

theorem history_preserves (actions : List Action) (s : State) (old : valid s) :
    valid (actions.foldl publish s) := by
  induction actions generalizing s with
  | nil => exact old
  | cons a rest ih =>
      apply ih
      cases h : apply s a with
      | none => simpa [publish, h] using old
      | some next => simpa [publish, h] using accepted_preserves s next a old h

end Norn.Jewelry
