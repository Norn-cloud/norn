import Std

/-!
Reference specification proofs for the `data` constitutional ontology pack
(`registry/packs/data/data.ts`).

Evidence class: **checked proof** of a standalone Lean 4 reference model (zero
`sorry`, zero warnings under `lean -DwarningAsError=true`, using only Lean 4
core / `Std`). This file does **not** prove declaration lowering, Rust
implementation correspondence, commit-path enforcement, or runtime non-vacuity.

Honest correspondence between each theorem group and the current `data.*` IR:

1. `DATA-CAUSAL-1` (Transitive No-Future-Leakage):
   - **Enforced by `data.Derivation`:** `data_causal_1_monotonic_transitive`
     directly corresponds to `data.Derivation.monotonic_knowledge_time`
     (`input_snapshot.known_at ≤ output_snapshot.known_at`). Along any finite
     derivation path `s₀ →* sₙ`, `s₀.knownAt ≤ sₙ.knownAt`.
   - **Stronger sufficient condition (not enforced by the base pack):**
     `data_causal_1_transitive_no_future_leakage` shows that activity-mediated
     ordering (`input.knownAt ≤ activity.endsAt ∧ activity.endsAt ≤ output.knownAt`)
     also implies `monotonicKnowledge`. The base pack currently enforces
     `activity.startsAt ≤ output.knownAt` plus `monotonic_knowledge_time`, and
     `window_overlap_alone_allows_retrograde_output` witnesses why the activity
     window laws alone are insufficient without `monotonic_knowledge_time`.

2. `DATA-ACYCLIC-1` (Conditional: Strict Temporal Progression Implies Acyclicity):
   - **Conditional theorem (stronger than the base pack):**
     `data_acyclic_1_strict_progression` proves acyclicity (`∀ s, ¬ NonEmptyPath G s s`)
     when a composing pack strengthens `monotonic_knowledge_time` to strict
     progression (`input.knownAt < output.knownAt`), and
     `data_acyclic_1_from_proper_activity` proves it when a pack requires
     `input.knownAt ≤ activity.startsAt ∧ activity.endsAt ≤ output.knownAt`
     over proper intervals (`core.TimeInterval.proper_interval`).
   - **Explicit gap witness:** `monotonic_alone_allows_two_snapshot_cycle`
     proves that the base pack's four `data.Derivation` laws alone allow a
     two-snapshot cycle `A → B → A` when `A.knownAt = B.knownAt` and `A ≠ B`.
     (The pack's `acyclic: true` link flag on `data.SnapshotRecord.supersedes`
     governs snapshot revision chains, not derivation edges.)

3. `TIME-ALLEN-1` (W3C OWL-Time Allen's 13 Interval Relations Partition Theorem):
   Over proper half-open intervals `[startsAt, endsAt)` with `startsAt < endsAt`
   (matching `core.TimeInterval.proper_interval` inherited by `data.Activity`),
   proves that Allen's 13 relations are mutually exclusive and jointly
   exhaustive. Mirrors the partition slice of
   `verification/owl-time-v1/AllenIntervals.lean` (checked by `owl-time-allen-lean`,
   which also proves the classifier, converse dualities, scheduling compositions,
   and pairs with `owl-time-allen-verus` over `allen_core.rs`).

4. `BFO-DISJOINT-1` (`GRAPH-ROLE-1`) & Target `DECLARED-DISJOINT`:
   - **Derived from `GRAPH-ROLE-1` (enforced by `norn-checker` today):**
     `bfo_role_partition` and `bfo_continuant_occurrent_disjoint` formalize the
     `ConceptRole` (`thing` vs `happening`) partition and directly prove
     `Observation ⊓ Claim = ∅` and `DatasetSnapshot ⊓ Activity = ∅`. (Note:
     `data.QualityFinding` is modeled as a `happening` assessment/resolution act
     in Norn rather than a pure BFO information continuant.)
   - **Target `disjointWith` contract (pending PR #1781, not yet enforced by
     `norn-checker`):** `declared_disjoint_observation_economic_event` and
     `declared_disjoint_observation_commitment` project the assumed
     `declaredDisjoint` hypothesis (`Observation ⊓ EconomicEvent = ∅`,
     `Observation ⊓ Commitment = ∅`), since both sides have role `happening`.
-/
namespace Norn.Verification.DataLineage

/-! ## 1. DATA-CAUSAL-1: Transitive No-Future-Leakage -/

/-- A content-addressed dataset snapshot with an integer UTC knowledge instant
(matching `data.DatasetSnapshot.known_at` embedded in `Int` via nanosecond
ordering keys). -/
structure Snapshot where
  id : Nat
  knownAt : Int
  deriving DecidableEq, Repr

/-- A proper half-open UTC activity interval `[startsAt, endsAt)` matching
`data.Activity` extending `core.TimeInterval` (`proper_interval`). -/
structure Activity where
  startsAt : Int
  endsAt : Int
  proper : startsAt < endsAt

/-- A qualified derivation edge matching `data.Derivation`. -/
structure Derivation where
  input : Snapshot
  output : Snapshot
  activity : Activity

/-- Monotonic knowledge-time edge law (`data.Derivation.monotonic_knowledge_time`):
an output snapshot cannot be known strictly before its input snapshot. -/
def monotonicKnowledge (d : Derivation) : Prop :=
  d.input.knownAt ≤ d.output.knownAt

/-- Stronger activity-mediated causal ordering (not enforced by the base pack,
which enforces `activity.startsAt ≤ output.knownAt` plus `monotonicKnowledge`):
input is known by activity end and output is known at or after activity end. -/
def causallyOrdered (d : Derivation) : Prop :=
  d.input.knownAt ≤ d.activity.endsAt ∧ d.activity.endsAt ≤ d.output.knownAt

/-- Stronger strict activity-window causal ordering (conditional hypothesis for
`DATA-ACYCLIC-1`): input is known by activity start and output is known at or
after activity end (strictly advancing via `proper`). -/
def strictlyCausallyOrdered (d : Derivation) : Prop :=
  d.input.knownAt ≤ d.activity.startsAt ∧ d.activity.endsAt ≤ d.output.knownAt

/-- Any activity-mediated causally ordered edge is knowledge-time monotonic. -/
theorem causallyOrdered_implies_monotonic (d : Derivation) (h : causallyOrdered d) :
    monotonicKnowledge d := by
  have h1 := h.1
  have h2 := h.2
  simp only [monotonicKnowledge]
  omega

/-- Any strictly causally ordered edge strictly advances knowledge time, using
`core.TimeInterval.proper_interval` (`d.activity.proper`). -/
theorem strictlyCausallyOrdered_implies_strict_knownAt (d : Derivation)
    (h : strictlyCausallyOrdered d) : d.input.knownAt < d.output.knownAt := by
  have h1 := h.1
  have h2 := h.2
  have hp := d.activity.proper
  omega

/-- Reflexive-transitive closure of derivation edges admitted by graph predicate `G`. -/
inductive DerivationPath (G : Derivation → Prop) : Snapshot → Snapshot → Prop
  | refl (s : Snapshot) : DerivationPath G s s
  | step {s₀ s₁ s₂ : Snapshot} (d : Derivation)
      (hEdge : G d)
      (hIn : d.input = s₀)
      (hOut : d.output = s₁)
      (hRest : DerivationPath G s₁ s₂) :
      DerivationPath G s₀ s₂

/-- DATA-CAUSAL-1 (base pack law `data.Derivation.monotonic_knowledge_time`):
if every derivation edge in `G` satisfies `monotonicKnowledge`, then knowledge
time is non-decreasing along any finite derivation path `s₀ →* sₙ`. -/
theorem data_causal_1_monotonic_transitive
    (G : Derivation → Prop)
    (hMonotonic : ∀ d, G d → monotonicKnowledge d)
    {s₀ sₙ : Snapshot}
    (path : DerivationPath G s₀ sₙ) :
    s₀.knownAt ≤ sₙ.knownAt := by
  induction path with
  | refl s => exact Int.le_refl s.knownAt
  | step d hEdge hIn hOut _ ih =>
      have hm : d.input.knownAt ≤ d.output.knownAt := hMonotonic d hEdge
      rw [hIn, hOut] at hm
      exact Int.le_trans hm ih

/-- No-future-leakage contrapositive under `monotonicKnowledge`: a snapshot known
strictly later cannot derive an earlier snapshot along any path. -/
theorem data_causal_1_no_backward_leakage_monotonic
    (G : Derivation → Prop)
    (hMonotonic : ∀ d, G d → monotonicKnowledge d)
    {s₀ sₙ : Snapshot}
    (hFuture : sₙ.knownAt < s₀.knownAt) :
    ¬ DerivationPath G s₀ sₙ := by
  intro path
  have hLe := data_causal_1_monotonic_transitive G hMonotonic path
  omega

/-- DATA-CAUSAL-1 (stronger activity-mediated sufficient condition): if every
derivation edge in `G` satisfies `causallyOrdered` (`input.knownAt ≤ activity.endsAt`
and `activity.endsAt ≤ output.knownAt`), then along any transitive derivation
path `s₀ →* sₙ`, `s₀.knownAt ≤ sₙ.knownAt`. -/
theorem data_causal_1_transitive_no_future_leakage
    (G : Derivation → Prop)
    (hCausal : ∀ d, G d → causallyOrdered d)
    {s₀ sₙ : Snapshot}
    (path : DerivationPath G s₀ sₙ) :
    s₀.knownAt ≤ sₙ.knownAt :=
  data_causal_1_monotonic_transitive G
    (fun d hd => causallyOrdered_implies_monotonic d (hCausal d hd))
    path

/-- No-future-leakage contrapositive under `causallyOrdered`. -/
theorem data_causal_1_no_backward_leakage
    (G : Derivation → Prop)
    (hCausal : ∀ d, G d → causallyOrdered d)
    {s₀ sₙ : Snapshot}
    (hFuture : sₙ.knownAt < s₀.knownAt) :
    ¬ DerivationPath G s₀ sₙ :=
  data_causal_1_no_backward_leakage_monotonic G
    (fun d hd => causallyOrdered_implies_monotonic d (hCausal d hd))
    hFuture

/-- Witness showing why `input.knownAt ≤ activity.endsAt` and
`activity.startsAt ≤ output.knownAt` alone do not prevent `output.knownAt < input.knownAt`
without `monotonic_knowledge_time` (or `activity.endsAt ≤ output.knownAt`). -/
theorem window_overlap_alone_allows_retrograde_output :
    ∃ d : Derivation,
      d.input.knownAt ≤ d.activity.endsAt ∧
      d.activity.startsAt ≤ d.output.knownAt ∧
      d.output.knownAt < d.input.knownAt :=
  ⟨{ input := ⟨1, 10⟩, output := ⟨2, 5⟩, activity := ⟨0, 20, by decide⟩ },
    by decide, by decide, by decide⟩

/-! ## 2. DATA-ACYCLIC-1 (Conditional): Strict Temporal Progression Implies Acyclicity -/

/-- Non-empty derivation path (one or more derivation edges in `G`). -/
inductive NonEmptyPath (G : Derivation → Prop) : Snapshot → Snapshot → Prop
  | single {s₀ s₁ : Snapshot} (d : Derivation)
      (hEdge : G d)
      (hIn : d.input = s₀)
      (hOut : d.output = s₁) :
      NonEmptyPath G s₀ s₁
  | cons {s₀ s₁ s₂ : Snapshot} (d : Derivation)
      (hEdge : G d)
      (hIn : d.input = s₀)
      (hOut : d.output = s₁)
      (hRest : NonEmptyPath G s₁ s₂) :
      NonEmptyPath G s₀ s₂

/-- Any non-empty path whose edges strictly advance `knownAt` strictly advances
`knownAt` from start to finish. -/
theorem nonEmptyPath_strict_knownAt
    (G : Derivation → Prop)
    (hStrict : ∀ d, G d → d.input.knownAt < d.output.knownAt)
    {s₀ sₙ : Snapshot}
    (path : NonEmptyPath G s₀ sₙ) :
    s₀.knownAt < sₙ.knownAt := by
  induction path with
  | single d hEdge hIn hOut =>
      have hs := hStrict d hEdge
      rw [hIn, hOut] at hs
      exact hs
  | cons d hEdge hIn hOut _ ih =>
      have hs := hStrict d hEdge
      rw [hIn, hOut] at hs
      omega

/-- DATA-ACYCLIC-1 (conditional on strict progression): if a pack strengthens
`monotonic_knowledge_time` so every derivation edge in `G` satisfies
`input.knownAt < output.knownAt`, then the derivation graph has no non-empty
cycles (`¬ NonEmptyPath G s s`). -/
theorem data_acyclic_1_strict_progression
    (G : Derivation → Prop)
    (hStrict : ∀ d, G d → d.input.knownAt < d.output.knownAt)
    (s : Snapshot) :
    ¬ NonEmptyPath G s s := by
  intro cycle
  have hLt := nonEmptyPath_strict_knownAt G hStrict cycle
  omega

/-- DATA-ACYCLIC-1 (conditional on strict activity window): if a pack requires
`input.knownAt ≤ activity.startsAt` and `activity.endsAt ≤ output.knownAt` over
proper intervals (`activity.startsAt < activity.endsAt`), then the derivation
graph is provably acyclic. -/
theorem data_acyclic_1_from_proper_activity
    (G : Derivation → Prop)
    (hWindow : ∀ d, G d → strictlyCausallyOrdered d)
    (s : Snapshot) :
    ¬ NonEmptyPath G s s :=
  data_acyclic_1_strict_progression G
    (fun d hd => strictlyCausallyOrdered_implies_strict_knownAt d (hWindow d hd))
    s

/-- Base-pack edge predicate corresponding to all four laws on `data.Derivation`:
`input_known_before_activity_end`, `output_known_after_activity_start`,
`monotonic_knowledge_time`, and `distinct_snapshots`. -/
def baseDerivationLaws (d : Derivation) : Prop :=
  d.input.knownAt ≤ d.activity.endsAt ∧
  d.activity.startsAt ≤ d.output.knownAt ∧
  monotonicKnowledge d ∧
  d.input.id ≠ d.output.id

/-- Witness showing why `DATA-ACYCLIC-1` is conditional: the base pack's four
`data.Derivation` laws allow a 2-cycle `A → B → A` when two distinct snapshots
share the same `knownAt`. -/
theorem monotonic_alone_allows_two_snapshot_cycle :
    ∃ s : Snapshot, NonEmptyPath baseDerivationLaws s s := by
  let a : Snapshot := ⟨1, 10⟩
  let b : Snapshot := ⟨2, 10⟩
  let act : Activity := ⟨0, 20, by decide⟩
  let d1 : Derivation := { input := a, output := b, activity := act }
  let d2 : Derivation := { input := b, output := a, activity := act }
  have h1 : baseDerivationLaws d1 :=
    ⟨by decide, by decide, by simp only [monotonicKnowledge]; decide, by decide⟩
  have h2 : baseDerivationLaws d2 :=
    ⟨by decide, by decide, by simp only [monotonicKnowledge]; decide, by decide⟩
  exact ⟨a, NonEmptyPath.cons d1 h1 rfl rfl (NonEmptyPath.single d2 h2 rfl rfl)⟩

/-! ## 3. TIME-ALLEN-1: W3C OWL-Time Allen's 13 Interval Relations Partition Theorem -/

/-- A proper half-open UTC interval `[startsAt, endsAt)` matching
`core.TimeInterval.proper_interval`. -/
structure ProperInterval where
  startsAt : Int
  endsAt : Int
  proper : startsAt < endsAt

def allenBefore (x y : ProperInterval) : Prop := x.endsAt < y.startsAt
def allenAfter (x y : ProperInterval) : Prop := y.endsAt < x.startsAt
def allenMeets (x y : ProperInterval) : Prop := x.endsAt = y.startsAt
def allenMetBy (x y : ProperInterval) : Prop := y.endsAt = x.startsAt
def allenOverlaps (x y : ProperInterval) : Prop :=
  x.startsAt < y.startsAt ∧ y.startsAt < x.endsAt ∧ x.endsAt < y.endsAt
def allenOverlappedBy (x y : ProperInterval) : Prop :=
  y.startsAt < x.startsAt ∧ x.startsAt < y.endsAt ∧ y.endsAt < x.endsAt
def allenStarts (x y : ProperInterval) : Prop := x.startsAt = y.startsAt ∧ x.endsAt < y.endsAt
def allenStartedBy (x y : ProperInterval) : Prop := x.startsAt = y.startsAt ∧ y.endsAt < x.endsAt
def allenDuring (x y : ProperInterval) : Prop := y.startsAt < x.startsAt ∧ x.endsAt < y.endsAt
def allenContains (x y : ProperInterval) : Prop := x.startsAt < y.startsAt ∧ y.endsAt < x.endsAt
def allenFinishes (x y : ProperInterval) : Prop := y.startsAt < x.startsAt ∧ x.endsAt = y.endsAt
def allenFinishedBy (x y : ProperInterval) : Prop := x.startsAt < y.startsAt ∧ x.endsAt = y.endsAt
def allenEquals (x y : ProperInterval) : Prop := x.startsAt = y.startsAt ∧ x.endsAt = y.endsAt

/-- W3C OWL-Time / Allen's 13 mutually exclusive interval relations. -/
inductive AllenRelation where
  | before | after | meets | metBy | overlaps | overlappedBy
  | starts | startedBy | during | contains | finishes | finishedBy | equals
  deriving DecidableEq, Repr

/-- Semantics of each Allen relation over proper half-open intervals. -/
def allenHolds (r : AllenRelation) (x y : ProperInterval) : Prop :=
  match r with
  | .before => allenBefore x y
  | .after => allenAfter x y
  | .meets => allenMeets x y
  | .metBy => allenMetBy x y
  | .overlaps => allenOverlaps x y
  | .overlappedBy => allenOverlappedBy x y
  | .starts => allenStarts x y
  | .startedBy => allenStartedBy x y
  | .during => allenDuring x y
  | .contains => allenContains x y
  | .finishes => allenFinishes x y
  | .finishedBy => allenFinishedBy x y
  | .equals => allenEquals x y

/-- At least one of Allen's 13 relations holds for every pair of proper intervals. -/
theorem time_allen_1_thirteen_way (x y : ProperInterval) :
    allenBefore x y ∨ allenAfter x y ∨ allenMeets x y ∨ allenMetBy x y ∨
      allenOverlaps x y ∨ allenOverlappedBy x y ∨ allenStarts x y ∨ allenStartedBy x y ∨
      allenDuring x y ∨ allenContains x y ∨ allenFinishes x y ∨ allenFinishedBy x y ∨
      allenEquals x y := by
  have hx := x.proper
  have hy := y.proper
  simp only [allenBefore, allenAfter, allenMeets, allenMetBy, allenOverlaps, allenOverlappedBy,
    allenStarts, allenStartedBy, allenDuring, allenContains, allenFinishes, allenFinishedBy,
    allenEquals]
  omega

/-- TIME-ALLEN-1 (jointly exhaustive): some Allen relation holds for every pair
of proper intervals. -/
theorem time_allen_1_exhaustive (x y : ProperInterval) : ∃ r : AllenRelation, allenHolds r x y := by
  rcases time_allen_1_thirteen_way x y with h | h | h | h | h | h | h | h | h | h | h | h | h
  · exact ⟨.before, h⟩
  · exact ⟨.after, h⟩
  · exact ⟨.meets, h⟩
  · exact ⟨.metBy, h⟩
  · exact ⟨.overlaps, h⟩
  · exact ⟨.overlappedBy, h⟩
  · exact ⟨.starts, h⟩
  · exact ⟨.startedBy, h⟩
  · exact ⟨.during, h⟩
  · exact ⟨.contains, h⟩
  · exact ⟨.finishes, h⟩
  · exact ⟨.finishedBy, h⟩
  · exact ⟨.equals, h⟩

/-- TIME-ALLEN-1 (mutually exclusive): if two Allen relations both hold for the
same pair of proper intervals, they are the same relation. -/
theorem time_allen_1_pairwise_disjoint (r s : AllenRelation) (x y : ProperInterval)
    (hr : allenHolds r x y) (hs : allenHolds s x y) : r = s := by
  have hx := x.proper
  have hy := y.proper
  cases r <;> cases s <;>
    simp only [allenHolds, allenBefore, allenAfter, allenMeets, allenMetBy, allenOverlaps,
      allenOverlappedBy, allenStarts, allenStartedBy, allenDuring, allenContains, allenFinishes,
      allenFinishedBy, allenEquals] at hr hs <;>
    first
      | rfl
      | omega

/-- TIME-ALLEN-1 (Partition Theorem): every pair of proper half-open intervals
satisfies *exactly one* of Allen's 13 relations. -/
theorem time_allen_1_partition (x y : ProperInterval) :
    ∃ r : AllenRelation, allenHolds r x y ∧ ∀ s : AllenRelation, allenHolds s x y → s = r := by
  rcases time_allen_1_exhaustive x y with ⟨r, hr⟩
  exact ⟨r, hr, fun s hs => time_allen_1_pairwise_disjoint s r x y hs hr⟩

/-! ## 4. BFO-DISJOINT-1 (`GRAPH-ROLE-1`) & Target `DECLARED-DISJOINT` -/

/-- ISO/IEC 21838-2 Basic Formal Ontology top-level partition reflected in Norn's
`GRAPH-ROLE-1`: `thing` (Continuant) vs `happening` (Occurrent). -/
inductive ConceptRole where
  | thing
  | happening
  deriving DecidableEq, Repr

/-- BFO continuant/occurrent partition: every `ConceptRole` is either `thing` or
`happening`, and never both. -/
theorem bfo_role_partition (r : ConceptRole) :
    (r = .thing ∨ r = .happening) ∧ ¬(r = .thing ∧ r = .happening) := by
  cases r
  · exact ⟨Or.inl rfl, fun h => nomatch h.2⟩
  · exact ⟨Or.inr rfl, fun h => nomatch h.1⟩

/-- Constitutional ontology interfaces across `core.*` and `data.*`. -/
inductive ConstitutionalInterface where
  | coreAgent
  | coreResource
  | coreClaim
  | coreEconomicEvent
  | coreCommitment
  | dataDataset
  | dataDatasetSnapshot
  | dataActivity
  | dataDerivation
  | dataObservation
  | dataQualityFinding
  deriving DecidableEq, Repr

/-- Declared `GRAPH-ROLE-1` role for each constitutional interface (`QualityFinding`
is modeled in Norn as a recorded assessment/resolution act with role `happening`). -/
def interfaceRole : ConstitutionalInterface → ConceptRole
  | .coreAgent => .thing
  | .coreResource => .thing
  | .coreClaim => .thing
  | .coreEconomicEvent => .happening
  | .coreCommitment => .happening
  | .dataDataset => .thing
  | .dataDatasetSnapshot => .thing
  | .dataActivity => .happening
  | .dataDerivation => .happening
  | .dataObservation => .happening
  | .dataQualityFinding => .happening

/-- Target declared interface disjointness pairs (`core.Claim ⊓ core.Commitment = ∅`
and `data.Observation ⊓ {core.EconomicEvent, core.Commitment, core.Claim} = ∅`,
pending `InterfaceDef.disjointWith` in PR #1781). -/
def declaredDisjoint (a b : ConstitutionalInterface) : Prop :=
  (a = .dataObservation ∧ (b = .coreEconomicEvent ∨ b = .coreCommitment ∨ b = .coreClaim)) ∨
  (b = .dataObservation ∧ (a = .coreEconomicEvent ∨ a = .coreCommitment ∨ a = .coreClaim)) ∨
  (a = .coreClaim ∧ b = .coreCommitment) ∨
  (a = .coreCommitment ∧ b = .coreClaim)

/-- An admitted concept carries a single effective `ConceptRole`, a set of
implemented constitutional interfaces, role agreement with every implemented
interface (`GRAPH-ROLE-1`), and respects declared disjointness pairs. -/
structure AdmittedConcept where
  role : ConceptRole
  implements : ConstitutionalInterface → Prop
  roleAgrees : ∀ iface, implements iface → interfaceRole iface = role
  respectsDisjoint : ∀ a b, declaredDisjoint a b → ¬(implements a ∧ implements b)

/-- BFO-DISJOINT-1 (`GRAPH-ROLE-1` / ISO/IEC 21838-2): no admitted concept can
implement both a `thing` interface and a `happening` interface. -/
theorem bfo_continuant_occurrent_disjoint
    (c : AdmittedConcept) (t h : ConstitutionalInterface)
    (ht : interfaceRole t = .thing)
    (hh : interfaceRole h = .happening) :
    ¬(c.implements t ∧ c.implements h) := by
  intro both
  have rt := c.roleAgrees t both.1
  have rh := c.roleAgrees h both.2
  rw [ht] at rt
  rw [hh] at rh
  have hEq : ConceptRole.thing = ConceptRole.happening := rt.trans rh.symm
  nomatch hEq

/-- BFO-DISJOINT-1 (`Observation ⊓ Claim = ∅`): derived purely from
`GRAPH-ROLE-1` Continuant/Occurrent disjointness (`Observation` is a happening,
`Claim` is a thing; enforced by `norn-checker` today). -/
theorem bfo_disjoint_1_observation_claim (c : AdmittedConcept) :
    ¬(c.implements .dataObservation ∧ c.implements .coreClaim) :=
  fun both =>
    bfo_continuant_occurrent_disjoint c .coreClaim .dataObservation rfl rfl ⟨both.2, both.1⟩

/-- BFO-DISJOINT-1 (`DatasetSnapshot ⊓ Activity = ∅`): derived purely from
`GRAPH-ROLE-1` Continuant/Occurrent disjointness (enforced by `norn-checker`
today). -/
theorem bfo_disjoint_1_snapshot_activity (c : AdmittedConcept) :
    ¬(c.implements .dataDatasetSnapshot ∧ c.implements .dataActivity) :=
  bfo_continuant_occurrent_disjoint c .dataDatasetSnapshot .dataActivity rfl rfl

/-- DECLARED-DISJOINT (`Observation ⊓ EconomicEvent = ∅`): both interfaces have
role `happening`, so disjointness does not follow from BFO roles alone; it
projects the assumed `declaredDisjoint` hypothesis modeling the target
`disjointWith` contract (pending PR #1781 in `norn-checker`). -/
theorem declared_disjoint_observation_economic_event (c : AdmittedConcept) :
    ¬(c.implements .dataObservation ∧ c.implements .coreEconomicEvent) :=
  c.respectsDisjoint .dataObservation .coreEconomicEvent (Or.inl ⟨rfl, Or.inl rfl⟩)

/-- DECLARED-DISJOINT (`Observation ⊓ Commitment = ∅`): both interfaces have
role `happening`; projects the assumed `declaredDisjoint` hypothesis modeling
the target `disjointWith` contract (pending PR #1781 in `norn-checker`). -/
theorem declared_disjoint_observation_commitment (c : AdmittedConcept) :
    ¬(c.implements .dataObservation ∧ c.implements .coreCommitment) :=
  c.respectsDisjoint .dataObservation .coreCommitment (Or.inl ⟨rfl, Or.inr (Or.inl rfl)⟩)

end Norn.Verification.DataLineage
