import NornSem.Exact

/-!
# Runtime values (`norn_ir::RuntimeValue`)

A runtime value is `null`, a Boolean, an exact number, a string, an ordered
array or an object with unique keys kept in key order.

* Numbers are their exact rational value, so numeric equality is equality in ℚ
  (`1.0` and `1` are the same value, as `rust_decimal`'s `Eq` has it).
  `Value.WF` requires each number to be an exact runtime decimal
  (`Exact.IsExact`).
* Strings are kept as given; NFC normalization belongs to hashing, not to value
  equality.
* Object keys are strictly increasing. Rust's `BTreeMap<String, _>` orders keys
  by UTF-8 bytes, which is the same order as Lean's `String` order on code
  points.
-/

namespace Norn.Sem

/-- A runtime value. -/
inductive Value where
  | null
  | bool (b : Bool)
  | number (q : ℚ)
  | string (s : String)
  | array (items : List Value)
  | object (fields : List (String × Value))

namespace Value

/-- The values `RuntimeValue` admits: exact numbers, and object keys strictly
    increasing (hence unique). -/
inductive WF : Value → Prop
  | null : WF .null
  | bool (b : Bool) : WF (.bool b)
  | number {q : ℚ} : Exact.IsExact q → WF (.number q)
  | string (s : String) : WF (.string s)
  | array {items : List Value} : (∀ v ∈ items, WF v) → WF (.array items)
  | object {fields : List (String × Value)} :
      (fields.map Prod.fst).Pairwise (· < ·) → (∀ p ∈ fields, WF p.2) → WF (.object fields)

/-- Field lookup; `none` on a missing key or a non-object. -/
def field (v : Value) (key : String) : Option Value :=
  match v with
  | .object fields => fields.lookup key
  | _ => none

/-- With strictly increasing keys, a member's key finds that member's value. -/
theorem lookup_of_mem : ∀ {fields : List (String × Value)} {k : String} {v : Value},
    (fields.map Prod.fst).Pairwise (· < ·) → (k, v) ∈ fields → fields.lookup k = some v
  | [], _, _, _, h => by simp at h
  | (k', v') :: rest, k, v, hs, h => by
    rw [List.map_cons, List.pairwise_cons] at hs
    rcases List.mem_cons.mp h with h | h
    · obtain ⟨rfl, rfl⟩ := Prod.mk.inj h
      simp [List.lookup]
    · have hk : k' < k := hs.1 k (List.mem_map.mpr ⟨(k, v), h, rfl⟩)
      have hne : k ≠ k' := fun e => by
        subst e
        exact lt_irrefl _ hk
      have hb : (k == k') = false := beq_false_of_ne hne
      simp only [List.lookup, hb, lookup_of_mem hs.2 h]

/-- A key's lookup result is a member. -/
theorem mem_of_lookup : ∀ {fields : List (String × Value)} {k : String} {v : Value},
    fields.lookup k = some v → (k, v) ∈ fields
  | [], _, _, h => by simp [List.lookup] at h
  | (k', v') :: rest, k, v, h => by
    by_cases hk : k = k'
    · subst hk
      simp only [List.lookup, beq_self_eq_true, Option.some.injEq] at h
      subst h
      simp
    · have hb : (k == k') = false := beq_false_of_ne hk
      simp only [List.lookup, hb] at h
      exact List.mem_cons_of_mem _ (mem_of_lookup h)

/-- In a well-formed object, a field is found exactly when it is a member, and
    then it has that member's value. -/
theorem field_eq_some {fields : List (String × Value)} (h : WF (.object fields))
    {k : String} {v : Value} : (Value.object fields).field k = some v ↔ (k, v) ∈ fields := by
  cases h with
  | object hs _ =>
    constructor
    · intro hl
      simp only [field] at hl
      exact mem_of_lookup hl
    · exact lookup_of_mem hs

/-- Every field of a well-formed object is well-formed. -/
theorem WF.field {fields : List (String × Value)} (h : WF (.object fields))
    {k : String} {v : Value} (hf : (Value.object fields).field k = some v) : WF v := by
  have hm := (field_eq_some h).mp hf
  cases h with
  | object _ hw => exact hw (k, v) hm

end Value

end Norn.Sem
