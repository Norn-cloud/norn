import Std

/-!
# Pure Observation Reducer & Typed Source-Version Ordering (`FEED-MODE-1`, Lane C3 / #1924)

Standalone Lean 4 reference proofs (`import Std` only, zero `sorry`, no custom
axioms) over the algebraic structure implemented in
`crates/norn-ir/src/feed_patch_core.rs` and `crates/norn-ir/src/feed.rs`:

1. **Tri-state `FieldPatch` idempotent monoid & reducer action**:
   * `absent` is left and right identity of `composeField`.
   * `composeField` is associative and idempotent (a band monoid).
   * `reduceField` is a deterministic, idempotent monoid action homomorphism:
     `reduceField (reduceField s p₁ nullVal) p₂ nullVal = reduceField s (composeField p₁ p₂) nullVal`,
     lifting inductively to arbitrary finite patch lists (`reduceField_fold_eq_compose`).

2. **Tri-state `LinkPatchOp` idempotent monoid & reducer action**:
   * Left/right identity (`absent`), associativity, idempotency, reducer homomorphism,
     and finite list-fold homomorphism (`reduceLink_fold_eq_compose`).

3. **Object-level map reducer homomorphism**:
   * Pointwise lifting over key-indexed maps `ObjState K V := K → Option V` and
     `ObjPatch K V := K → FieldPatch V` preserves monoid laws, idempotency,
     homomorphism, and untouched-field preservation (`reduceObj_absent_preserves`).

4. **Typed `SourceVersion` instant ordering & monotonic watermark**:
   * Normalized UTC instants `(epochSeconds, nanos)` with `0 ≤ nanos < 10^9` form a
     strict total order agreeing exactly with `totalNanos = epochSeconds * 10^9 + nanos`.
   * Watermark advancement (`watermarkStep` / `watermarkFold`) is monotone, ignores
     stale versions, and is idempotent on duplicate versions.
   * `VersionKind.opaqueToken` is strictly non-orderable (`opaque_unorderable`).

Evidence scope: checked proofs of the reference mathematical model (`Int` and
polymorphic `Type`). Rust correspondence to `crates/norn-ir/src/feed_patch_core.rs`
via Charon/Aeneas extraction is a separate gate once run on a Charon/Aeneas runner.
-/
namespace Norn.Verification.FeedReducer

/-! ## 1. Tri-state `FieldPatch` monoid and reducer -/

/-- Tri-state field patch operation (`FEED-MODE-1`). -/
inductive FieldPatch (V : Type) where
  | absent
  | explicitNull
  | value (v : V)
  deriving DecidableEq

/-- Chronological composition of two field patches (`earlier` then `later`). -/
def composeField {V : Type} (earlier later : FieldPatch V) : FieldPatch V :=
  match later with
  | .absent => earlier
  | .explicitNull => .explicitNull
  | .value v => .value v

/-- Pure field-level observation reducer. -/
def reduceField {V : Type} (prior : Option V) (patch : FieldPatch V) (nullVal : V) : Option V :=
  match patch with
  | .absent => prior
  | .explicitNull => some nullVal
  | .value v => some v

/-- Chronological fold of a list of field patches into a single composed patch. -/
def foldFieldPatches {V : Type} : List (FieldPatch V) → FieldPatch V
  | [] => .absent
  | p :: ps => composeField p (foldFieldPatches ps)

/-- Sequential reduction of an optional field value through a list of patches. -/
def applyFieldPatches {V : Type} (prior : Option V) (ps : List (FieldPatch V)) (nullVal : V) :
    Option V :=
  match ps with
  | [] => prior
  | p :: ps => applyFieldPatches (reduceField prior p nullVal) ps nullVal

theorem composeField_absent_left {V : Type} (p : FieldPatch V) :
    composeField .absent p = p := by
  cases p <;> rfl

theorem composeField_absent_right {V : Type} (p : FieldPatch V) :
    composeField p .absent = p := rfl

theorem composeField_assoc {V : Type} (p₁ p₂ p₃ : FieldPatch V) :
    composeField (composeField p₁ p₂) p₃ = composeField p₁ (composeField p₂ p₃) := by
  cases p₃ <;> rfl

theorem composeField_idempotent {V : Type} (p : FieldPatch V) :
    composeField p p = p := by
  cases p <;> rfl

theorem reduceField_absent {V : Type} (prior : Option V) (nullVal : V) :
    reduceField prior .absent nullVal = prior := rfl

theorem reduceField_explicitNull {V : Type} (prior : Option V) (nullVal : V) :
    reduceField prior .explicitNull nullVal = some nullVal := rfl

theorem reduceField_value {V : Type} (prior : Option V) (v nullVal : V) :
    reduceField prior (.value v) nullVal = some v := rfl

theorem reduceField_homomorphism {V : Type} (prior : Option V)
    (p₁ p₂ : FieldPatch V) (nullVal : V) :
    reduceField (reduceField prior p₁ nullVal) p₂ nullVal =
      reduceField prior (composeField p₁ p₂) nullVal := by
  cases p₂ <;> rfl

theorem reduceField_idempotent {V : Type} (prior : Option V)
    (p : FieldPatch V) (nullVal : V) :
    reduceField (reduceField prior p nullVal) p nullVal = reduceField prior p nullVal := by
  cases p <;> rfl

theorem reduceField_fold_eq_compose {V : Type} (prior : Option V)
    (ps : List (FieldPatch V)) (nullVal : V) :
    applyFieldPatches prior ps nullVal = reduceField prior (foldFieldPatches ps) nullVal := by
  induction ps generalizing prior with
  | nil => rfl
  | cons p ps ih =>
      simp only [applyFieldPatches, foldFieldPatches]
      rw [ih, reduceField_homomorphism]

/-! ## 2. Tri-state `LinkPatchOp` monoid and reducer -/

/-- Tri-state single-valued link update (`FEED-MODE-1`). -/
inductive LinkPatchOp (T : Type) where
  | absent
  | clear
  | target (t : T)
  deriving DecidableEq

/-- Chronological composition of two link patch operations (`earlier` then `later`). -/
def composeLink {T : Type} (earlier later : LinkPatchOp T) : LinkPatchOp T :=
  match later with
  | .absent => earlier
  | .clear => .clear
  | .target t => .target t

/-- Pure link-level observation reducer. -/
def reduceLink {T : Type} (prior : Option T) (op : LinkPatchOp T) : Option T :=
  match op with
  | .absent => prior
  | .clear => none
  | .target t => some t

/-- Chronological fold of a list of link patch operations. -/
def foldLinkPatches {T : Type} : List (LinkPatchOp T) → LinkPatchOp T
  | [] => .absent
  | op :: ops => composeLink op (foldLinkPatches ops)

/-- Sequential reduction of an optional link target through a list of operations. -/
def applyLinkPatches {T : Type} (prior : Option T) (ops : List (LinkPatchOp T)) : Option T :=
  match ops with
  | [] => prior
  | op :: ops => applyLinkPatches (reduceLink prior op) ops

theorem composeLink_absent_left {T : Type} (op : LinkPatchOp T) :
    composeLink .absent op = op := by
  cases op <;> rfl

theorem composeLink_absent_right {T : Type} (op : LinkPatchOp T) :
    composeLink op .absent = op := rfl

theorem composeLink_assoc {T : Type} (op₁ op₂ op₃ : LinkPatchOp T) :
    composeLink (composeLink op₁ op₂) op₃ = composeLink op₁ (composeLink op₂ op₃) := by
  cases op₃ <;> rfl

theorem composeLink_idempotent {T : Type} (op : LinkPatchOp T) :
    composeLink op op = op := by
  cases op <;> rfl

theorem reduceLink_absent {T : Type} (prior : Option T) :
    reduceLink prior .absent = prior := rfl

theorem reduceLink_clear {T : Type} (prior : Option T) :
    reduceLink prior .clear = none := rfl

theorem reduceLink_target {T : Type} (prior : Option T) (t : T) :
    reduceLink prior (.target t) = some t := rfl

theorem reduceLink_homomorphism {T : Type} (prior : Option T)
    (op₁ op₂ : LinkPatchOp T) :
    reduceLink (reduceLink prior op₁) op₂ = reduceLink prior (composeLink op₁ op₂) := by
  cases op₂ <;> rfl

theorem reduceLink_idempotent {T : Type} (prior : Option T) (op : LinkPatchOp T) :
    reduceLink (reduceLink prior op) op = reduceLink prior op := by
  cases op <;> rfl

theorem reduceLink_fold_eq_compose {T : Type} (prior : Option T)
    (ops : List (LinkPatchOp T)) :
    applyLinkPatches prior ops = reduceLink prior (foldLinkPatches ops) := by
  induction ops generalizing prior with
  | nil => rfl
  | cons op ops ih =>
      simp only [applyLinkPatches, foldLinkPatches]
      rw [ih, reduceLink_homomorphism]

/-! ## 3. Object-level map reducer homomorphism -/

/-- Canonical object state indexed by property key. -/
abbrev ObjState (K V : Type) := K → Option V

/-- Object-level tri-state patch indexed by property key. -/
abbrev ObjPatch (K V : Type) := K → FieldPatch V

/-- Identity object patch (every key omitted). -/
def emptyObjPatch {K V : Type} : ObjPatch K V := fun _ => .absent

/-- Pointwise chronological composition of two object patches. -/
def composeObj {K V : Type} (earlier later : ObjPatch K V) : ObjPatch K V :=
  fun k => composeField (earlier k) (later k)

/-- Pointwise object-level observation reducer. -/
def reduceObj {K V : Type} (prior : ObjState K V) (patch : ObjPatch K V) (nullVal : V) :
    ObjState K V :=
  fun k => reduceField (prior k) (patch k) nullVal

theorem composeObj_absent_left {K V : Type} (p : ObjPatch K V) :
    composeObj emptyObjPatch p = p := by
  funext k
  exact composeField_absent_left (p k)

theorem composeObj_absent_right {K V : Type} (p : ObjPatch K V) :
    composeObj p emptyObjPatch = p := rfl

theorem composeObj_assoc {K V : Type} (p₁ p₂ p₃ : ObjPatch K V) :
    composeObj (composeObj p₁ p₂) p₃ = composeObj p₁ (composeObj p₂ p₃) := by
  funext k
  exact composeField_assoc (p₁ k) (p₂ k) (p₃ k)

theorem reduceObj_homomorphism {K V : Type} (prior : ObjState K V)
    (p₁ p₂ : ObjPatch K V) (nullVal : V) :
    reduceObj (reduceObj prior p₁ nullVal) p₂ nullVal =
      reduceObj prior (composeObj p₁ p₂) nullVal := by
  funext k
  exact reduceField_homomorphism (prior k) (p₁ k) (p₂ k) nullVal

theorem reduceObj_idempotent {K V : Type} (prior : ObjState K V)
    (p : ObjPatch K V) (nullVal : V) :
    reduceObj (reduceObj prior p nullVal) p nullVal = reduceObj prior p nullVal := by
  funext k
  exact reduceField_idempotent (prior k) (p k) nullVal

theorem reduceObj_absent_preserves {K V : Type} (prior : ObjState K V)
    (patch : ObjPatch K V) (nullVal : V) (k : K) (h : patch k = .absent) :
    reduceObj prior patch nullVal k = prior k := by
  simp only [reduceObj, h, reduceField]

/-! ## 4. Typed `SourceVersion` instant ordering and monotonic watermark -/

/-- Three-way comparison outcome matching `feed_patch_core::VersionCmp`. -/
inductive VersionCmp where
  | lt
  | eq
  | gt
  deriving DecidableEq

/-- Declared source-version ordering kinds (`FEED-MODE-1`). -/
inductive VersionKind where
  | lexicographical
  | rfc3339
  | uint64
  | int64
  | opaqueToken
  deriving DecidableEq

/-- Whether versions of a kind admit watermark ordering. -/
def VersionKind.isOrderable : VersionKind → Bool
  | .lexicographical => true
  | .rfc3339 => true
  | .uint64 => true
  | .int64 => true
  | .opaqueToken => false

theorem opaque_unorderable : VersionKind.isOrderable .opaqueToken = false := rfl

/-- Normalized UTC instant `(epochSeconds, nanos)` with `0 ≤ nanos < 10^9`. -/
structure InstantKey where
  epochSeconds : Int
  nanos : Int
  nanos_nonneg : 0 ≤ nanos
  nanos_lt : nanos < 1000000000

/-- Total nanoseconds since the Unix epoch. -/
def InstantKey.totalNanos (k : InstantKey) : Int :=
  k.epochSeconds * 1000000000 + k.nanos

/-- Lexicographic comparison matching `feed_patch_core::compare_instant_core`. -/
def compareInstant (a b : InstantKey) : VersionCmp :=
  if a.epochSeconds < b.epochSeconds then .lt
  else if a.epochSeconds > b.epochSeconds then .gt
  else if a.nanos < b.nanos then .lt
  else if a.nanos > b.nanos then .gt
  else .eq

theorem compareInstant_lt_iff_totalNanos_lt (a b : InstantKey) :
    compareInstant a b = .lt ↔ a.totalNanos < b.totalNanos := by
  have ha0 := a.nanos_nonneg
  have ha1 := a.nanos_lt
  have hb0 := b.nanos_nonneg
  have hb1 := b.nanos_lt
  unfold compareInstant InstantKey.totalNanos
  split
  · constructor
    · intro _; omega
    · intro _; rfl
  · split
    · constructor <;> intro h
      · cases h
      · omega
    · split
      · constructor
        · intro _; omega
        · intro _; rfl
      · split
        · constructor <;> intro h
          · cases h
          · omega
        · constructor <;> intro h
          · cases h
          · omega

theorem compareInstant_eq_iff_totalNanos_eq (a b : InstantKey) :
    compareInstant a b = .eq ↔ a.totalNanos = b.totalNanos := by
  have ha0 := a.nanos_nonneg
  have ha1 := a.nanos_lt
  have hb0 := b.nanos_nonneg
  have hb1 := b.nanos_lt
  unfold compareInstant InstantKey.totalNanos
  split
  · constructor <;> intro h
    · cases h
    · omega
  · split
    · constructor <;> intro h
      · cases h
      · omega
    · split
      · constructor <;> intro h
        · cases h
        · omega
      · split
        · constructor <;> intro h
          · cases h
          · omega
        · constructor
          · intro _; omega
          · intro _; rfl

theorem compareInstant_gt_iff_totalNanos_gt (a b : InstantKey) :
    compareInstant a b = .gt ↔ a.totalNanos > b.totalNanos := by
  have ha0 := a.nanos_nonneg
  have ha1 := a.nanos_lt
  have hb0 := b.nanos_nonneg
  have hb1 := b.nanos_lt
  unfold compareInstant InstantKey.totalNanos
  split
  · constructor <;> intro h
    · cases h
    · omega
  · split
    · constructor
      · intro _; omega
      · intro _; rfl
    · split
      · constructor <;> intro h
        · cases h
        · omega
      · split
        · constructor
          · intro _; omega
          · intro _; rfl
        · constructor <;> intro h
          · cases h
          · omega

theorem compareInstant_refl (a : InstantKey) : compareInstant a a = .eq := by
  exact (compareInstant_eq_iff_totalNanos_eq a a).mpr rfl

theorem compareInstant_antisymm (a b : InstantKey)
    (h : compareInstant a b = .lt) : compareInstant b a = .gt := by
  have hlt : a.totalNanos < b.totalNanos :=
    (compareInstant_lt_iff_totalNanos_lt a b).mp h
  exact (compareInstant_gt_iff_totalNanos_gt b a).mpr (by omega)

theorem compareInstant_trans_lt (a b c : InstantKey)
    (hab : compareInstant a b = .lt) (hbc : compareInstant b c = .lt) :
    compareInstant a c = .lt := by
  have h1 : a.totalNanos < b.totalNanos :=
    (compareInstant_lt_iff_totalNanos_lt a b).mp hab
  have h2 : b.totalNanos < c.totalNanos :=
    (compareInstant_lt_iff_totalNanos_lt b c).mp hbc
  exact (compareInstant_lt_iff_totalNanos_lt a c).mpr (by omega)

/-- Monotonic watermark update: advances strictly when `cand` succeeds `cur`. -/
def watermarkStep (cur cand : InstantKey) : InstantKey :=
  match compareInstant cur cand with
  | .lt => cand
  | .eq => cur
  | .gt => cur

/-- Fold a list of candidate versions through the monotonic watermark step. -/
def watermarkFold (cur : InstantKey) : List InstantKey → InstantKey
  | [] => cur
  | c :: cs => watermarkFold (watermarkStep cur c) cs

theorem watermarkStep_monotone (cur cand : InstantKey) :
    cur.totalNanos ≤ (watermarkStep cur cand).totalNanos := by
  unfold watermarkStep
  cases h : compareInstant cur cand with
  | lt =>
      dsimp only
      have hlt : cur.totalNanos < cand.totalNanos :=
        (compareInstant_lt_iff_totalNanos_lt cur cand).mp h
      omega
  | eq =>
      dsimp only
      exact Int.le_refl _
  | gt =>
      dsimp only
      exact Int.le_refl _

theorem watermarkStep_stale_ignored (cur cand : InstantKey)
    (h : compareInstant cand cur = .lt) : watermarkStep cur cand = cur := by
  have hgt : compareInstant cur cand = .gt := compareInstant_antisymm cand cur h
  simp only [watermarkStep, hgt]

theorem watermarkStep_duplicate_idempotent (cur : InstantKey) :
    watermarkStep cur cur = cur := by
  simp only [watermarkStep, compareInstant_refl]

theorem watermarkFold_monotone (cur : InstantKey) (cs : List InstantKey) :
    cur.totalNanos ≤ (watermarkFold cur cs).totalNanos := by
  induction cs generalizing cur with
  | nil =>
      simp only [watermarkFold]
      omega
  | cons c cs ih =>
      simp only [watermarkFold]
      have hstep : cur.totalNanos ≤ (watermarkStep cur c).totalNanos :=
        watermarkStep_monotone cur c
      have hrest : (watermarkStep cur c).totalNanos ≤
          (watermarkFold (watermarkStep cur c) cs).totalNanos := ih (watermarkStep cur c)
      omega

end Norn.Verification.FeedReducer

#print axioms Norn.Verification.FeedReducer.composeField_assoc
#print axioms Norn.Verification.FeedReducer.reduceField_homomorphism
#print axioms Norn.Verification.FeedReducer.reduceField_idempotent
#print axioms Norn.Verification.FeedReducer.reduceField_fold_eq_compose
#print axioms Norn.Verification.FeedReducer.reduceLink_homomorphism
#print axioms Norn.Verification.FeedReducer.reduceLink_fold_eq_compose
#print axioms Norn.Verification.FeedReducer.reduceObj_homomorphism
#print axioms Norn.Verification.FeedReducer.compareInstant_lt_iff_totalNanos_lt
#print axioms Norn.Verification.FeedReducer.compareInstant_trans_lt
#print axioms Norn.Verification.FeedReducer.watermarkFold_monotone
