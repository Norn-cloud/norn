import NornSem.Value

/-!
# Laws and their exception trees (`norn_ir::LawIR`)

A law constrains every point where it binds. It binds at a point when it is in
scope, its optional condition holds, and its exception tree does not lift it.
What it then requires depends on its modality. Predicates here are their
Boolean denotations at a point of type `σ`. Expression evaluation, and its
refusals, are P5's: an admitted law's predicates are Boolean.

**Exception trees (#1187, #1207).** Each exception names the node it overrides:
the law itself or another exception of the same tree. Its only effect is
lifting. The defeasible reading is a labelling of the exceptions (`Stable`):

> an exception applies exactly when its condition holds and no exception
> overriding it applies.

The law is lifted when some exception overriding the law applies. `lifts` is the
depth-bounded fold that the checker and the commit path compute. For a
`WellFormed` tree (bounded, unique ids distinct from the law, every node reaches
the law), the following hold:

* `stable_exists` and `stable_unique`: exactly one stable labelling exists;
* `lifted_iff`: the fold with fuel `|nodes| + 1` decides lifting under it;
* compiled guard = defeasible semantics.

**Never binds beyond the bare law:** `binds_bare` and `holdsOn_of_bare` show that an
exception tree never makes a law bind where the bare law does not, so it never
adds a requirement. `lifts_needs_top_condition` shows that lifting needs a
first-level exception whose condition holds. This is *not* monotone in the
tree: `nested_exception_can_lift` adds one exception under another and lifts
the law where the smaller tree did not.
-/

namespace Norn.Sem

namespace Law

/-- One declared exception (`LawException`), with its condition's denotation. -/
structure Exception (Id σ : Type) where
  id : Id
  overrides : Id
  condition : σ → Bool

/-- A law's exception tree: the law's own id and its declared exceptions. -/
structure Tree (Id σ : Type) where
  root : Id
  nodes : List (Exception Id σ)

namespace Tree

variable {Id σ : Type}

/-- `MAX_LAW_EXCEPTIONS`. -/
def maxExceptions : ℕ := 64

/-- The declared exception ids, in declaration order. -/
def ids (T : Tree Id σ) : List Id := T.nodes.map Exception.id

/-- Following `overrides` from id `i` reaches the law in exactly `k` steps. -/
inductive ReachesIn (T : Tree Id σ) : Id → ℕ → Prop
  | root : ReachesIn T T.root 0
  | step {e : Exception Id σ} {k : ℕ} :
      e ∈ T.nodes → ReachesIn T e.overrides k → ReachesIn T e.id (k + 1)

/-- The admitted shape: bounded, unique ids distinct from the law, and every
    exception reaches the law by following `overrides` (so no target dangles
    and there is no cycle). -/
structure WellFormed (T : Tree Id σ) : Prop where
  bounded : T.nodes.length ≤ maxExceptions
  nodup : T.ids.Nodup
  rootFresh : T.root ∉ T.ids
  reaches : ∀ e ∈ T.nodes, ∃ k, T.ReachesIn e.id k

theorem mem_ids {T : Tree Id σ} {e : Exception Id σ} (he : e ∈ T.nodes) : e.id ∈ T.ids :=
  List.mem_map.mpr ⟨e, he, rfl⟩

theorem ReachesIn.inv {T : Tree Id σ} {i : Id} {k : ℕ} (h : T.ReachesIn i k) :
    (i = T.root ∧ k = 0) ∨
      ∃ e ∈ T.nodes, ∃ k', i = e.id ∧ k = k' + 1 ∧ T.ReachesIn e.overrides k' := by
  cases h with
  | root => exact Or.inl ⟨rfl, rfl⟩
  | step he h => exact Or.inr ⟨_, he, _, rfl, rfl, h⟩

/-- A child of a node at depth `d` is at depth `d + 1`. -/
theorem ReachesIn.child {T : Tree Id σ} {c : Exception Id σ} {v : Id} {d : ℕ}
    (hc : c ∈ T.nodes) (hov : c.overrides = v) (hv : T.ReachesIn v d) :
    T.ReachesIn c.id (d + 1) := by
  subst hov
  exact .step hc hv

/-- With unique ids distinct from the law, an id has at most one depth. -/
theorem ReachesIn.unique {T : Tree Id σ} (hnd : T.ids.Nodup) (hroot : T.root ∉ T.ids)
    {i : Id} {k₁ k₂ : ℕ} (h₁ : T.ReachesIn i k₁) (h₂ : T.ReachesIn i k₂) : k₁ = k₂ := by
  induction h₁ generalizing k₂ with
  | root =>
    rcases h₂.inv with ⟨_, rfl⟩ | ⟨e, he, k', hi, -, -⟩
    · rfl
    · exact (hroot (hi ▸ mem_ids he)).elim
  | @step e k he _ ih =>
    rcases h₂.inv with ⟨hi, -⟩ | ⟨e', he', k', hi, rfl, h'⟩
    · exact (hroot (hi ▸ mem_ids he)).elim
    · have hee : e = e' := List.inj_on_of_nodup_map hnd he he' hi
      subst hee
      exact congrArg (· + 1) (ih h')

/-- Every depth up to a node's own is taken by some node on its path. -/
theorem ReachesIn.prefix {T : Tree Id σ} {i : Id} {k : ℕ} (h : T.ReachesIn i k) :
    ∀ j ≤ k, ∃ i', T.ReachesIn i' j ∧ (0 < j → i' ∈ T.ids) := by
  induction h with
  | root =>
    intro j hj
    have hj0 : j = 0 := by omega
    subst hj0
    exact ⟨T.root, .root, fun h => absurd h (lt_irrefl 0)⟩
  | @step e k he h ih =>
    intro j hj
    rcases Nat.lt_or_ge j (k + 1) with hlt | hge
    · exact ih j (by omega)
    · have hjk : j = k + 1 := by omega
      subst hjk
      exact ⟨e.id, .step he h, fun _ => mem_ids he⟩

/-- No path is longer than the tree: depths are bounded by the node count. -/
theorem ReachesIn.le_length [DecidableEq Id] {T : Tree Id σ} (hnd : T.ids.Nodup) (hroot : T.root ∉ T.ids)
    {i : Id} {k : ℕ} (h : T.ReachesIn i k) : k ≤ T.nodes.length := by
  have hp : ∀ j, ∃ i', j ≤ k → T.ReachesIn i' j ∧ (0 < j → i' ∈ T.ids) := by
    intro j
    by_cases hj : j ≤ k
    · obtain ⟨i', hi'⟩ := h.prefix j hj
      exact ⟨i', fun _ => hi'⟩
    · exact ⟨T.root, fun h' => absurd h' hj⟩
  choose f hf using hp
  have hmaps : ∀ a ∈ Finset.Icc 1 k, f a ∈ T.ids.toFinset := by
    intro a ha
    rw [Finset.mem_Icc] at ha
    exact List.mem_toFinset.mpr ((hf a ha.2).2 (by omega))
  have hinj : Set.InjOn f (Finset.Icc 1 k : Set ℕ) := by
    intro a ha b hb hab
    rw [Finset.coe_Icc, Set.mem_Icc] at ha hb
    have h2 : T.ReachesIn (f a) b := by
      rw [hab]
      exact (hf b hb.2).1
    exact ReachesIn.unique hnd hroot (hf a ha.2).1 h2
  have hcard := Finset.card_le_card_of_injOn f (by intro a ha; exact hmaps a ha) hinj
  rw [Nat.card_Icc, List.toFinset_card_of_nodup hnd] at hcard
  simp only [ids, List.length_map] at hcard
  omega

/-- The depth-bounded fold: with `f` levels, some exception overriding `v`
    has a holding condition and is not itself lifted. -/
def lifts [DecidableEq Id] (T : Tree Id σ) (x : σ) : ℕ → Id → Bool
  | 0, _ => false
  | f + 1, v => T.nodes.any fun c => decide (c.overrides = v) && c.condition x && !lifts T x f c.id

theorem lifts_succ_iff [DecidableEq Id] (T : Tree Id σ) (x : σ) (f : ℕ) (v : Id) :
    T.lifts x (f + 1) v = true ↔
      ∃ c ∈ T.nodes, c.overrides = v ∧ c.condition x = true ∧ T.lifts x f c.id = false := by
  simp [lifts, List.any_eq_true, Bool.and_eq_true, and_assoc]

/-- Once the fuel covers the remaining depth, more fuel changes nothing. -/
theorem lifts_stable [DecidableEq Id] {T : Tree Id σ} (wf : T.WellFormed) (x : σ) :
    ∀ j v d, T.ReachesIn v d → T.nodes.length ≤ d + j →
      ∀ f, j ≤ f → T.lifts x f v = T.lifts x j v := by
  intro j
  induction j with
  | zero =>
    intro v d hv hle f _
    cases f with
    | zero => rfl
    | succ f' =>
      rw [Bool.eq_iff_iff, lifts_succ_iff]
      simp only [lifts, Bool.false_eq_true, iff_false]
      rintro ⟨c, hc, hov, -, -⟩
      have := (ReachesIn.child hc hov hv).le_length wf.nodup wf.rootFresh
      omega
  | succ j ih =>
    intro v d hv hle f hf
    obtain ⟨f', rfl⟩ : ∃ f', f = f' + 1 := ⟨f - 1, by omega⟩
    rw [Bool.eq_iff_iff, lifts_succ_iff, lifts_succ_iff]
    refine exists_congr fun c => and_congr_right fun hc => and_congr_right fun hov =>
      and_congr_right fun _ => ?_
    rw [ih c.id (d + 1) (ReachesIn.child hc hov hv) (by omega) f' (by omega)]

/-- The defeasible reading: an exception applies exactly when its condition
    holds and no exception overriding it applies. -/
def Stable (T : Tree Id σ) (x : σ) (A : Id → Prop) : Prop :=
  ∀ e ∈ T.nodes, (A e.id ↔ e.condition x = true ∧ ∀ c ∈ T.nodes, c.overrides = e.id → ¬ A c.id)

/-- The law is lifted under labelling `A`: some exception overriding it applies. -/
def LiftedBy (T : Tree Id σ) (A : Id → Prop) : Prop :=
  ∃ c ∈ T.nodes, c.overrides = T.root ∧ A c.id

/-- A well-formed tree has at most one stable labelling of its exceptions. -/
theorem stable_unique [DecidableEq Id] {T : Tree Id σ} (wf : T.WellFormed) {x : σ} {A B : Id → Prop}
    (hA : T.Stable x A) (hB : T.Stable x B) : ∀ e ∈ T.nodes, (A e.id ↔ B e.id) := by
  have key : ∀ j, ∀ e ∈ T.nodes, ∀ d, T.ReachesIn e.id d → T.nodes.length ≤ d + j →
      (A e.id ↔ B e.id) := by
    intro j
    induction j with
    | zero =>
      intro e he d hd hle
      have noChild : ∀ c ∈ T.nodes, c.overrides = e.id → False := fun c hc hov => by
        have := (ReachesIn.child hc hov hd).le_length wf.nodup wf.rootFresh
        omega
      rw [hA e he, hB e he]
      exact and_congr_right fun _ =>
        ⟨fun _ c hc hov => (noChild c hc hov).elim, fun _ c hc hov => (noChild c hc hov).elim⟩
    | succ j ih =>
      intro e he d hd hle
      rw [hA e he, hB e he]
      refine and_congr_right fun _ => ⟨fun h c hc hov hBc => ?_, fun h c hc hov hAc => ?_⟩
      · exact h c hc hov ((ih c hc (d + 1) (ReachesIn.child hc hov hd) (by omega)).mpr hBc)
      · exact h c hc hov ((ih c hc (d + 1) (ReachesIn.child hc hov hd) (by omega)).mp hAc)
  intro e he
  obtain ⟨d, hd⟩ := wf.reaches e he
  exact key T.nodes.length e he d hd (by omega)

/-- The labelling the fold computes. -/
def star [DecidableEq Id] (T : Tree Id σ) (x : σ) (i : Id) : Prop :=
  ∃ e ∈ T.nodes, e.id = i ∧ e.condition x = true ∧ T.lifts x T.nodes.length i = false

theorem star_iff [DecidableEq Id] {T : Tree Id σ} (wf : T.WellFormed) (x : σ) {e : Exception Id σ}
    (he : e ∈ T.nodes) :
    T.star x e.id ↔ e.condition x = true ∧ T.lifts x T.nodes.length e.id = false := by
  constructor
  · rintro ⟨e', he', hid, hc, hl⟩
    have hee : e' = e := List.inj_on_of_nodup_map wf.nodup he' he hid
    subst hee
    exact ⟨hc, hl⟩
  · rintro ⟨hc, hl⟩
    exact ⟨e, he, rfl, hc, hl⟩

/-- The fold's labelling is stable. -/
theorem stable_star [DecidableEq Id] {T : Tree Id σ} (wf : T.WellFormed) (x : σ) : T.Stable x (T.star x) := by
  intro e he
  obtain ⟨d, hd⟩ := wf.reaches e he
  obtain ⟨m, hm⟩ : ∃ m, T.nodes.length = m + 1 :=
    ⟨T.nodes.length - 1, by have := List.length_pos_of_mem he; omega⟩
  rw [star_iff wf x he]
  refine and_congr_right fun _ => ?_
  rw [hm, ← Bool.not_eq_true, lifts_succ_iff]
  constructor
  · rintro hno c hc hov hstar
    rw [star_iff wf x hc, hm] at hstar
    apply hno
    refine ⟨c, hc, hov, hstar.1, ?_⟩
    rw [← hstar.2]
    have hcd := ReachesIn.child hc hov hd
    exact (lifts_stable wf x m c.id (d + 1) hcd (by omega) (m + 1) (by omega)).symm
  · rintro hall ⟨c, hc, hov, hcond, hl⟩
    apply hall c hc hov
    rw [star_iff wf x hc, hm]
    refine ⟨hcond, ?_⟩
    rw [← hl]
    have hcd := ReachesIn.child hc hov hd
    exact lifts_stable wf x m c.id (d + 1) hcd (by omega) (m + 1) (by omega)

/-- A well-formed tree has a stable labelling at every point. -/
theorem stable_exists [DecidableEq Id] {T : Tree Id σ} (wf : T.WellFormed) (x : σ) : ∃ A, T.Stable x A :=
  ⟨T.star x, stable_star wf x⟩

/-- Compiled guard = defeasible semantics: under the unique stable labelling,
    the law is lifted exactly when the fold with fuel `|nodes| + 1` says so. -/
theorem lifted_iff [DecidableEq Id] {T : Tree Id σ} (wf : T.WellFormed) {x : σ} {A : Id → Prop}
    (hA : T.Stable x A) : T.LiftedBy A ↔ T.lifts x (T.nodes.length + 1) T.root = true := by
  rw [lifts_succ_iff]
  have hagree := stable_unique wf hA (stable_star wf x)
  refine exists_congr fun c => and_congr_right fun hc => and_congr_right fun _ => ?_
  rw [hagree c hc, star_iff wf x hc]

/-- Lifting needs a first-level exception whose condition holds. -/
theorem lifts_needs_top_condition [DecidableEq Id] (T : Tree Id σ) (x : σ) (f : ℕ)
    (h : T.lifts x f T.root = true) :
    ∃ c ∈ T.nodes, c.overrides = T.root ∧ c.condition x = true := by
  cases f with
  | zero => simp [lifts] at h
  | succ f =>
    obtain ⟨c, hc, hov, hcond, -⟩ := (lifts_succ_iff T x f T.root).mp h
    exact ⟨c, hc, hov, hcond⟩

/-- An empty tree never lifts. -/
theorem lifts_nil [DecidableEq Id] (root : Id) (x : σ) (f : ℕ) : (Tree.mk root ([] : List (Exception Id σ))).lifts x f root = false := by
  cases f <;> simp [lifts]

/-- Every exception strictly above `i` on its path to the law has a holding
    condition at `x`. A decision reports an exception as applied when it
    applies (under the stable labelling) and its ancestors all hold, so an
    exception below one whose condition fails is never reported. -/
inductive AboveHolds (T : Tree Id σ) (x : σ) : Id → Prop
  | top {e : Exception Id σ} : e ∈ T.nodes → e.overrides = T.root → AboveHolds T x e.id
  | up {e c : Exception Id σ} : e ∈ T.nodes → c ∈ T.nodes → e.overrides = c.id →
      c.condition x = true → AboveHolds T x c.id → AboveHolds T x e.id

theorem AboveHolds.inv {T : Tree Id σ} {x : σ} {i : Id} (h : T.AboveHolds x i) :
    (∃ e ∈ T.nodes, e.id = i ∧ e.overrides = T.root) ∨
      ∃ e ∈ T.nodes, ∃ c ∈ T.nodes, e.id = i ∧ e.overrides = c.id ∧
        c.condition x = true ∧ T.AboveHolds x c.id := by
  cases h with
  | top he hr => exact Or.inl ⟨_, he, rfl, hr⟩
  | up he hc hov hcond h => exact Or.inr ⟨_, he, _, hc, rfl, hov, hcond, h⟩

/-- The exceptions a decision reports as applied under labelling `A`. -/
def Reported (T : Tree Id σ) (x : σ) (A : Id → Prop) (i : Id) : Prop :=
  A i ∧ T.AboveHolds x i

/-- `law ← a`. -/
def nestA : Exception ℕ Unit := ⟨1, 0, fun _ => true⟩
/-- `a ← b`. -/
def nestB : Exception ℕ Unit := ⟨2, 1, fun _ => true⟩
/-- `b ← c`. -/
def nestC : Exception ℕ Unit := ⟨3, 2, fun _ => true⟩

/-- Nesting does not narrow monotonically. Adding an exception under an
    exception can lift the law where the smaller tree did not: under
    `law ← a ← b`, `b` applies and stops `a`, but a new `c` under `b` stops
    `b`, so `a` applies again and lifts the law. Exceptions only narrow
    relative to the bare law (`Law.binds_bare`), not relative to a subtree. -/
theorem nested_exception_can_lift :
    ∃ T T' : Tree ℕ Unit, T.WellFormed ∧ T'.WellFormed ∧ T'.root = T.root ∧
      (∀ e ∈ T.nodes, e ∈ T'.nodes) ∧
      T.lifts () (T.nodes.length + 1) T.root = false ∧
      T'.lifts () (T'.nodes.length + 1) T'.root = true := by
  have ra : ∀ ns : List (Exception ℕ Unit), nestA ∈ ns → (Tree.mk 0 ns).ReachesIn 1 1 :=
    fun _ ha => .step (e := nestA) ha .root
  have rb : ∀ ns : List (Exception ℕ Unit), nestA ∈ ns → nestB ∈ ns →
      (Tree.mk 0 ns).ReachesIn 2 2 :=
    fun ns ha hb => .step (e := nestB) hb (ra ns ha)
  have rc : ∀ ns : List (Exception ℕ Unit), nestA ∈ ns → nestB ∈ ns → nestC ∈ ns →
      (Tree.mk 0 ns).ReachesIn 3 3 :=
    fun ns ha hb hc => .step (e := nestC) hc (rb ns ha hb)
  refine ⟨⟨0, [nestA, nestB]⟩, ⟨0, [nestA, nestB, nestC]⟩, ⟨by decide, by decide, by decide, ?_⟩,
    ⟨by decide, by decide, by decide, ?_⟩, rfl, ?_, by decide, by decide⟩
  · intro e he
    simp only [List.mem_cons, List.not_mem_nil, or_false] at he
    rcases he with rfl | rfl
    · exact ⟨1, ra _ (by simp)⟩
    · exact ⟨2, rb _ (by simp) (by simp)⟩
  · intro e he
    simp only [List.mem_cons, List.not_mem_nil, or_false] at he
    rcases he with rfl | rfl | rfl
    · exact ⟨1, ra _ (by simp)⟩
    · exact ⟨2, rb _ (by simp) (by simp)⟩
    · exact ⟨3, rc _ (by simp) (by simp) (by simp)⟩
  · intro e he
    simp only [List.mem_cons, List.not_mem_nil, or_false] at he ⊢
    rcases he with rfl | rfl
    · exact Or.inl rfl
    · exact Or.inr (Or.inl rfl)

end Tree

/-- The admitted temporal wrapper (`LawModality`). -/
inductive Modality (σ : Type) where
  | always
  | never
  | eventually
  | untilStop (stop : σ → Bool)

end Law

/-- The semantic core of a `LawIR` with a predicate response. Scope is the
    conjunction of its filters, denoted as one predicate. -/
structure Law (Id σ : Type) where
  id : Id
  scope : σ → Bool
  condition : Option (σ → Bool)
  modality : Law.Modality σ
  response : σ → Bool
  exceptions : List (Law.Exception Id σ)

namespace Law

variable {Id σ : Type} [DecidableEq Id]

/-- The law's exception tree, rooted at its own id. -/
def tree (L : Law Id σ) : Tree Id σ := ⟨L.id, L.exceptions⟩

/-- Active before exceptions: in scope, and the condition (if any) holds. -/
def active (L : Law Id σ) (x : σ) : Bool := L.scope x && (L.condition.map (· x)).getD true

/-- The law binds at `x`: active and not lifted by its exception tree. -/
def binds (L : Law Id σ) (x : σ) : Bool :=
  L.active x && !L.tree.lifts x (L.exceptions.length + 1) L.id

/-- The law holds on a trace: at each point where it binds, its modality's
    requirement is met (strong until for `untilStop`). -/
def holdsOn (L : Law Id σ) (tr : ℕ → σ) : Prop :=
  ∀ i, L.binds (tr i) = true →
    match L.modality with
    | .always => L.response (tr i) = true
    | .never => L.response (tr i) = false
    | .eventually => ∃ j, i ≤ j ∧ L.response (tr j) = true
    | .untilStop stop =>
        ∃ j, i ≤ j ∧ stop (tr j) = true ∧ ∀ k, i ≤ k → k < j → L.response (tr k) = true

/-- A state satisfies a law when the constant trace does: for `always` and
    `never`, the pointwise check on a final state. -/
def holdsAt (L : Law Id σ) (x : σ) : Prop := L.holdsOn fun _ => x

theorem holdsAt_always {L : Law Id σ} (h : L.modality = .always) (x : σ) :
    L.holdsAt x ↔ (L.binds x = true → L.response x = true) := by
  simp [holdsAt, holdsOn, h]

theorem holdsAt_never {L : Law Id σ} (h : L.modality = .never) (x : σ) :
    L.holdsAt x ↔ (L.binds x = true → L.response x = false) := by
  simp [holdsAt, holdsOn, h]

omit [DecidableEq Id] in
/-- The same law with no exceptions. -/
def bare (L : Law Id σ) : Law Id σ := { L with exceptions := [] }

/-- Never binds beyond the bare law: a law binds only where its bare law binds. -/
theorem binds_bare (L : Law Id σ) (x : σ) (h : L.binds x = true) : L.bare.binds x = true := by
  have hnil : (Tree.mk L.id ([] : List (Exception Id σ))).lifts x 1 L.id = false :=
    Tree.lifts_nil _ _ _
  unfold binds at h ⊢
  rw [Bool.and_eq_true] at h ⊢
  exact ⟨h.1, by simpa [bare, tree] using hnil⟩

/-- Never binds beyond the bare law: an exception tree never adds a requirement, so a
    trace satisfying the bare law satisfies the law. -/
theorem holdsOn_of_bare (L : Law Id σ) (tr : ℕ → σ) (h : L.bare.holdsOn tr) : L.holdsOn tr :=
  fun i hb => h i (binds_bare L (tr i) hb)

end Law

end Norn.Sem
