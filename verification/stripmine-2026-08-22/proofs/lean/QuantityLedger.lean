import Std

namespace Norn.Verification.QuantityLedger

/-!
Mathematical reference only. This file does not establish correspondence to the
production Rust representation, checked i128 arithmetic, or persistence adapter.
It intentionally contains no axioms, `sorry`, or `admit`.
-/

abbrev Amount := Int

def net (legs : List Amount) : Amount := legs.sum

def balanced (legs : List Amount) : Prop := net legs = 0

def reverse (legs : List Amount) : List Amount := legs.map Int.neg

theorem balanced_pair (amount : Amount) : balanced [amount, -amount] := by
  simp only [balanced, net, List.sum_cons, List.sum_nil]
  rw [Int.add_zero, Int.add_right_neg]

theorem net_append (left right : List Amount) :
    net (left ++ right) = net left + net right := by
  simp [net, List.sum_append]

theorem net_reverse (legs : List Amount) : net (reverse legs) = -net legs := by
  induction legs with
  | nil => simp [net, reverse]
  | cons head tail ih =>
      change -head + net (reverse tail) = -(head + net tail)
      rw [ih]
      exact Int.neg_add.symm

theorem balanced_reverse {legs : List Amount} (h : balanced legs) :
    balanced (reverse legs) := by
  unfold balanced at h ⊢
  rw [net_reverse, h]
  simp

theorem balanced_append {left right : List Amount}
    (left_balanced : balanced left) (right_balanced : balanced right) :
    balanced (left ++ right) := by
  unfold balanced at left_balanced right_balanced ⊢
  rw [net_append, left_balanced, right_balanced]
  simp

end Norn.Verification.QuantityLedger
