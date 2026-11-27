import Std

/-!
Reference algebraic proofs for:

1. **`BI-ADD-1` (W3C RDF Data Cube / Kimball Rollup Soundness)**:
   * Additive homomorphism: flow sums decompose additively across arbitrary
     disjoint partitions (1D lists and 2D entity × time-window tables) and are
     invariant to partition concatenation order.
   * Non-additive ratio soundness and counterexamples: the weighted ratio of a
     concatenated partition equals `(num(A) + num(B)) / (den(A) + den(B))` for
     all positive denominators, and explicit counterexamples prove that neither
     averaging nor summing sub-slice ratios reproduces the combined ratio.
   * Semi-additive balance theorem: point-in-time spatial sums across disjoint
     account sets commute with snapshots, whereas summing an unchanged positive
     holding across time double-counts it (`sumFlow [s, s] ≠ s` for `s > 0`),
     while taking the latest snapshot is idempotent (`lastSnapshot [s, s] = some s`).

2. **`ANALYTICS-SPLIT-1` (No-Lookahead Split Separation)**:
   * Over an ordered split `trainCutoff ≤ holdoutStart`, every training
     observation with `knownAt < trainCutoff` strictly precedes every holdout
     outcome with `occurredAt ≥ holdoutStart` (`knownAt < occurredAt`), no
     instant belongs to both windows, and any label learned at or after a
     holdout outcome is excluded from the training window.

3. **`UNITS-SCALE-1` (Exact Rational Conversion Consistency)**:
   * Exact rational factor scaling (`ExactRat`) is invertible for all positive
     factors, composes associatively with zero rounding drift, and respects
     rational equivalence. Concrete theorems pin `ms -> s`, `us -> s`, `ns -> s`,
     `bit -> By`, `KiBy -> By`, `MiBy -> By`, `GiBy -> By`, `TiBy -> By`,
     `kBy -> By`, and `kW.h -> J` (including IEC binary `KiBy ≠ kBy`).

Evidence scope: checked proofs of the reference mathematical model over `Int`
and `ExactRat`. This file does not prove correspondence to Rust lowering,
SQL/DuckDB/DataFusion execution, or cell runtime storage.
-/
namespace Norn.Verification.BiAdditivity

/-! ## Exact rational numbers over `Int` -/

/-- An exact rational number `num / den` with strictly positive denominator. -/
structure ExactRat where
  num : Int
  den : Int
  den_pos : 0 < den

/-- Cross-multiplied rational equivalence `a.num / a.den = b.num / b.den`. -/
def ExactRat.eqv (a b : ExactRat) : Prop :=
  a.num * b.den = b.num * a.den

/-- Exact rational addition. -/
def ExactRat.add (a b : ExactRat) : ExactRat where
  num := a.num * b.den + b.num * a.den
  den := a.den * b.den
  den_pos := Int.mul_pos a.den_pos b.den_pos

/-- Exact rational multiplication. -/
def ExactRat.mul (a b : ExactRat) : ExactRat where
  num := a.num * b.num
  den := a.den * b.den
  den_pos := Int.mul_pos a.den_pos b.den_pos

/-- Exact reciprocal of a strictly positive rational factor. -/
def ExactRat.inv (f : ExactRat) (hpos : 0 < f.num) : ExactRat where
  num := f.den
  den := f.num
  den_pos := hpos

/-- Unweighted arithmetic mean `(a + b) / 2` of two exact rationals. -/
def ExactRat.avg (a b : ExactRat) : ExactRat where
  num := a.num * b.den + b.num * a.den
  den := 2 * (a.den * b.den)
  den_pos := by
    have h := Int.mul_pos a.den_pos b.den_pos
    omega

theorem ExactRat.eqv_refl (a : ExactRat) : a.eqv a := rfl

theorem ExactRat.eqv_symm {a b : ExactRat} (h : a.eqv b) : b.eqv a :=
  h.symm

theorem ExactRat.eqv_trans {a b c : ExactRat} (hab : a.eqv b) (hbc : b.eqv c) : a.eqv c := by
  unfold ExactRat.eqv at *
  have hb_ne : b.den ≠ 0 := by
    have := b.den_pos
    omega
  have hmul : (a.num * c.den) * b.den = (c.num * a.den) * b.den := by
    calc
      (a.num * c.den) * b.den = (a.num * b.den) * c.den := by
        rw [Int.mul_assoc, Int.mul_comm c.den b.den, ← Int.mul_assoc]
      _ = (b.num * a.den) * c.den := by rw [hab]
      _ = a.den * (b.num * c.den) := by
        rw [Int.mul_comm b.num a.den, Int.mul_assoc]
      _ = a.den * (c.num * b.den) := by rw [hbc]
      _ = (c.num * a.den) * b.den := by
        rw [← Int.mul_assoc, Int.mul_comm a.den c.num]
  exact Int.eq_of_mul_eq_mul_right hb_ne hmul

/-! ## BI-ADD-1 Part 1: Additive flow homomorphism -/

/-- Exact total of a list of additive flow observations. -/
def sumFlow : List Int → Int
  | [] => 0
  | x :: xs => x + sumFlow xs

/-- Additive homomorphism: summing a concatenated partition equals the sum of
the partition sums. -/
theorem additive_homomorphism (a b : List Int) :
    sumFlow (a ++ b) = sumFlow a + sumFlow b := by
  induction a with
  | nil =>
      simp only [List.nil_append, sumFlow]
      omega
  | cons x xs ih =>
      simp only [List.cons_append, sumFlow, ih]
      omega

/-- Additive rollups are invariant to partition concatenation order. -/
theorem additive_comm_append (a b : List Int) :
    sumFlow (a ++ b) = sumFlow (b ++ a) := by
  rw [additive_homomorphism, additive_homomorphism]
  omega

/-- Two-dimensional rollup across entities and time windows. -/
def sumMatrix (rows : List (List Int)) : Int :=
  sumFlow (rows.map sumFlow)

/-- Additive homomorphism across both entity partitions and time-window tables. -/
theorem additive_rollup_2d (p q : List (List Int)) :
    sumMatrix (p ++ q) = sumMatrix p + sumMatrix q := by
  simp only [sumMatrix, List.map_append]
  exact additive_homomorphism (p.map sumFlow) (q.map sumFlow)

/-! ## BI-ADD-1 Part 2: Non-additive ratio soundness and counterexamples -/

/-- One observation contributing an exact numerator and strictly positive
denominator to a non-additive ratio metric. -/
structure RatioObs where
  num : Int
  den : Int
  den_pos : 0 < den

/-- Total numerator across a slice of ratio observations. -/
def sumNum (xs : List RatioObs) : Int :=
  sumFlow (xs.map RatioObs.num)

/-- Total denominator across a slice of ratio observations. -/
def sumDen (xs : List RatioObs) : Int :=
  sumFlow (xs.map RatioObs.den)

theorem sumNum_append (a b : List RatioObs) :
    sumNum (a ++ b) = sumNum a + sumNum b := by
  simp only [sumNum, List.map_append]
  exact additive_homomorphism (a.map RatioObs.num) (b.map RatioObs.num)

theorem sumDen_append (a b : List RatioObs) :
    sumDen (a ++ b) = sumDen a + sumDen b := by
  simp only [sumDen, List.map_append]
  exact additive_homomorphism (a.map RatioObs.den) (b.map RatioObs.den)

theorem sumDen_nonneg (xs : List RatioObs) : 0 ≤ sumDen xs := by
  induction xs with
  | nil =>
      simp only [sumDen, List.map_nil, sumFlow]
      omega
  | cons x xs ih =>
      have hx := x.den_pos
      simp only [sumDen, List.map_cons, sumFlow] at *
      omega

theorem sumDen_pos_of_cons (x : RatioObs) (xs : List RatioObs) :
    0 < sumDen (x :: xs) := by
  have hx := x.den_pos
  have hxs := sumDen_nonneg xs
  simp only [sumDen, List.map_cons, sumFlow] at *
  omega

theorem sumDen_append_pos (a b : List RatioObs) (ha : 0 < sumDen a) (hb : 0 < sumDen b) :
    0 < sumDen (a ++ b) := by
  rw [sumDen_append]
  omega

/-- Exact weighted ratio `sumNum(xs) / sumDen(xs)` for a positive-denominator slice. -/
def weightedRatio (xs : List RatioObs) (hpos : 0 < sumDen xs) : ExactRat where
  num := sumNum xs
  den := sumDen xs
  den_pos := hpos

/-- Ratio-of-sums rollup soundness: the combined weighted ratio of two disjoint
positive-denominator partitions equals `(num(A) + num(B)) / (den(A) + den(B))`. -/
theorem weightedRatio_append_sound (a b : List RatioObs)
    (ha : 0 < sumDen a) (hb : 0 < sumDen b) :
    (weightedRatio (a ++ b) (sumDen_append_pos a b ha hb)).eqv
      ⟨sumNum a + sumNum b, sumDen a + sumDen b, by omega⟩ := by
  simp only [weightedRatio, ExactRat.eqv, sumNum_append, sumDen_append]

/-- Classic BI bug counterexample (unweighted average of ratios):
partition `A` has `1/1` (100%) and partition `B` has `0/3` (0%).
The true weighted ratio is `1/4` (25%), while `avg(1/1, 0/3) = 1/2` (50%). -/
theorem ratio_not_averageable :
    ∃ (a b : List RatioObs) (ha : 0 < sumDen a) (hb : 0 < sumDen b),
      ¬ (weightedRatio (a ++ b) (sumDen_append_pos a b ha hb)).eqv
        (ExactRat.avg (weightedRatio a ha) (weightedRatio b hb)) := by
  let a : List RatioObs := [⟨1, 1, by omega⟩]
  let b : List RatioObs := [⟨0, 3, by omega⟩]
  have ha : 0 < sumDen a := by
    simp only [a, sumDen, List.map_cons, List.map_nil, sumFlow]
    omega
  have hb : 0 < sumDen b := by
    simp only [b, sumDen, List.map_cons, List.map_nil, sumFlow]
    omega
  refine ⟨a, b, ha, hb, ?_⟩
  simp only [a, b, weightedRatio, ExactRat.avg, ExactRat.eqv, sumNum, sumDen,
    List.cons_append, List.nil_append, List.map_cons, List.map_nil, sumFlow]
  omega

/-- Non-additive ratios also cannot be summed across partitions:
`A = [1/1]` and `B = [1/1]` have combined ratio `2/2 = 1`, not `1 + 1 = 2`. -/
theorem ratio_not_summable :
    ∃ (a b : List RatioObs) (ha : 0 < sumDen a) (hb : 0 < sumDen b),
      ¬ (weightedRatio (a ++ b) (sumDen_append_pos a b ha hb)).eqv
        (ExactRat.add (weightedRatio a ha) (weightedRatio b hb)) := by
  let a : List RatioObs := [⟨1, 1, by omega⟩]
  let b : List RatioObs := [⟨1, 1, by omega⟩]
  have ha : 0 < sumDen a := by
    simp only [a, sumDen, List.map_cons, List.map_nil, sumFlow]
    omega
  have hb : 0 < sumDen b := by
    simp only [b, sumDen, List.map_cons, List.map_nil, sumFlow]
    omega
  refine ⟨a, b, ha, hb, ?_⟩
  simp only [a, b, weightedRatio, ExactRat.add, ExactRat.eqv, sumNum, sumDen,
    List.cons_append, List.nil_append, List.map_cons, List.map_nil, sumFlow]
  omega

/-! ## BI-ADD-1 Part 3: Semi-additive point-in-time balance theorem -/

/-- Spatial total across a list of account balance trajectories at instant `t`. -/
def spatialTotal (accounts : List (Int → Int)) (t : Int) : Int :=
  sumFlow (accounts.map (fun balance => balance t))

/-- Semi-additive spatial homomorphism: summing point-in-time stock balances
across disjoint account partitions commutes with taking the snapshot at `t`. -/
theorem semi_additive_spatial_commutes (acctA acctB : List (Int → Int)) (t : Int) :
    spatialTotal (acctA ++ acctB) t = spatialTotal acctA t + spatialTotal acctB t := by
  simp only [spatialTotal, List.map_append]
  exact additive_homomorphism
    (acctA.map (fun balance => balance t))
    (acctB.map (fun balance => balance t))

/-- Summing an unchanged strictly positive holding over two time snapshots
double-counts it (`sumFlow [s, s] = 2 * s ≠ s`). -/
theorem semi_additive_temporal_sum_double_counts (s : Int) (hs : 0 < s) :
    sumFlow [s, s] ≠ s := by
  simp only [sumFlow]
  omega

/-- Existential witness that temporal summation over unchanged holdings fails
for semi-additive stock measures. -/
theorem exists_semi_additive_temporal_double_count :
    ∃ s : Int, 0 < s ∧ sumFlow [s, s] ≠ s :=
  ⟨1, by omega, semi_additive_temporal_sum_double_counts 1 (by omega)⟩

/-- Point-in-time `last` reduction along the time axis (`METRIC-SNAPSHOT-ASOF-1`). -/
def lastSnapshot : List Int → Option Int
  | [] => none
  | [x] => some x
  | _ :: y :: ys => lastSnapshot (y :: ys)

/-- Unlike temporal summation, `lastSnapshot` does not double-count an
unchanged holding observed across consecutive snapshots. -/
theorem lastSnapshot_unchanged (s : Int) : lastSnapshot [s, s] = some s := rfl

/-! ## ANALYTICS-SPLIT-1: No-lookahead temporal split separation -/

/-- Preregistered temporal split with ordered boundary `trainCutoff ≤ holdoutStart`. -/
structure TemporalSplit where
  trainCutoff : Int
  holdoutStart : Int
  ordered : trainCutoff ≤ holdoutStart

/-- No-lookahead theorem: every training record known strictly before
`trainCutoff` strictly precedes every holdout outcome occurring at or after
`holdoutStart`. -/
theorem no_lookahead_split (split : TemporalSplit) (knownAt occurredAt : Int)
    (hTrain : knownAt < split.trainCutoff)
    (hHoldout : split.holdoutStart ≤ occurredAt) :
    knownAt < occurredAt := by
  have hOrd := split.ordered
  omega

/-- Training and holdout instant windows are disjoint. -/
theorem disjoint_train_holdout (split : TemporalSplit) (t : Int) :
    ¬ (t < split.trainCutoff ∧ split.holdoutStart ≤ t) := by
  have hOrd := split.ordered
  omega

/-- Any label learned at or after a holdout outcome (`occurredAt ≤ knownAt`)
is excluded from the training knowledge window (`¬ (knownAt < trainCutoff)`). -/
theorem late_holdout_label_excluded (split : TemporalSplit) (knownAt occurredAt : Int)
    (hHoldout : split.holdoutStart ≤ occurredAt)
    (hCausal : occurredAt ≤ knownAt) :
    ¬ (knownAt < split.trainCutoff) := by
  have hOrd := split.ordered
  omega

/-! ## UNITS-SCALE-1: Exact rational unit conversion consistency -/

/-- Scale an exact rational quantity by an exact rational conversion factor. -/
def ExactRat.scale (x factor : ExactRat) : ExactRat :=
  ExactRat.mul x factor

/-- Exact scaling preserves rational equivalence. -/
theorem scale_respects_eqv {x y : ExactRat} (f : ExactRat) (h : x.eqv y) :
    (x.scale f).eqv (y.scale f) := by
  simp only [ExactRat.eqv, ExactRat.scale, ExactRat.mul] at *
  calc
    (x.num * f.num) * (y.den * f.den) = (x.num * y.den) * (f.num * f.den) := by
      rw [Int.mul_assoc x.num f.num (y.den * f.den),
          Int.mul_left_comm f.num y.den f.den,
          ← Int.mul_assoc x.num y.den (f.num * f.den)]
    _ = (y.num * x.den) * (f.num * f.den) := by rw [h]
    _ = (y.num * f.num) * (x.den * f.den) := by
      rw [Int.mul_assoc y.num x.den (f.num * f.den),
          Int.mul_left_comm x.den f.num f.den,
          ← Int.mul_assoc y.num f.num (x.den * f.den)]

/-- Exact rational conversion invertibility: scaling by any positive rational
factor and then by its exact reciprocal returns an equivalent rational with
zero rounding drift. -/
theorem scale_invertible (x factor : ExactRat) (hpos : 0 < factor.num) :
    ((x.scale factor).scale (factor.inv hpos)).eqv x := by
  simp only [ExactRat.eqv, ExactRat.scale, ExactRat.mul, ExactRat.inv]
  calc
    ((x.num * factor.num) * factor.den) * x.den
        = (x.num * (factor.num * factor.den)) * x.den := by
          rw [Int.mul_assoc x.num factor.num factor.den]
    _ = x.num * ((factor.num * factor.den) * x.den) := by
          rw [Int.mul_assoc x.num (factor.num * factor.den) x.den]
    _ = x.num * (x.den * (factor.den * factor.num)) := by
          rw [Int.mul_comm (factor.num * factor.den) x.den,
              Int.mul_comm factor.num factor.den]
    _ = x.num * ((x.den * factor.den) * factor.num) := by
          rw [← Int.mul_assoc x.den factor.den factor.num]

/-- Exact rational conversion composition is associative with zero drift. -/
theorem scale_compose (x f g : ExactRat) :
    ((x.scale f).scale g).eqv (x.scale (f.mul g)) := by
  simp only [ExactRat.eqv, ExactRat.scale, ExactRat.mul]
  rw [Int.mul_assoc x.num f.num g.num, Int.mul_assoc x.den f.den g.den]

/-- Exact UCUM / IEC 80000-13 / ISO 80000 conversion factors to canonical bases. -/
def msToS : ExactRat := ⟨1, 1000, by omega⟩
def usToS : ExactRat := ⟨1, 1000000, by omega⟩
def nsToS : ExactRat := ⟨1, 1000000000, by omega⟩
def bitToBy : ExactRat := ⟨1, 8, by omega⟩
def kiByToBy : ExactRat := ⟨1024, 1, by omega⟩
def miByToBy : ExactRat := ⟨1048576, 1, by omega⟩
def giByToBy : ExactRat := ⟨1073741824, 1, by omega⟩
def tiByToBy : ExactRat := ⟨1099511627776, 1, by omega⟩
def kByToBy : ExactRat := ⟨1000, 1, by omega⟩
def kWhToJ : ExactRat := ⟨3600000, 1, by omega⟩

theorem ms_roundtrip (x : ExactRat) :
    ((x.scale msToS).scale (msToS.inv (by simp only [msToS]; omega))).eqv x :=
  scale_invertible x msToS (by simp only [msToS]; omega)

theorem kiBy_roundtrip (x : ExactRat) :
    ((x.scale kiByToBy).scale (kiByToBy.inv (by simp only [kiByToBy]; omega))).eqv x :=
  scale_invertible x kiByToBy (by simp only [kiByToBy]; omega)

theorem giBy_roundtrip (x : ExactRat) :
    ((x.scale giByToBy).scale (giByToBy.inv (by simp only [giByToBy]; omega))).eqv x :=
  scale_invertible x giByToBy (by simp only [giByToBy]; omega)

theorem kWh_roundtrip (x : ExactRat) :
    ((x.scale kWhToJ).scale (kWhToJ.inv (by simp only [kWhToJ]; omega))).eqv x :=
  scale_invertible x kWhToJ (by simp only [kWhToJ]; omega)

theorem ns_composes_ms_cubed :
    ((msToS.mul msToS).mul msToS).eqv nsToS := by
  simp only [ExactRat.eqv, ExactRat.mul, msToS, nsToS]
  omega

theorem us_composes_ms_squared :
    (msToS.mul msToS).eqv usToS := by
  simp only [ExactRat.eqv, ExactRat.mul, msToS, usToS]
  omega

theorem giBy_composes_kiBy_cubed :
    ((kiByToBy.mul kiByToBy).mul kiByToBy).eqv giByToBy := by
  simp only [ExactRat.eqv, ExactRat.mul, kiByToBy, giByToBy]
  omega

theorem miBy_composes_kiBy_squared :
    (kiByToBy.mul kiByToBy).eqv miByToBy := by
  simp only [ExactRat.eqv, ExactRat.mul, kiByToBy, miByToBy]
  omega

theorem tiBy_composes_giBy_kiBy :
    (giByToBy.mul kiByToBy).eqv tiByToBy := by
  simp only [ExactRat.eqv, ExactRat.mul, giByToBy, kiByToBy, tiByToBy]
  omega

/-- IEC 80000-13 binary `KiBy` (1024 By) and SI decimal `kBy` (1000 By) are distinct. -/
theorem kiBy_neq_kBy : ¬ kiByToBy.eqv kByToBy := by
  simp only [ExactRat.eqv, kiByToBy, kByToBy]
  omega

/-- `1 kW.h = 1000 W * 3600 s = 3,600,000 J` exactly. -/
theorem kWh_eq_3600_kJ :
    (ExactRat.mul ⟨1000, 1, by omega⟩ ⟨3600, 1, by omega⟩).eqv kWhToJ := by
  simp only [ExactRat.eqv, ExactRat.mul, kWhToJ]
  omega

/-- `8 bit = 1 By` exactly. -/
theorem eight_bits_eq_one_byte :
    (ExactRat.scale ⟨8, 1, by omega⟩ bitToBy).eqv ⟨1, 1, by omega⟩ := by
  simp only [ExactRat.eqv, ExactRat.scale, ExactRat.mul, bitToBy]
  omega

end Norn.Verification.BiAdditivity
