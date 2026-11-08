import Std

/- Source-only, uncompiled construction model; not a Rust refinement proof.
   Source pin: 08ea1512d734bc390201dd22abb682d8f4952757.
   See README.md for the structural and machine-correspondence obligations.
   No executable certificate checker substitutes for the max construction. -/
namespace Norn.DatalogStratification

structure Edge where
  body : Nat
  head : Nat
  negative : Bool
  deriving DecidableEq, Repr

def Edge.weight (e : Edge) : Nat := if e.negative then 1 else 0

abbrev Table := Nat → Option Nat

def empty : Table := fun _ => none

def put (table : Table) (key value : Nat) : Table :=
  fun node => if node = key then some value else table node

/-- Contract predicate dependencies using the supplied SCC owner map.
    Retaining both signs for a pair is harmless: max imposes the stronger one.
    Rust instead coalesces the pair and marks it negative if any occurrence is. -/
def quotient (owner : Nat → Nat) (edges : List Edge) : List Edge :=
  edges.map fun e => ⟨owner e.body, owner e.head, e.negative⟩

def incoming (edges : List Edge) (node : Nat) : List Edge :=
  edges.filter fun e => decide (e.head = node ∧ e.body ≠ node)

/-- Rust validate.rs:476–491: reject a negative dependency within one group.
    This does not reject an internal positive edge. -/
def internalOK : List Edge → Bool
  | [] => true
  | e :: rest =>
    if e.body = e.head ∧ e.negative = true then false else internalOK rest

/-- Rust validate.rs:498–509: left-to-right max accumulator, checked addition,
    and failed lookup. `limit` is the mathematical value of usize::MAX.
    none erases the diagnostic, never confers admission. -/
def foldMax (limit : Nat) (table : Table) : List Edge → Nat → Option Nat
  | [], acc => some acc
  | e :: rest, acc =>
    match table e.body with
    | none => none
    | some previous =>
      if previous + e.weight ≤ limit then
        foldMax limit table rest (max acc (previous + e.weight))
      else none

/-- Rust validate.rs:495–513: process the supplied component order once. -/
def assign (limit : Nat) (edges : List Edge) : List Nat → Table → Option Table
  | [], table => some table
  | node :: rest, table =>
    match foldMax limit table (incoming edges node) 0 with
    | none => none
    | some value => assign limit edges rest (put table node value)

/-- `budgetOK` abstracts successful metering of the entire operation. It is
    not a cost model. All budget refusals project to none; see README.md. -/
def construct (limit : Nat) (budgetOK : Bool) (edges : List Edge)
    (order : List Nat) : Option Table :=
  if budgetOK then
    if internalOK edges then assign limit edges order empty else none
  else none

def stratify (limit : Nat) (budgetOK : Bool) (owner : Nat → Nat)
    (edges : List Edge) (order : List Nat) : Option Table :=
  construct limit budgetOK (quotient owner edges) order

def Fresh (order : List Nat) (table : Table) : Prop :=
  ∀ node, node ∈ order → table node = none

/-- The fold actually constructs an upper bound on every incoming dependency;
    the inequalities here are conclusions, not certificate assumptions. -/
theorem foldMax_bounds (limit : Nat) (table : Table) (edges : List Edge)
    (acc result : Nat) (success : foldMax limit table edges acc = some result) :
    acc ≤ result ∧ ∀ e ∈ edges,
      ∃ previous, table e.body = some previous ∧ previous + e.weight ≤ result := by
  revert success
  induction edges generalizing acc result with
  | nil =>
    intro success
    simp only [foldMax, Option.some.injEq] at success
    subst result
    exact ⟨Nat.le_refl _, by simp⟩
  | cons e rest ih =>
    intro success
    cases lookup : table e.body with
    | none => simp [foldMax, lookup] at success
    | some previous =>
      by_cases fits : previous + e.weight ≤ limit
      · have tail : foldMax limit table rest (max acc (previous + e.weight)) =
            some result := by simpa [foldMax, lookup, fits] using success
        obtain ⟨grows, bounds⟩ := ih _ _ tail
        refine ⟨Nat.le_trans (Nat.le_max_left _ _) grows, ?_⟩
        intro edge member
        rcases List.mem_cons.mp member with same | later
        · subst edge
          exact ⟨previous, lookup, Nat.le_trans (Nat.le_max_right _ _) grows⟩
        · exact bounds edge later
      · simp [foldMax, lookup, fits] at success

/-- Leastness of this particular max fold, not merely existence of a solution.
    This is local leastness; no global minimum-stratum claim is made here. -/
theorem foldMax_least (limit : Nat) (table : Table) (edges : List Edge)
    (acc result bound : Nat) (success : foldMax limit table edges acc = some result)
    (initial : acc ≤ bound)
    (upper : ∀ e ∈ edges,
      ∃ previous, table e.body = some previous ∧ previous + e.weight ≤ bound) :
    result ≤ bound := by
  revert success initial upper
  induction edges generalizing acc result with
  | nil =>
    intro success initial upper
    simp only [foldMax, Option.some.injEq] at success
    subst result
    exact initial
  | cons e rest ih =>
    intro success initial upper
    cases lookup : table e.body with
    | none => simp [foldMax, lookup] at success
    | some previous =>
      by_cases fits : previous + e.weight ≤ limit
      · have tail : foldMax limit table rest (max acc (previous + e.weight)) =
            some result := by simpa [foldMax, lookup, fits] using success
        obtain ⟨stored, found, bounded⟩ := upper e (by simp)
        have same : stored = previous := by
          rw [lookup] at found
          exact (Option.some.inj found).symm
        subst stored
        apply ih _ _ tail
        · omega
        · intro edge member
          exact upper edge (List.mem_cons_of_mem e member)
      · simp [foldMax, lookup, fits] at success

theorem foldMax_machine_bound (limit : Nat) (table : Table) (edges : List Edge)
    (acc result : Nat) (success : foldMax limit table edges acc = some result)
    (initial : acc ≤ limit) : result ≤ limit := by
  revert success initial
  induction edges generalizing acc result with
  | nil =>
    intro success initial
    simp only [foldMax, Option.some.injEq] at success
    subst result
    exact initial
  | cons e rest ih =>
    intro success initial
    cases lookup : table e.body with
    | none => simp [foldMax, lookup] at success
    | some previous =>
      by_cases fits : previous + e.weight ≤ limit
      · apply ih (max acc (previous + e.weight)) result
        · simpa [foldMax, lookup, fits] using success
        · omega
      · simp [foldMax, lookup, fits] at success

theorem fresh_tail (node value : Nat) (rest : List Nat) (table : Table)
    (unique : (node :: rest).Nodup) (fresh : Fresh (node :: rest) table) :
    Fresh rest (put table node value) := by
  intro other member
  have different : other ≠ node := by
    intro same
    subst other
    exact (List.nodup_cons.mp unique).1 member
  simp [put, different, fresh other (List.mem_cons_of_mem node member)]

/-- Later iterations cannot overwrite an already constructed predecessor. -/
theorem assign_preserves (limit : Nat) (edges : List Edge) (order : List Nat)
    (table out : Table) (node value : Nat) (unique : order.Nodup)
    (fresh : Fresh order table) (success : assign limit edges order table = some out)
    (known : table node = some value) : out node = some value := by
  revert unique fresh success known
  induction order generalizing table out with
  | nil =>
    intro unique fresh success known
    simp only [assign, Option.some.injEq] at success
    subst out
    exact known
  | cons current rest ih =>
    intro unique fresh success known
    cases folded : foldMax limit table (incoming edges current) 0 with
    | none => simp [assign, folded] at success
    | some stratum =>
      have tail : assign limit edges rest (put table current stratum) = some out := by
        simpa [assign, folded] using success
      have different : node ≠ current := by
        intro same
        subst node
        have absent := fresh current (by simp)
        rw [known] at absent
        cases absent
      apply ih (put table current stratum) out (List.nodup_cons.mp unique).2
        (fresh_tail current stratum rest table unique fresh) tail
      simpa [put, different] using known

theorem assign_covers (limit : Nat) (edges : List Edge) (order : List Nat)
    (table out : Table) (unique : order.Nodup) (fresh : Fresh order table)
    (success : assign limit edges order table = some out) :
    ∀ node ∈ order, ∃ value, out node = some value := by
  revert unique fresh success
  induction order generalizing table out with
  | nil => simp
  | cons current rest ih =>
    intro unique fresh success
    cases folded : foldMax limit table (incoming edges current) 0 with
    | none => simp [assign, folded] at success
    | some stratum =>
      have tail : assign limit edges rest (put table current stratum) = some out := by
        simpa [assign, folded] using success
      have tailUnique := (List.nodup_cons.mp unique).2
      have tailFresh := fresh_tail current stratum rest table unique fresh
      intro node member
      rcases List.mem_cons.mp member with same | later
      · subst node
        exact ⟨stratum, assign_preserves limit edges rest _ out current stratum
          tailUnique tailFresh tail (by simp [put])⟩
      · exact ih _ out tailUnique tailFresh tail node later

/-- Cross-component edge bounds follow from the successful construction.
    A successful lookup supplies the predecessor; no desired bound or valid
    topological order is smuggled in as a premise. -/
theorem assign_cross_bounds (limit : Nat) (edges : List Edge) (order : List Nat)
    (table out : Table) (unique : order.Nodup) (fresh : Fresh order table)
    (success : assign limit edges order table = some out) :
    ∀ e ∈ edges, e.head ∈ order → e.body ≠ e.head →
      ∃ body head, out e.body = some body ∧ out e.head = some head ∧
        body + e.weight ≤ head := by
  revert unique fresh success
  induction order generalizing table out with
  | nil => simp
  | cons current rest ih =>
    intro unique fresh success
    cases folded : foldMax limit table (incoming edges current) 0 with
    | none => simp [assign, folded] at success
    | some stratum =>
      have tail : assign limit edges rest (put table current stratum) = some out := by
        simpa [assign, folded] using success
      have tailUnique := (List.nodup_cons.mp unique).2
      have tailFresh := fresh_tail current stratum rest table unique fresh
      intro e member scheduled cross
      rcases List.mem_cons.mp scheduled with currentHead | later
      · have different : e.body ≠ current := by simpa only [currentHead] using cross
        have inFold : e ∈ incoming edges current := by
          simp [incoming, member, currentHead, different]
        obtain ⟨previous, lookup, bound⟩ :=
          (foldMax_bounds limit table _ 0 stratum folded).2 e inFold
        have bodyKnown : put table current stratum e.body = some previous := by
          simpa [put, different] using lookup
        have bodyFinal := assign_preserves limit edges rest _ out e.body previous
          tailUnique tailFresh tail bodyKnown
        have headFinal := assign_preserves limit edges rest _ out current stratum
          tailUnique tailFresh tail (by simp [put])
        refine ⟨previous, stratum, bodyFinal, ?_, bound⟩
        simpa [currentHead] using headFinal
      · exact ih _ out tailUnique tailFresh tail e member later cross

theorem internalOK_positive (edges : List Edge) (accepted : internalOK edges = true) :
    ∀ e ∈ edges, e.body = e.head → e.negative = false := by
  revert accepted
  induction edges with
  | nil => simp
  | cons first rest ih =>
    intro accepted
    by_cases bad : first.body = first.head ∧ first.negative = true
    · simp [internalOK, bad] at accepted
    · have tail : internalOK rest = true := by
        simpa [internalOK, bad] using accepted
      intro e member same
      rcases List.mem_cons.mp member with equal | later
      · subst e
        cases sign : first.negative with
        | false => rfl
        | true => exact False.elim (bad ⟨same, sign⟩)
      · exact ih tail e later same

def rank (table : Table) (node : Nat) : Nat := (table node).getD 0

def Law (edges : List Edge) (stratum : Nat → Nat) : Prop :=
  ∀ e ∈ edges, stratum e.body + e.weight ≤ stratum e.head

/-- The default in rank is never used for a component in a successful order. -/
theorem construct_covers (limit : Nat) (budgetOK : Bool) (edges : List Edge)
    (order : List Nat) (out : Table) (unique : order.Nodup)
    (success : construct limit budgetOK edges order = some out) :
    ∀ node ∈ order, ∃ value, out node = some value := by
  cases budgetOK with
  | false => simp [construct] at success
  | true =>
    cases checked : internalOK edges with
    | false => simp [construct, checked] at success
    | true =>
      apply assign_covers limit edges order empty out unique
      · intro node member
        rfl
      · simpa [construct, checked] using success

/-- Conditional model theorem. The only scheduling premises are structural
    uniqueness and head coverage. Failed predecessor lookup cannot succeed. -/
theorem construct_sound (limit : Nat) (budgetOK : Bool) (edges : List Edge)
    (order : List Nat) (out : Table) (unique : order.Nodup)
    (covers : ∀ e ∈ edges, e.head ∈ order)
    (success : construct limit budgetOK edges order = some out) :
    Law edges (rank out) := by
  cases budgetOK with
  | false => simp [construct] at success
  | true =>
    cases checked : internalOK edges with
    | false => simp [construct, checked] at success
    | true =>
      have built : assign limit edges order empty = some out := by
        simpa [construct, checked] using success
      have fresh : Fresh order empty := by intro node member; rfl
      intro e member
      by_cases same : e.body = e.head
      · have positive := internalOK_positive edges checked e member same
        simp [same, Edge.weight, positive]
      · obtain ⟨body, head, bodyValue, headValue, bound⟩ :=
          assign_cross_bounds limit edges order empty out unique fresh built
            e member (covers e member) same
        simpa [rank, bodyValue, headValue] using bound

/-- Lift component construction back to signed predicate dependencies. SCC
    correctness is deliberately not assumed or claimed by this theorem. -/
theorem stratify_sound (limit : Nat) (budgetOK : Bool) (owner : Nat → Nat)
    (edges : List Edge) (order : List Nat) (out : Table) (unique : order.Nodup)
    (covers : ∀ e ∈ edges, owner e.head ∈ order)
    (success : stratify limit budgetOK owner edges order = some out) :
    Law edges (fun node => rank out (owner node)) := by
  have componentCover : ∀ e ∈ quotient owner edges, e.head ∈ order := by
    intro e member
    obtain ⟨original, originalMember, mappedEqual⟩ := List.mem_map.mp member
    exact (congrArg (fun edge : Edge => edge.head ∈ order) mappedEqual).mp
      (covers original originalMember)
  have componentLaw := construct_sound limit budgetOK (quotient owner edges)
    order out unique componentCover success
  intro e member
  have mapped : (⟨owner e.body, owner e.head, e.negative⟩ : Edge) ∈
      quotient owner edges := List.mem_map.mpr ⟨e, member, rfl⟩
  exact componentLaw _ mapped

theorem positive_edge (edges : List Edge) (stratum : Nat → Nat) (law : Law edges stratum)
    (e : Edge) (member : e ∈ edges) (positive : e.negative = false) :
    stratum e.body ≤ stratum e.head := by
  simpa [Edge.weight, positive] using law e member

theorem negative_edge (edges : List Edge) (stratum : Nat → Nat) (law : Law edges stratum)
    (e : Edge) (member : e ∈ edges) (negative : e.negative = true) :
    stratum e.body < stratum e.head := by
  have bound := law e member
  simp [Edge.weight, negative] at bound
  omega

/-- A path records the sum of negative-edge weights, including zero for refl. -/
inductive Path (edges : List Edge) : Nat → Nat → Nat → Prop where
  | refl (node : Nat) : Path edges node node 0
  | cons (e : Edge) (member : e ∈ edges) {finish weight : Nat}
      (tail : Path edges e.head finish weight) :
      Path edges e.body finish (e.weight + weight)

theorem path_weight_monotone (edges : List Edge) (stratum : Nat → Nat)
    (law : Law edges stratum) {start finish weight : Nat}
    (path : Path edges start finish weight) :
    stratum start + weight ≤ stratum finish := by
  induction path with
  | refl node => omega
  | cons e member tail ih =>
    have edgeBound := law e member
    omega

theorem closed_path_weight_zero (edges : List Edge) (stratum : Nat → Nat)
    (law : Law edges stratum) (node weight : Nat) (path : Path edges node node weight) :
    weight = 0 := by
  have bound := path_weight_monotone edges stratum law path
  omega

theorem negative_edge_no_return (edges : List Edge) (stratum : Nat → Nat)
    (law : Law edges stratum) (e : Edge) (member : e ∈ edges)
    (negative : e.negative = true) (weight : Nat)
    (back : Path edges e.head e.body weight) : False := by
  have strict := negative_edge edges stratum law e member negative
  have backBound := path_weight_monotone edges stratum law back
  omega

theorem successful_no_negative_cycle (limit : Nat) (budgetOK : Bool) (owner : Nat → Nat)
    (edges : List Edge) (order : List Nat) (out : Table) (unique : order.Nodup)
    (covers : ∀ e ∈ edges, owner e.head ∈ order)
    (success : stratify limit budgetOK owner edges order = some out)
    (e : Edge) (member : e ∈ edges) (negative : e.negative = true)
    (weight : Nat) (back : Path edges e.head e.body weight) : False := by
  exact negative_edge_no_return edges (fun node => rank out (owner node))
    (stratify_sound limit budgetOK owner edges order out unique covers success)
    e member negative weight back

/-- Structural specifications for future graph correspondence, NOT axioms and
    NOT additional production admission conditions. Neither is certified here. -/
def SCCPartition (nodes : List Nat) (edges : List Edge) (owner : Nat → Nat) : Prop :=
  (∀ e ∈ edges, e.body ∈ nodes ∧ e.head ∈ nodes) ∧
  ∀ a ∈ nodes, ∀ b ∈ nodes, owner a = owner b ↔
    (∃ weight, Path edges a b weight) ∧ (∃ weight, Path edges b a weight)

def Before (a b : Nat) (order : List Nat) : Prop :=
  ∃ initial middle suffix, order = initial ++ (a :: (middle ++ (b :: suffix)))

def Topology (edges : List Edge) (order : List Nat) : Prop :=
  order.Nodup ∧ (∀ e ∈ edges, e.body ∈ order ∧ e.head ∈ order) ∧
  ∀ e ∈ edges, e.body ≠ e.head → Before e.body e.head order

theorem budget_abort (limit : Nat) (edges : List Edge) (order : List Nat) :
    construct limit false edges order = none := by rfl

theorem negative_self_refused (limit : Nat) :
    construct limit true [⟨0, 0, true⟩] [0] = none := by rfl

theorem missing_predecessor_refused (limit : Nat) :
    construct limit true [⟨0, 1, false⟩] [1] = none := by rfl

theorem overflowing_increment_refused :
    foldMax 0 (put empty 0 0) [⟨0, 1, true⟩] 0 = none := by rfl

/-- Constructive witness: a two-predicate positive cycle succeeds at stratum 0. -/
theorem positive_cycle_admitted :
    stratify 0 true (fun _ => 0) [⟨0, 1, false⟩, ⟨1, 0, false⟩] [0] =
      some (put empty 0 0) := by rfl

end Norn.DatalogStratification
