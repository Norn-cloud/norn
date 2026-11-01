import Std

namespace Norn.Verification.CoreValueLedger

/-!
`norn.core` laws for value: conservation, immutability and linked reversals.

This is a mathematical reference for the `norn.core` physics pack. It has no
axioms, `sorry` or `admit`. The book below admits value only through two
operations, `post` and `reverse`, the way the kernel admits journal legs only
through the cell commit check. Every theorem is about this model. It is not a
refinement proof for the Rust kernel, checked `i128` arithmetic, serialization
or storage; that correspondence is tested, not proved.

Laws (ids as cited by the core pack):
* `core.law.value_conservation`: every admitted journal balances in every unit,
  and so does every book reachable through the primitives.
* `core.law.posted_immutable`: no operation edits or removes a posted entry;
  each admitted operation adds exactly one entry in front of the old ones.
* `core.law.correction_is_linked_reversal`: a correction is a new entry linked
  to its original, with exactly negated legs, so the pair nets to zero on every
  account and unit. An entry is reversed at most once and a reversal is never
  itself reversed.
-/

structure Leg where
  account : Nat
  unit : Nat
  amount : Int
  deriving DecidableEq

/-- Net amount of the legs whose `key` is `k` (an account or a unit). -/
def sumBy (key : Leg → Nat) (k : Nat) : List Leg → Int
  | [] => 0
  | l :: ls => (if key l = k then l.amount else 0) + sumBy key k ls

/-- A journal balances when it nets to zero in every unit. -/
def Balanced (legs : List Leg) : Prop :=
  ∀ u, sumBy Leg.unit u legs = 0

/-- The executable admission check: every unit that occurs nets to zero. -/
def checkBalanced (legs : List Leg) : Bool :=
  legs.all (fun l => sumBy Leg.unit l.unit legs == 0)

/-- The exact reversal of a journal: same accounts and units, negated amounts. -/
def negate : List Leg → List Leg
  | [] => []
  | l :: ls => ⟨l.account, l.unit, -l.amount⟩ :: negate ls

theorem sumBy_absent (key : Leg → Nat) (k : Nat) :
    ∀ legs : List Leg, (∀ l ∈ legs, key l ≠ k) → sumBy key k legs = 0
  | [], _ => rfl
  | l :: ls, h => by
      have hl : key l ≠ k := h l (by simp)
      have hr := sumBy_absent key k ls (fun x hx => h x (by simp [hx]))
      simp [sumBy, hl, hr]

theorem sumBy_append (key : Leg → Nat) (k : Nat) :
    ∀ a b : List Leg, sumBy key k (a ++ b) = sumBy key k a + sumBy key k b
  | [], b => by simp [sumBy]
  | l :: ls, b => by
      have ih := sumBy_append key k ls b
      simp [sumBy, ih, Int.add_assoc]

theorem sumBy_negate_unit (u : Nat) :
    ∀ legs : List Leg, sumBy Leg.unit u (negate legs) = -sumBy Leg.unit u legs
  | [] => by simp [negate, sumBy]
  | l :: ls => by
      have ih := sumBy_negate_unit u ls
      by_cases h : l.unit = u <;> simp [negate, sumBy, h, ih] <;> omega

theorem sumBy_negate_account (a : Nat) :
    ∀ legs : List Leg, sumBy Leg.account a (negate legs) = -sumBy Leg.account a legs
  | [] => by simp [negate, sumBy]
  | l :: ls => by
      have ih := sumBy_negate_account a ls
      by_cases h : l.account = a <;> simp [negate, sumBy, h, ih] <;> omega

/-- The admission check is exact: it accepts precisely the balanced journals. -/
theorem checkBalanced_iff (legs : List Leg) : checkBalanced legs = true ↔ Balanced legs := by
  constructor
  · intro h u
    by_cases hu : ∃ l ∈ legs, l.unit = u
    · obtain ⟨l, hl, rfl⟩ := hu
      have hall := List.all_eq_true.mp h l hl
      simpa using hall
    · exact sumBy_absent Leg.unit u legs (fun l hl heq => hu ⟨l, hl, heq⟩)
  · intro h
    exact List.all_eq_true.mpr (fun l _ => by simp [h l.unit])

/-- Exchanges compose: two balanced journals posted together still balance. -/
theorem balanced_append {a b : List Leg} (ha : Balanced a) (hb : Balanced b) :
    Balanced (a ++ b) := by
  intro u
  simp [sumBy_append, ha u, hb u]

theorem balanced_negate {legs : List Leg} (h : Balanced legs) : Balanced (negate legs) := by
  intro u
  simp [sumBy_negate_unit, h u]

/-- A journal and its reversal net to zero on every account. -/
theorem reversal_nets_account (legs : List Leg) (a : Nat) :
    sumBy Leg.account a (legs ++ negate legs) = 0 := by
  rw [sumBy_append, sumBy_negate_account]
  omega

/-- A journal and its reversal net to zero in every unit. -/
theorem reversal_nets_unit (legs : List Leg) (u : Nat) :
    sumBy Leg.unit u (legs ++ negate legs) = 0 := by
  rw [sumBy_append, sumBy_negate_unit]
  omega

structure Entry where
  id : Nat
  legs : List Leg
  reverses : Option Nat

/-- Entries are kept newest first; nothing outside `step` changes them. -/
abbrev Book := List Entry

def idCount (id : Nat) : Book → Nat
  | [] => 0
  | e :: es => (if e.id = id then 1 else 0) + idCount id es

def reversalCount (target : Nat) : Book → Nat
  | [] => 0
  | e :: es => (if e.reverses = some target then 1 else 0) + reversalCount target es

def lookup (id : Nat) : Book → Option Entry
  | [] => none
  | e :: es => if e.id = id then some e else lookup id es

/-- The only two ways to change a book. There is no edit and no delete. -/
inductive Op where
  | post (id : Nat) (legs : List Leg)
  | reverse (id : Nat) (target : Nat)

def step (book : Book) : Op → Option Book
  | .post id legs =>
      if idCount id book = 0 ∧ checkBalanced legs = true then
        some (⟨id, legs, none⟩ :: book)
      else none
  | .reverse id target =>
      match lookup target book with
      | none => none
      | some original =>
          if idCount id book = 0 ∧ original.reverses = none ∧ reversalCount target book = 0 then
            some (⟨id, negate original.legs, some target⟩ :: book)
          else none

/-- A workflow is any sequence of primitive operations; it stops at the first refusal. -/
def exec : Book → List Op → Option Book
  | book, [] => some book
  | book, op :: ops =>
      match step book op with
      | none => none
      | some next => exec next ops

theorem lookup_mem {id : Nat} : ∀ {book : Book} {e : Entry}, lookup id book = some e → e ∈ book
  | [], _, h => by simp [lookup] at h
  | x :: xs, e, h => by
      unfold lookup at h
      split at h
      · simp only [Option.some.injEq] at h
        subst h
        simp
      · exact List.mem_cons_of_mem _ (lookup_mem h)

theorem lookup_id {id : Nat} : ∀ {book : Book} {e : Entry}, lookup id book = some e → e.id = id
  | [], _, h => by simp [lookup] at h
  | x :: xs, e, h => by
      unfold lookup at h
      split at h
      · simp only [Option.some.injEq] at h
        subst h
        assumption
      · exact lookup_id h

/-- `core.law.posted_immutable`: every admitted operation only prepends one entry. -/
theorem step_append_only {book next : Book} {op : Op} (h : step book op = some next) :
    ∃ e, next = e :: book := by
  cases op with
  | post id legs =>
      simp only [step] at h
      split at h
      · exact ⟨_, (Option.some.inj h).symm⟩
      · contradiction
  | reverse id target =>
      simp only [step] at h
      split at h
      · contradiction
      · split at h
        · exact ⟨_, (Option.some.inj h).symm⟩
        · contradiction

/-- Posted entries survive every workflow unchanged, in order. -/
theorem run_append_only : ∀ {book next : Book} {ops : List Op},
    exec book ops = some next → ∃ added, next = added ++ book
  | book, next, [], h => ⟨[], by simp [exec] at h; simp [h]⟩
  | book, next, op :: ops, h => by
      unfold exec at h
      split at h
      · contradiction
      · rename_i mid hmid
        obtain ⟨e, rfl⟩ := step_append_only hmid
        obtain ⟨added, rfl⟩ := run_append_only h
        exact ⟨added ++ [e], by simp⟩

def AllBalanced (book : Book) : Prop := ∀ e ∈ book, Balanced e.legs

/-- `core.law.value_conservation`, one step. -/
theorem step_balanced {book next : Book} {op : Op}
    (hbook : AllBalanced book) (h : step book op = some next) : AllBalanced next := by
  cases op with
  | post id legs =>
      simp only [step] at h
      split at h
      · rename_i guard
        rw [← Option.some.inj h]
        intro e he
        rcases List.mem_cons.mp he with rfl | he
        · exact (checkBalanced_iff legs).mp guard.2
        · exact hbook e he
      · contradiction
  | reverse id target =>
      simp only [step] at h
      split at h
      · contradiction
      · rename_i original horig
        split at h
        · rw [← Option.some.inj h]
          intro e he
          rcases List.mem_cons.mp he with rfl | he
          · exact balanced_negate (hbook original (lookup_mem horig))
          · exact hbook e he
        · contradiction

/--
Inheritance (the LCF-style claim): any workflow built only from `post` and
`reverse`, starting from a book whose entries balance, ends with every entry
balanced. Workflows do not re-prove the law; they inherit it from the primitive.
-/
theorem run_balanced : ∀ {book next : Book} {ops : List Op},
    AllBalanced book → exec book ops = some next → AllBalanced next
  | book, next, [], hbook, h => by
      simp [exec] at h
      rw [← h]
      exact hbook
  | book, next, op :: ops, hbook, h => by
      unfold exec at h
      split at h
      · contradiction
      · rename_i mid hmid
        exact run_balanced (step_balanced hbook hmid) h

/-- Trial balance of a book in one unit. -/
def trial (u : Nat) : Book → Int
  | [] => 0
  | e :: es => sumBy Leg.unit u e.legs + trial u es

theorem trial_zero : ∀ {book : Book}, AllBalanced book → ∀ u, trial u book = 0
  | [], _, _ => rfl
  | e :: es, h, u => by
      have he := h e (by simp)
      have hes : AllBalanced es := fun x hx => h x (by simp [hx])
      simp [trial, he u, trial_zero hes u]

/-- Every workflow from the empty book keeps the trial balance at zero in every unit. -/
theorem run_trial_zero {next : Book} {ops : List Op} (h : exec [] ops = some next) (u : Nat) :
    trial u next = 0 :=
  trial_zero (run_balanced (fun _ he => by simp at he) h) u

/-- A reversal links to an existing original that is not itself a reversal, and negates it exactly. -/
theorem reverse_is_linked_exact {book next : Book} {id target : Nat}
    (h : step book (.reverse id target) = some next) :
    ∃ original, original ∈ book ∧ original.id = target ∧ original.reverses = none ∧
      next = ⟨id, negate original.legs, some target⟩ :: book := by
  simp only [step] at h
  split at h
  · contradiction
  · rename_i original horig
    split at h
    · rename_i guard
      exact ⟨original, lookup_mem horig, lookup_id horig, guard.2.1, (Option.some.inj h).symm⟩
    · contradiction

def ReversedAtMostOnce (book : Book) : Prop := ∀ t, reversalCount t book ≤ 1

/-- `core.law.correction_is_linked_reversal`: no entry is ever reversed twice. -/
theorem step_reversed_at_most_once {book next : Book} {op : Op}
    (hbook : ReversedAtMostOnce book) (h : step book op = some next) :
    ReversedAtMostOnce next := by
  cases op with
  | post id legs =>
      simp only [step] at h
      split at h
      · rw [← Option.some.inj h]
        intro t
        simpa [reversalCount] using hbook t
      · contradiction
  | reverse id target =>
      simp only [step] at h
      split at h
      · contradiction
      · split at h
        · rename_i guard
          rw [← Option.some.inj h]
          intro t
          by_cases ht : target = t
          · subst ht
            simp [reversalCount, guard.2.2]
          · have := hbook t
            simp [reversalCount, ht] <;> omega
        · contradiction

theorem run_reversed_at_most_once : ∀ {book next : Book} {ops : List Op},
    ReversedAtMostOnce book → exec book ops = some next → ReversedAtMostOnce next
  | book, next, [], hbook, h => by
      simp [exec] at h
      rw [← h]
      exact hbook
  | book, next, op :: ops, hbook, h => by
      unfold exec at h
      split at h
      · contradiction
      · rename_i mid hmid
        exact run_reversed_at_most_once (step_reversed_at_most_once hbook hmid) h

/-- The admission check refuses an unbalanced journal, so no workflow can post one. -/
theorem unbalanced_post_refused {book : Book} {id : Nat} {legs : List Leg}
    (h : ¬ Balanced legs) : step book (.post id legs) = none := by
  simp only [step]
  have : checkBalanced legs = false := by
    cases hc : checkBalanced legs
    · rfl
    · exact absurd ((checkBalanced_iff legs).mp hc) h
  simp [this]

end Norn.Verification.CoreValueLedger
