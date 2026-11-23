import Std

namespace Norn.Verification.TimeDstPin

/-!
Core 01 `TIME-PIN-1` pinned IANA tzdb wall-clock -> UTC interval resolution.

This is a mathematical reference for `time.Calendar` and `TIME-PIN-1` temporal
materialization. It has no axioms, `sorry`, or `admit`. Every theorem is
parametric over an arbitrary tzdb rule mapping local wall-clock times to unique
UTC instants, spring-forward gaps, or fall-back folds; it is not a refinement
proof for the Rust chrono/tzdb runtime or storage codecs.

Laws proved:
* Determinism: pinned `(rule, policy, startLocal, endLocal)` deterministically
  yields one outcome.
* Half-open interval validity: every admitted UTC interval satisfies
  `startUtc < endUtc`; zero-length or reversed intervals are refused.
* Fail-closed gap refusal: spring-forward gaps refuse under every `DstPolicy`.
* Fail-closed fold refusal: ambiguous fall-back folds refuse under `DstPolicy.reject`,
  and resolve strictly ordered (`earlierUtc < laterUtc`) under `earlier`/`later`.
* Replay pin immutability: upgrading the active tzdb version leaves every
  previously persisted `PinnedRecord` and its UTC bounds unchanged.
-/

inductive DstPolicy where
  | reject
  | earlier
  | later
  deriving DecidableEq

/-- Classification of one local wall-clock instant under a pinned IANA tzdb version. -/
inductive LocalResolution where
  | unique (utc : Int)
  | gap
  | fold (earlierUtc laterUtc : Int)
  deriving DecidableEq

/-- Every fall-back fold in a valid tzdb rule orders the earlier occurrence strictly
before the later occurrence. -/
def ValidTzRule (rule : Nat → LocalResolution) : Prop :=
  ∀ localTime e l, rule localTime = .fold e l → e < l

/-- Resolve a single local wall-clock instant under a pinned tzdb rule and DST policy. -/
def resolveInstant (rule : Nat → LocalResolution) (policy : DstPolicy) (localTime : Nat) :
    Option Int :=
  match rule localTime with
  | .unique u => some u
  | .gap => none
  | .fold e l =>
      match policy with
      | .reject => none
      | .earlier => some e
      | .later => some l

structure UtcInterval where
  startUtc : Int
  endUtc : Int
  deriving DecidableEq

def UtcInterval.isHalfOpen (iv : UtcInterval) : Prop :=
  iv.startUtc < iv.endUtc

/-- Materialize a local wall-clock window into a half-open UTC interval `[startUtc, endUtc)`. -/
def resolveInterval
    (rule : Nat → LocalResolution)
    (policy : DstPolicy)
    (startLocal endLocal : Nat) : Option UtcInterval :=
  match resolveInstant rule policy startLocal, resolveInstant rule policy endLocal with
  | some s, some e =>
      if s < e then some { startUtc := s, endUtc := e } else none
  | _, _ => none

theorem resolveInstant_deterministic
    (rule : Nat → LocalResolution) (policy : DstPolicy) (t : Nat)
    {u1 u2 : Int}
    (h1 : resolveInstant rule policy t = some u1)
    (h2 : resolveInstant rule policy t = some u2) :
    u1 = u2 := by
  rw [h1] at h2
  exact Option.some.inj h2

theorem resolveInterval_deterministic
    (rule : Nat → LocalResolution) (policy : DstPolicy) (startLocal endLocal : Nat)
    {iv1 iv2 : UtcInterval}
    (h1 : resolveInterval rule policy startLocal endLocal = some iv1)
    (h2 : resolveInterval rule policy startLocal endLocal = some iv2) :
    iv1 = iv2 := by
  rw [h1] at h2
  exact Option.some.inj h2

/-- Spring-forward gaps always fail closed under every `DstPolicy`. -/
theorem resolveInstant_gap_refused
    (rule : Nat → LocalResolution) (policy : DstPolicy) (t : Nat)
    (hgap : rule t = .gap) :
    resolveInstant rule policy t = none := by
  simp only [resolveInstant, hgap]

/-- Ambiguous fall-back folds fail closed under `DstPolicy.reject`. -/
theorem resolveInstant_fold_reject_refused
    (rule : Nat → LocalResolution) (t : Nat) (e l : Int)
    (hfold : rule t = .fold e l) :
    resolveInstant rule .reject t = none := by
  simp only [resolveInstant, hfold]

/-- Under a valid tzdb rule, `earlier` and `later` resolve a fold deterministically
with `earlierUtc < laterUtc`. -/
theorem resolveInstant_fold_ordered
    {rule : Nat → LocalResolution} (hvalid : ValidTzRule rule)
    (t : Nat) (e l : Int) (hfold : rule t = .fold e l) :
    resolveInstant rule .earlier t = some e ∧
      resolveInstant rule .later t = some l ∧
      e < l := by
  refine ⟨?_, ?_, hvalid t e l hfold⟩
  · simp only [resolveInstant, hfold]
  · simp only [resolveInstant, hfold]

/-- Any interval whose start falls in a spring-forward gap is refused. -/
theorem resolveInterval_gap_start_refused
    (rule : Nat → LocalResolution) (policy : DstPolicy) (startLocal endLocal : Nat)
    (hgap : rule startLocal = .gap) :
    resolveInterval rule policy startLocal endLocal = none := by
  simp only [resolveInterval, resolveInstant_gap_refused rule policy startLocal hgap]

/-- Any interval whose end falls in a spring-forward gap is refused. -/
theorem resolveInterval_gap_end_refused
    (rule : Nat → LocalResolution) (policy : DstPolicy) (startLocal endLocal : Nat)
    (hgap : rule endLocal = .gap) :
    resolveInterval rule policy startLocal endLocal = none := by
  simp only [resolveInterval, resolveInstant_gap_refused rule policy endLocal hgap]
  cases resolveInstant rule policy startLocal <;> rfl

/-- Any interval whose start falls in a fall-back fold is refused under `DstPolicy.reject`. -/
theorem resolveInterval_fold_reject_start_refused
    (rule : Nat → LocalResolution) (startLocal endLocal : Nat) (e l : Int)
    (hfold : rule startLocal = .fold e l) :
    resolveInterval rule .reject startLocal endLocal = none := by
  simp only [resolveInterval, resolveInstant_fold_reject_refused rule startLocal e l hfold]

/-- Any interval whose end falls in a fall-back fold is refused under `DstPolicy.reject`. -/
theorem resolveInterval_fold_reject_end_refused
    (rule : Nat → LocalResolution) (startLocal endLocal : Nat) (e l : Int)
    (hfold : rule endLocal = .fold e l) :
    resolveInterval rule .reject startLocal endLocal = none := by
  simp only [resolveInterval, resolveInstant_fold_reject_refused rule endLocal e l hfold]
  cases resolveInstant rule .reject startLocal <;> rfl

/-- Every admitted materialized UTC interval is strictly half-open (`startUtc < endUtc`). -/
theorem resolveInterval_admitted_half_open
    {rule : Nat → LocalResolution} {policy : DstPolicy} {startLocal endLocal : Nat}
    {iv : UtcInterval}
    (h : resolveInterval rule policy startLocal endLocal = some iv) :
    iv.isHalfOpen := by
  unfold resolveInterval at h
  cases hs : resolveInstant rule policy startLocal with
  | none =>
      rw [hs] at h
      contradiction
  | some s =>
      rw [hs] at h
      cases he : resolveInstant rule policy endLocal with
      | none =>
          rw [he] at h
          contradiction
      | some e =>
          rw [he] at h
          dsimp only at h
          by_cases hlt : s < e
          · rw [if_pos hlt] at h
            have hiv := Option.some.inj h
            rw [← hiv]
            exact hlt
          · rw [if_neg hlt] at h
            contradiction

/-- Zero-length or reversed resolved endpoints (`e ≤ s`) are always refused. -/
theorem resolveInterval_non_increasing_refused
    {rule : Nat → LocalResolution} {policy : DstPolicy} {startLocal endLocal : Nat}
    {s e : Int}
    (hs : resolveInstant rule policy startLocal = some s)
    (he : resolveInstant rule policy endLocal = some e)
    (hge : e ≤ s) :
    resolveInterval rule policy startLocal endLocal = none := by
  unfold resolveInterval
  rw [hs, he]
  dsimp only
  have hnot : ¬ (s < e) := by omega
  rw [if_neg hnot]

/-- A persisted temporal record keeps its resolved UTC bounds alongside its temporal pins. -/
structure PinnedRecord where
  id : Nat
  tzdbVersion : Nat
  policy : DstPolicy
  interval : UtcInterval
  deriving DecidableEq

structure Store where
  activeTzdb : Nat
  records : List PinnedRecord
  deriving DecidableEq

def AllHalfOpen (recs : List PinnedRecord) : Prop :=
  ∀ r ∈ recs, r.interval.isHalfOpen

inductive Op where
  | materialize (id : Nat) (policy : DstPolicy) (startLocal endLocal : Nat)
  | upgradeTzdb (newVersion : Nat)
  deriving DecidableEq

def step (catalog : Nat → Nat → LocalResolution) (st : Store) : Op → Option Store
  | .materialize id policy startLocal endLocal =>
      match resolveInterval (catalog st.activeTzdb) policy startLocal endLocal with
      | none => none
      | some iv =>
          let entry : PinnedRecord := {
            id := id
            tzdbVersion := st.activeTzdb
            policy := policy
            interval := iv
          }
          some { st with records := entry :: st.records }
  | .upgradeTzdb newVersion =>
      some { st with activeTzdb := newVersion }

def exec (catalog : Nat → Nat → LocalResolution) : Store → List Op → Option Store
  | st, [] => some st
  | st, op :: ops =>
      match step catalog st op with
      | none => none
      | some next => exec catalog next ops

/-- Upgrading the active tzdb version leaves every persisted record unchanged on replay. -/
theorem upgradeTzdb_preserves_persisted_records
    (catalog : Nat → Nat → LocalResolution) (st : Store) (newVersion : Nat) :
    step catalog st (.upgradeTzdb newVersion) = some { st with activeTzdb := newVersion } := by
  rfl

/-- Every admitted store operation preserves half-open validity of all persisted records
and only appends new records without mutating prior ones. -/
theorem step_preserves_half_open_and_append_only
    {catalog : Nat → Nat → LocalResolution} {st next : Store} {op : Op}
    (hvalid : AllHalfOpen st.records)
    (hstep : step catalog st op = some next) :
    AllHalfOpen next.records ∧ ∃ added, next.records = added ++ st.records := by
  cases op with
  | materialize id policy startLocal endLocal =>
      unfold step at hstep
      cases hres : resolveInterval (catalog st.activeTzdb) policy startLocal endLocal with
      | none =>
          rw [hres] at hstep
          contradiction
      | some iv =>
          rw [hres] at hstep
          have hnext := Option.some.inj hstep
          rw [← hnext]
          refine ⟨?_, [{ id := id, tzdbVersion := st.activeTzdb, policy := policy, interval := iv }], rfl⟩
          intro r hr
          rcases List.mem_cons.mp hr with rfl | hr
          · exact resolveInterval_admitted_half_open hres
          · exact hvalid r hr
  | upgradeTzdb newVersion =>
      unfold step at hstep
      have hnext := Option.some.inj hstep
      rw [← hnext]
      exact ⟨hvalid, [], rfl⟩

/-- Inductive safety: any workflow of temporal materializations and tzdb upgrades
preserves half-open UTC intervals and never mutates previously persisted records. -/
theorem run_preserves_half_open :
    ∀ {catalog : Nat → Nat → LocalResolution} {st next : Store} {ops : List Op},
      AllHalfOpen st.records →
      exec catalog st ops = some next →
      AllHalfOpen next.records
  | _, st, next, [], hvalid, h => by
      simp only [exec] at h
      rw [← Option.some.inj h]
      exact hvalid
  | catalog, st, next, op :: ops, hvalid, h => by
      unfold exec at h
      split at h
      · contradiction
      · rename_i mid hmid
        have hstep := step_preserves_half_open_and_append_only hvalid hmid
        exact run_preserves_half_open hstep.1 h

end Norn.Verification.TimeDstPin
