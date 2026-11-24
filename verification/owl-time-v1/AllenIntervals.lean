import Std

/-!
Reference algebra of Allen's thirteen interval relations over proper half-open
intervals `[start, stop)` of integer instants (Core 01 temporal context:
intervals are half-open `[from, to)`).

Proved here, for every pair of proper intervals:

* exactly one of the thirteen relations holds (jointly exhaustive, pairwise
  disjoint), and it is the one the comparison classifier `classify` returns;
* each relation holds one way exactly when its converse holds the other way;
* two intervals share no instant exactly when the relation is one of before,
  after, meets or met-by, so adjacent `[a, b)` and `[b, c)` meet without
  overlapping;
* the scheduling compositions listed under "Composition";
* lexicographic order on `(seconds, nanoseconds)` ordering keys is the order of
  the instants.

Instants are `Int`. The theorems use only the order of the instants, so they
apply to any carrier that embeds in `Int` in order, such as a canonical UTC
ordering key counted in nanoseconds. This file does not prove correspondence
to the Rust classifier in `crates/norn-expr/src/temporal/allen.rs`; the bounded
check of that code is `allen_kani.rs` beside this file. Calendar arithmetic,
timezone rules and DST resolution are outside this file.
-/
namespace Norn.Verification.OwlTime

/-- A proper half-open interval `[start, stop)`: it has at least one instant. -/
structure Interval where
  start : Int
  stop : Int
  proper : start < stop

/-- The instant `t` lies in `[x.start, x.stop)`: the start is in, the stop is out. -/
def inInterval (t : Int) (x : Interval) : Prop := x.start ≤ t ∧ t < x.stop

/-! ## The thirteen relations, by endpoint conditions -/

def before (x y : Interval) : Prop := x.stop < y.start
def after (x y : Interval) : Prop := y.stop < x.start
def meets (x y : Interval) : Prop := x.stop = y.start
def metBy (x y : Interval) : Prop := y.stop = x.start
def overlaps (x y : Interval) : Prop :=
  x.start < y.start ∧ y.start < x.stop ∧ x.stop < y.stop
def overlappedBy (x y : Interval) : Prop :=
  y.start < x.start ∧ x.start < y.stop ∧ y.stop < x.stop
def starts (x y : Interval) : Prop := x.start = y.start ∧ x.stop < y.stop
def startedBy (x y : Interval) : Prop := x.start = y.start ∧ y.stop < x.stop
def during (x y : Interval) : Prop := y.start < x.start ∧ x.stop < y.stop
def contains (x y : Interval) : Prop := x.start < y.start ∧ y.stop < x.stop
def finishes (x y : Interval) : Prop := y.start < x.start ∧ x.stop = y.stop
def finishedBy (x y : Interval) : Prop := x.start < y.start ∧ x.stop = y.stop
def equals (x y : Interval) : Prop := x.start = y.start ∧ x.stop = y.stop

/-- The names of the thirteen relations. -/
inductive Relation where
  | before | after | meets | metBy | overlaps | overlappedBy | starts | startedBy
  | during | contains | finishes | finishedBy | equals
  deriving DecidableEq, Repr

/-- The relation named `r` holds from `x` to `y`. -/
def holds (r : Relation) (x y : Interval) : Prop :=
  match r with
  | .before => before x y
  | .after => after x y
  | .meets => meets x y
  | .metBy => metBy x y
  | .overlaps => overlaps x y
  | .overlappedBy => overlappedBy x y
  | .starts => starts x y
  | .startedBy => startedBy x y
  | .during => during x y
  | .contains => contains x y
  | .finishes => finishes x y
  | .finishedBy => finishedBy x y
  | .equals => equals x y

/-- The name of the relation read from the second interval to the first. -/
def Relation.converse : Relation → Relation
  | .before => .after
  | .after => .before
  | .meets => .metBy
  | .metBy => .meets
  | .overlaps => .overlappedBy
  | .overlappedBy => .overlaps
  | .starts => .startedBy
  | .startedBy => .starts
  | .during => .contains
  | .contains => .during
  | .finishes => .finishedBy
  | .finishedBy => .finishes
  | .equals => .equals

/-- Classification by endpoint comparison: first the starts, then the stops,
then (when both start and stop fall on the same side) the remaining pair. -/
def classify (x y : Interval) : Relation :=
  if x.start = y.start then
    if x.stop = y.stop then .equals
    else if x.stop < y.stop then .starts
    else .startedBy
  else if x.start < y.start then
    if x.stop = y.stop then .finishedBy
    else if y.stop < x.stop then .contains
    else if x.stop < y.start then .before
    else if x.stop = y.start then .meets
    else .overlaps
  else
    if x.stop = y.stop then .finishes
    else if x.stop < y.stop then .during
    else if y.stop < x.start then .after
    else if y.stop = x.start then .metBy
    else .overlappedBy

/-! ## Jointly exhaustive and pairwise disjoint -/

/-- At least one of the thirteen relations holds. -/
theorem thirteen_way (x y : Interval) :
    before x y ∨ after x y ∨ meets x y ∨ metBy x y ∨ overlaps x y ∨ overlappedBy x y ∨
      starts x y ∨ startedBy x y ∨ during x y ∨ contains x y ∨ finishes x y ∨
      finishedBy x y ∨ equals x y := by
  have hx := x.proper
  have hy := y.proper
  simp only [before, after, meets, metBy, overlaps, overlappedBy, starts, startedBy, during,
    contains, finishes, finishedBy, equals]
  omega

/-- Jointly exhaustive: some named relation holds. -/
theorem jointly_exhaustive (x y : Interval) : ∃ r, holds r x y := by
  rcases thirteen_way x y with h | h | h | h | h | h | h | h | h | h | h | h | h
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

/-- Pairwise disjoint: two relations that both hold are the same relation.
The case split covers all 13 × 13 ordered pairs, so all 78 unordered pairs of
distinct relations are excluded. -/
theorem pairwise_disjoint (r s : Relation) (x y : Interval)
    (hr : holds r x y) (hs : holds s x y) : r = s := by
  have hx := x.proper
  have hy := y.proper
  cases r <;> cases s <;>
    simp only [holds, before, after, meets, metBy, overlaps, overlappedBy, starts, startedBy,
      during, contains, finishes, finishedBy, equals] at hr hs <;>
    first
      | rfl
      | omega

/-- Two distinct relations never hold together. -/
theorem distinct_relations_exclude (r s : Relation) (x y : Interval) (distinct : r ≠ s) :
    ¬(holds r x y ∧ holds s x y) :=
  fun both => distinct (pairwise_disjoint r s x y both.1 both.2)

/-- Exactly one relation holds. -/
theorem exactly_one (x y : Interval) :
    ∃ r, holds r x y ∧ ∀ s, holds s x y → s = r := by
  rcases jointly_exhaustive x y with ⟨r, hr⟩
  exact ⟨r, hr, fun s hs => pairwise_disjoint s r x y hs hr⟩

/-- The comparison classifier returns a relation that holds. -/
theorem classify_holds (x y : Interval) : holds (classify x y) x y := by
  have hx := x.proper
  have hy := y.proper
  unfold classify
  repeat' split
  all_goals
    simp only [holds, before, after, meets, metBy, overlaps, overlappedBy, starts, startedBy,
      during, contains, finishes, finishedBy, equals]
    omega

/-- A relation holds exactly when the classifier returns it. -/
theorem holds_iff_classify (r : Relation) (x y : Interval) :
    holds r x y ↔ classify x y = r :=
  ⟨fun h => pairwise_disjoint (classify x y) r x y (classify_holds x y) h,
    fun h => h ▸ classify_holds x y⟩

/-! ## Converse dualities -/

theorem before_iff_after (x y : Interval) : before x y ↔ after y x := Iff.rfl
theorem meets_iff_metBy (x y : Interval) : meets x y ↔ metBy y x := Iff.rfl
theorem overlaps_iff_overlappedBy (x y : Interval) : overlaps x y ↔ overlappedBy y x :=
  Iff.rfl
theorem starts_iff_startedBy (x y : Interval) : starts x y ↔ startedBy y x := by
  simp only [starts, startedBy]
  omega
theorem during_iff_contains (x y : Interval) : during x y ↔ contains y x := Iff.rfl
theorem finishes_iff_finishedBy (x y : Interval) : finishes x y ↔ finishedBy y x := by
  simp only [finishes, finishedBy]
  omega
theorem equals_iff_equals (x y : Interval) : equals x y ↔ equals y x := by
  simp only [equals]
  omega

/-- Every relation holds one way exactly when its converse holds the other way. -/
theorem holds_iff_converse (r : Relation) (x y : Interval) :
    holds r x y ↔ holds r.converse y x := by
  cases r
  · exact before_iff_after x y
  · exact (before_iff_after y x).symm
  · exact meets_iff_metBy x y
  · exact (meets_iff_metBy y x).symm
  · exact overlaps_iff_overlappedBy x y
  · exact (overlaps_iff_overlappedBy y x).symm
  · exact starts_iff_startedBy x y
  · exact (starts_iff_startedBy y x).symm
  · exact during_iff_contains x y
  · exact (during_iff_contains y x).symm
  · exact finishes_iff_finishedBy x y
  · exact (finishes_iff_finishedBy y x).symm
  · exact equals_iff_equals x y

/-- Taking the converse twice gives the relation back. -/
theorem converse_converse (r : Relation) : r.converse.converse = r := by
  cases r <;> rfl

/-- Swapping the intervals turns the classification into its converse. -/
theorem classify_swap (x y : Interval) : classify y x = (classify x y).converse :=
  (holds_iff_classify (classify x y).converse y x).mp
    ((holds_iff_converse (classify x y) x y).mp (classify_holds x y))

/-! ## Boundary: sharing an instant -/

/-- No instant lies in both intervals. -/
def shareNoInstant (x y : Interval) : Prop := ∀ t, ¬(inInterval t x ∧ inInterval t y)

/-- The overlap test of the `intervals_overlap` builtin:
`aStart < bEnd && bStart < aEnd`. -/
def intersects (x y : Interval) : Prop := x.start < y.stop ∧ y.start < x.stop

/-- Non-overlap law: two proper half-open intervals share no instant exactly
when one is before, after, meeting or met by the other. -/
theorem shareNoInstant_iff (x y : Interval) :
    shareNoInstant x y ↔ before x y ∨ after x y ∨ meets x y ∨ metBy x y := by
  have hx := x.proper
  have hy := y.proper
  simp only [shareNoInstant, inInterval, before, after, meets, metBy]
  constructor
  · intro h
    have atXStart := h x.start
    have atYStart := h y.start
    omega
  · intro h t
    omega

/-- The builtin overlap test holds exactly when some instant lies in both. -/
theorem intersects_iff_shared_instant (x y : Interval) :
    intersects x y ↔ ∃ t, inInterval t x ∧ inInterval t y := by
  have hx := x.proper
  have hy := y.proper
  simp only [intersects, inInterval]
  constructor
  · intro h
    by_cases startsFirst : x.start ≤ y.start
    · exact ⟨y.start, by omega⟩
    · exact ⟨x.start, by omega⟩
  · intro h
    rcases h with ⟨t, h⟩
    omega

/-- The builtin overlap test is false exactly for before, after, meets and
met-by. -/
theorem intersects_iff_not_apart (x y : Interval) :
    intersects x y ↔ ¬(before x y ∨ after x y ∨ meets x y ∨ metBy x y) := by
  simp only [intersects, before, after, meets, metBy]
  omega

/-- Adjacent `[a, b)` and `[b, c)` meet, share no instant, and do not overlap. -/
theorem adjacent_meet_without_overlap (a b c : Int) (hab : a < b) (hbc : b < c) :
    meets ⟨a, b, hab⟩ ⟨b, c, hbc⟩ ∧ shareNoInstant ⟨a, b, hab⟩ ⟨b, c, hbc⟩ ∧
      ¬intersects ⟨a, b, hab⟩ ⟨b, c, hbc⟩ := by
  simp only [meets, shareNoInstant, inInterval, intersects]
  refine ⟨trivial, ?_, ?_⟩
  · intro t
    omega
  · omega

/-! ## Composition -/

theorem before_trans (x y z : Interval) (hxy : before x y) (hyz : before y z) :
    before x z := by
  have hy := y.proper
  simp only [before] at *
  omega

theorem after_trans (x y z : Interval) (hxy : after x y) (hyz : after y z) :
    after x z := by
  have hy := y.proper
  simp only [after] at *
  omega

theorem during_trans (x y z : Interval) (hxy : during x y) (hyz : during y z) :
    during x z := by
  simp only [during] at *
  omega

theorem contains_trans (x y z : Interval) (hxy : contains x y) (hyz : contains y z) :
    contains x z := by
  simp only [contains] at *
  omega

theorem equals_trans (x y z : Interval) (hxy : equals x y) (hyz : equals y z) :
    equals x z := by
  simp only [equals] at *
  omega

/-- Two consecutive hand-overs leave a gap as long as the middle interval. -/
theorem meets_meets_before (x y z : Interval) (hxy : meets x y) (hyz : meets y z) :
    before x z := by
  have hy := y.proper
  simp only [meets, before] at *
  omega

theorem before_meets_before (x y z : Interval) (hxy : before x y) (hyz : meets y z) :
    before x z := by
  have hy := y.proper
  simp only [meets, before] at *
  omega

theorem meets_before_before (x y z : Interval) (hxy : meets x y) (hyz : before y z) :
    before x z := by
  have hy := y.proper
  simp only [meets, before] at *
  omega

/-- Anything inside an interval that is before `z` is itself before `z`. -/
theorem during_before_before (x y z : Interval) (hxy : during x y) (hyz : before y z) :
    before x z := by
  simp only [during, before] at *
  omega

/-! ## Ordering key

`timestamp_ordering_key` returns `(seconds, nanos)` with `0 ≤ nanos < 10⁹`.
Comparing such pairs lexicographically is comparing the instants counted in
nanoseconds, so the key embeds in `Int` in order and the theorems above apply
to intervals of keys. -/

/-- Lexicographic order on `(seconds, nanos)` is the order of the instants. -/
theorem key_lt_iff_instant_lt (s n s' n' : Int)
    (hn : 0 ≤ n ∧ n < 1000000000) (hn' : 0 ≤ n' ∧ n' < 1000000000) :
    (s < s' ∨ (s = s' ∧ n < n')) ↔ s * 1000000000 + n < s' * 1000000000 + n' := by
  omega

/-- Two keys are equal exactly when they are the same instant. -/
theorem key_eq_iff_instant_eq (s n s' n' : Int)
    (hn : 0 ≤ n ∧ n < 1000000000) (hn' : 0 ≤ n' ∧ n' < 1000000000) :
    (s = s' ∧ n = n') ↔ s * 1000000000 + n = s' * 1000000000 + n' := by
  omega

end Norn.Verification.OwlTime
