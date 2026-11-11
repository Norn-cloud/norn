import Mathlib

/-!
# Data exchange and chase certificates (#718 U7, path-mapping preservation)

A path mapping `F : target → source` lowers to *mapping rules*. Each is a full
tuple-generating dependency (TGD) with one head atom, which pulls source data
into the target (`pull`). Both catalogs lower their path equations and
functional links to TGDs and equality-generating dependencies (EGDs). The
mapping **preserves** a target EGD when every source instance satisfying the
source theory pulls back to a target instance satisfying that EGD
(`Preserves`).

The current admission searches for counterexamples within a bound, and its
finding no counterexample is only bounded evidence. This module states what a
**certificate** for preservation is, and proves it sound:

1. *Unfolding.* A target atom in the pulled-back instance was produced by some
   mapping rule with that relation. For every choice of producing rule per body
   atom (`cases`), the certificate starts from the canonical instance: the
   chosen rules' bodies, kept apart, with each head identified with its target
   atom.
2. *Chase trace.* A sequence of source TGD and EGD steps, each applied at an
   explicit assignment (a homomorphism of the step's body into the current
   instance). An EGD step merges two elements; a TGD step adds its head with
   fresh elements.
3. The trace must end with the target EGD's two variables merged.

`check` is that certificate check, an executable specification. `check_sound`
proves that a certificate that checks implies `Preserves` for every source
instance over every domain. The proof is the standard soundness of the chase:
a homomorphism from the chase instance into any model is maintained step by
step.
-/

namespace Norn.Sem.Exchange

/-- An atom over variables. -/
structure Atom where
  rel : ℕ
  args : List ℕ
  deriving DecidableEq, Repr

/-- A dependency over variables `0, 1, …`. -/
inductive Dep where
  | tgd (body head : List Atom)
  | egd (body : List Atom) (left right : ℕ)
  deriving DecidableEq, Repr

/-- An instance over a domain: the tuples each relation holds. -/
def Inst (D : Type) := ℕ → List D → Prop

/-- An atom holds in `I` under the assignment `g`. -/
def Atom.Holds {D : Type} (I : Inst D) (g : ℕ → D) (a : Atom) : Prop := I a.rel (a.args.map g)

/-- Every atom holds. -/
def AllHold {D : Type} (I : Inst D) (g : ℕ → D) (as : List Atom) : Prop := ∀ a ∈ as, a.Holds I g

/-- The variables of a list of atoms. -/
def vars (as : List Atom) : List ℕ := as.flatMap Atom.args

/-- Satisfaction. A TGD's head variables that are not in its body are
    existential. -/
def Dep.Sat {D : Type} (I : Inst D) : Dep → Prop
  | .tgd body head => ∀ g, AllHold I g body →
      ∃ g' : ℕ → D, (∀ v ∈ vars body, g' v = g v) ∧ AllHold I g' head
  | .egd body l r => ∀ g, AllHold I g body → g l = g r

/-- A model of a theory. -/
def Model {D : Type} (I : Inst D) (theory : List Dep) : Prop := ∀ d ∈ theory, d.Sat I

/-- A mapping rule: a full TGD with one head atom. -/
structure Rule where
  body : List Atom
  head : Atom
  deriving DecidableEq, Repr

/-- The target instance a source instance pulls back to: exactly the heads of
    the mapping rules that fire. -/
def pull {D : Type} (rules : List Rule) (I : Inst D) : Inst D := fun r t =>
  ∃ m ∈ rules, m.head.rel = r ∧ ∃ g, AllHold I g m.body ∧ t = m.head.args.map g

/-- The mapping preserves the target EGD `body → left = right`. -/
def Preserves (theory : List Dep) (rules : List Rule) (body : List Atom) (l r : ℕ) : Prop :=
  ∀ {D : Type} (I : Inst D), Model I theory → (Dep.egd body l r).Sat (pull rules I)

/-! ## The chase state and its steps -/

/-- A chase state over elements `0 … next - 1`. `rep` maps each element to
    its class representative. -/
structure State where
  atoms : List (ℕ × List ℕ)
  rep : ℕ → ℕ
  next : ℕ

/-- The state holds an atom with these elements, up to merged classes. -/
def State.has (s : State) (r : ℕ) (es : List ℕ) : Bool :=
  s.atoms.any fun a => decide (a.1 = r) && decide (a.2.map s.rep = es.map s.rep)

/-- Merge the classes of `a` and `b`. -/
def State.merge (s : State) (a b : ℕ) : State :=
  { s with rep := fun x => if s.rep x = s.rep b then s.rep a else s.rep x }

/-- The value of variable `v` under an assignment list (`0` when missing). -/
def at' (asg : List ℕ) (v : ℕ) : ℕ := asg.getD v 0

/-- One more than the largest variable of the atoms. -/
def width (as : List Atom) : ℕ := (vars as).foldr max 0 + 1

/-- A step: apply theory dependency `dep` at the assignment `asg` (one
    element per body variable). -/
structure Step where
  dep : ℕ
  asg : List ℕ
  deriving DecidableEq, Repr

/-- The assignment maps every body variable to an existing element, and every
    body atom is in the state. -/
def bodyMatches (s : State) (body : List Atom) (asg : List ℕ) : Bool :=
  (vars body).all (fun v => decide (at' asg v < s.next)) &&
    body.all fun a => s.has a.rel (a.args.map (at' asg))

/-- A TGD step's element for head variable `v`: the assignment's on a body
    variable, a fresh one otherwise. -/
def headElem (s : State) (body : List Atom) (asg : List ℕ) (v : ℕ) : ℕ :=
  if v ∈ vars body then at' asg v else s.next + v

/-- Apply one step, or refuse. -/
def apply (theory : List Dep) (s : State) (st : Step) : Option State :=
  match theory[st.dep]? with
  | some (.egd body l r) =>
      if bodyMatches s body st.asg && decide (at' st.asg l < s.next) &&
          decide (at' st.asg r < s.next) then
        some (s.merge (at' st.asg l) (at' st.asg r))
      else none
  | some (.tgd body head) =>
      if bodyMatches s body st.asg then
        some { atoms := s.atoms ++ head.map (fun a => (a.rel, a.args.map (headElem s body st.asg)))
               rep := s.rep
               next := s.next + width head }
      else none
  | none => none

/-- Run a trace. -/
def run (theory : List Dep) : State → List Step → Option State
  | s, [] => some s
  | s, st :: rest => (apply theory s st).bind fun s' => run theory s' rest

/-! ## The canonical instance of one unfolding case -/

/-- The element of case-`i` variable `v`, after the target variables `0 … nB-1`. -/
def caseElem (nB w i v : ℕ) : ℕ := nB + i * w + v

/-- The width of every rule's variables. -/
def ruleWidth (rules : List Rule) : ℕ :=
  rules.foldr (fun m acc => max (width (m.head :: m.body)) acc) 1

/-- A placeholder rule for out-of-range indices (never chosen by `cases`). -/
def noRule : Rule := ⟨[], ⟨0, []⟩⟩

/-- The rule a choice picks for body atom `i`. -/
def chosen (rules : List Rule) (choice : List ℕ) (i : ℕ) : Rule := rules.getD (choice.getD i 0) noRule

/-- Body atom `i`. -/
def atomAt (body : List Atom) (i : ℕ) : Atom := body.getD i ⟨0, []⟩

/-- The canonical state of the case that produces body atom `i` by rule
    `choice[i]`: those rules' bodies, kept apart (case `i`'s variable `v` is
    element `caseElem`), with each head variable merged into its target
    variable (target variables are elements `0 … width body - 1`). -/
def initial (rules : List Rule) (body : List Atom) (choice : List ℕ) : State :=
  let nB := width body
  let w := ruleWidth rules
  let k := choice.length
  let atoms := (List.range k).flatMap fun i =>
    (chosen rules choice i).body.map fun a => (a.rel, a.args.map (caseElem nB w i))
  let pairs := (List.range k).flatMap fun i =>
    ((chosen rules choice i).head.args.zip (atomAt body i).args).map fun p =>
      (caseElem nB w i p.1, p.2)
  pairs.foldl (fun s p => s.merge p.1 p.2) { atoms := atoms, rep := id, next := nB + k * w }

/-- Every rule choice whose heads have the body atoms' relations and arities. -/
def cases (rules : List Rule) : List Atom → List (List ℕ)
  | [] => [[]]
  | b :: bs =>
      ((List.range rules.length).filter fun m =>
        match rules[m]? with
        | some r => decide (r.head.rel = b.rel ∧ r.head.args.length = b.args.length)
        | none => false).flatMap fun m => (cases rules bs).map (m :: ·)

/-- A certificate: one trace per case, in `cases` order. -/
def check (theory : List Dep) (rules : List Rule) (body : List Atom) (l r : ℕ)
    (traces : List (List Step)) : Bool :=
  let cs := cases rules body
  decide (l ∈ vars body) && decide (r ∈ vars body) && decide (cs.length = traces.length) &&
    (cs.zip traces).all fun (c, t) =>
      match run theory (initial rules body c) t with
      | some s => decide (s.rep l = s.rep r)
      | none => false

/-! ## Soundness -/

theorem mem_vars {as : List Atom} {a : Atom} (ha : a ∈ as) {v : ℕ} (hv : v ∈ a.args) :
    v ∈ vars as :=
  List.mem_flatMap.mpr ⟨a, ha, hv⟩

theorem le_foldr_max : ∀ {l : List ℕ} {v : ℕ}, v ∈ l → v ≤ l.foldr max 0
  | [], _, h => by simp at h
  | x :: xs, v, h => by
    simp only [List.foldr_cons]
    rcases List.mem_cons.mp h with rfl | h
    · exact le_max_left _ _
    · exact le_trans (le_foldr_max h) (le_max_right _ _)

theorem lt_width {as : List Atom} {v : ℕ} (hv : v ∈ vars as) : v < width as :=
  Nat.lt_succ_of_le (le_foldr_max hv)

theorem width_le_ruleWidth : ∀ {rules : List Rule} {m : Rule}, m ∈ rules →
    width (m.head :: m.body) ≤ ruleWidth rules
  | [], _, h => by simp at h
  | x :: xs, m, h => by
    simp only [ruleWidth, List.foldr_cons]
    rcases List.mem_cons.mp h with rfl | h
    · exact le_max_left _ _
    · exact le_trans (width_le_ruleWidth h) (le_max_right _ _)

theorem ruleWidth_pos (rules : List Rule) : 0 < ruleWidth rules := by
  cases rules with
  | nil => simp [ruleWidth]
  | cons x xs => exact lt_of_lt_of_le (by simp [width]) (le_max_left (width (x.head :: x.body)) _)

section Chase

variable {D : Type} (I : Inst D)

/-- The chase invariant: `h` maps every atom of the state into `I`, respects
    merged classes, and every element in use is below `next`. -/
structure Good (h : ℕ → D) (s : State) : Prop where
  atoms : ∀ a ∈ s.atoms, I a.1 (a.2.map h)
  resp : ∀ x y, s.rep x = s.rep y → h x = h y
  repLt : ∀ x, x < s.next → s.rep x < s.next
  repId : ∀ x, s.next ≤ x → s.rep x = x
  elems : ∀ a ∈ s.atoms, ∀ e ∈ a.2, e < s.next

omit I in
theorem map_resp {rep : ℕ → ℕ} {h : ℕ → D} (hr : ∀ x y, rep x = rep y → h x = h y) :
    ∀ {xs ys : List ℕ}, xs.map rep = ys.map rep → xs.map h = ys.map h
  | [], [], _ => rfl
  | x :: xs, y :: ys, he => by
    simp only [List.map_cons, List.cons.injEq] at he ⊢
    exact ⟨hr x y he.1, map_resp hr he.2⟩
  | [], _ :: _, he => by simp at he
  | _ :: _, [], he => by simp at he

variable {I}

theorem Good.has {h : ℕ → D} {s : State} (hg : Good I h s) {r : ℕ} {es : List ℕ}
    (hh : s.has r es = true) : I r (es.map h) := by
  unfold State.has at hh
  rw [List.any_eq_true] at hh
  obtain ⟨a, ha, hc⟩ := hh
  simp only [Bool.and_eq_true, decide_eq_true_eq] at hc
  obtain ⟨hr, hm⟩ := hc
  have hI := hg.atoms a ha
  rw [map_resp hg.resp hm, hr] at hI
  exact hI

theorem Good.merge {h : ℕ → D} {s : State} (hg : Good I h s) {a b : ℕ} (ha : a < s.next)
    (hb : b < s.next) (hab : h a = h b) : Good I h (s.merge a b) where
  atoms := hg.atoms
  elems := hg.elems
  resp x y hxy := by
    simp only [State.merge] at hxy
    split_ifs at hxy with hx hy hy
    · exact (hg.resp x b hx).trans (hg.resp y b hy).symm
    · exact (hg.resp x b hx).trans (hab.symm.trans (hg.resp a y hxy))
    · exact ((hg.resp y b hy).trans (hab.symm.trans (hg.resp a x hxy.symm))).symm
    · exact hg.resp x y hxy
  repLt x hx := by
    simp only [State.merge]
    split_ifs
    · exact hg.repLt a ha
    · exact hg.repLt x hx
  repId x hx := by
    change s.next ≤ x at hx
    simp only [State.merge]
    have hrx := hg.repId x hx
    have hrb := hg.repLt b hb
    rw [if_neg (by omega)]
    exact hrx

/-- The body atoms of a step hold under `h` composed with its assignment. -/
theorem Good.body {h : ℕ → D} {s : State} (hg : Good I h s) {body : List Atom} {asg : List ℕ}
    (hm : bodyMatches s body asg = true) :
    AllHold I (fun v => h (at' asg v)) body ∧ ∀ v ∈ vars body, at' asg v < s.next := by
  unfold bodyMatches at hm
  rw [Bool.and_eq_true, List.all_eq_true, List.all_eq_true] at hm
  refine ⟨fun a ha => ?_, fun v hv => by simpa using hm.1 v hv⟩
  have := hg.has (hm.2 a ha)
  rw [List.map_map] at this
  exact this

theorem Good.step {theory : List Dep} (hmod : Model I theory) {h : ℕ → D} {s s' : State}
    {st : Step} (hg : Good I h s) (ha : apply theory s st = some s') :
    ∃ h' : ℕ → D, Good I h' s' ∧ s.next ≤ s'.next ∧ ∀ x < s.next, h' x = h x := by
  unfold Norn.Sem.Exchange.apply at ha
  cases hd : theory[st.dep]? with
  | none => simp [hd] at ha
  | some d =>
    have hmem : d ∈ theory := List.mem_of_getElem? hd
    cases d with
    | egd body l r =>
      simp only [hd] at ha
      split_ifs at ha with hc
      · cases ha
        simp only [Bool.and_eq_true, decide_eq_true_eq] at hc
        obtain ⟨⟨hb, hl⟩, hr⟩ := hc
        have hbody := (hg.body hb).1
        have heq := hmod _ hmem _ hbody
        exact ⟨h, hg.merge hl hr heq, le_rfl, fun _ _ => rfl⟩
    | tgd body head =>
      simp only [hd] at ha
      split_ifs at ha with hc
      · cases ha
        obtain ⟨hbody, hlt⟩ := hg.body hc
        obtain ⟨g', hg'eq, hhead⟩ := hmod _ hmem _ hbody
        let h' : ℕ → D := fun x => if x < s.next then h x else g' (x - s.next)
        have hhe : ∀ v ∈ vars head, h' (headElem s body st.asg v) = g' v := by
          intro v _
          unfold headElem
          split_ifs with hv
          · simp only [h', if_pos (hlt v hv)]
            exact (hg'eq v hv).symm
          · simp [h']
        have hhe_lt : ∀ v ∈ vars head, headElem s body st.asg v < s.next + width head := by
          intro v hvh
          unfold headElem
          split_ifs with hv
          · exact lt_of_lt_of_le (hlt v hv) (Nat.le_add_right _ _)
          · have := lt_width hvh
            omega
        refine ⟨h', ⟨?_, ?_, ?_, ?_, ?_⟩, Nat.le_add_right _ _, fun x hx => by simp [h', hx]⟩
        · intro a ha
          rcases List.mem_append.mp ha with ha | ha
          · have hv := hg.atoms a ha
            have : a.2.map h' = a.2.map h :=
              List.map_congr_left fun e he => by simp [h', hg.elems a ha e he]
            rw [this]
            exact hv
          · obtain ⟨b, hb, rfl⟩ := List.mem_map.mp ha
            have : (b.args.map (headElem s body st.asg)).map h' = b.args.map g' := by
              rw [List.map_map]
              exact List.map_congr_left fun v hv => hhe v (mem_vars hb hv)
            rw [this]
            exact hhead b hb
        · intro x y hxy
          change s.rep x = s.rep y at hxy
          by_cases hx : x < s.next
          · by_cases hy : y < s.next
            · simp only [h', if_pos hx, if_pos hy]
              exact hg.resp x y hxy
            · have := hg.repLt x hx
              rw [hg.repId y (by omega)] at hxy
              omega
          · rw [hg.repId x (by omega)] at hxy
            by_cases hy : y < s.next
            · have := hg.repLt y hy
              omega
            · rw [hg.repId y (by omega)] at hxy
              rw [hxy]
        · intro x hx
          by_cases hxs : x < s.next
          · exact lt_of_lt_of_le (hg.repLt x hxs) (Nat.le_add_right _ _)
          · simp only
            rw [hg.repId x (by omega)]
            exact hx
        · intro x hx
          exact hg.repId x (le_trans (Nat.le_add_right _ _) hx)
        · intro a ha e he
          rcases List.mem_append.mp ha with ha | ha
          · exact lt_of_lt_of_le (hg.elems a ha e he) (Nat.le_add_right _ _)
          · obtain ⟨b, hb, rfl⟩ := List.mem_map.mp ha
            obtain ⟨v, hv, rfl⟩ := List.mem_map.mp he
            exact hhe_lt v (mem_vars hb hv)

theorem Good.run {theory : List Dep} (hmod : Model I theory) :
    ∀ (t : List Step) {h : ℕ → D} {s s' : State}, Good I h s → run theory s t = some s' →
      ∃ h' : ℕ → D, Good I h' s' ∧ ∀ x < s.next, h' x = h x
  | [], h, s, s', hg, hr => by
    cases hr
    exact ⟨h, hg, fun _ _ => rfl⟩
  | st :: rest, h, s, s', hg, hr => by
    change (apply theory s st).bind (fun s' => Norn.Sem.Exchange.run theory s' rest) = some s' at hr
    cases ha : apply theory s st with
    | none => simp [ha] at hr
    | some s1 =>
      simp only [ha, Option.bind_some] at hr
      obtain ⟨h1, hg1, hle, hag1⟩ := hg.step hmod ha
      obtain ⟨h2, hg2, hag2⟩ := Good.run hmod rest hg1 hr
      exact ⟨h2, hg2, fun x hx => (hag2 x (lt_of_lt_of_le hx hle)).trans (hag1 x hx)⟩

theorem Good.foldMerge {h : ℕ → D} :
    ∀ (ps : List (ℕ × ℕ)) {s : State}, Good I h s →
      (∀ p ∈ ps, p.1 < s.next ∧ p.2 < s.next ∧ h p.1 = h p.2) →
      Good I h (ps.foldl (fun s p => s.merge p.1 p.2) s) ∧
        (ps.foldl (fun s p => s.merge p.1 p.2) s).next = s.next
  | [], _, hg, _ => ⟨hg, rfl⟩
  | p :: ps, s, hg, hps => by
    simp only [List.foldl_cons]
    obtain ⟨h1, h2, h3⟩ := hps p (List.mem_cons_self ..)
    have hg' := hg.merge h1 h2 h3
    have hnext : (s.merge p.1 p.2).next = s.next := rfl
    obtain ⟨hg'', hn⟩ := Good.foldMerge ps hg' (fun q hq => by
      rw [hnext]
      exact hps q (List.mem_cons_of_mem _ hq))
    exact ⟨hg'', hn.trans hnext⟩

end Chase

/-! ## Cases cover every producing rule -/

theorem cases_complete (rules : List Rule) :
    ∀ (body : List Atom) (choice : List ℕ), choice.length = body.length →
      (∀ i, i < body.length → ∃ r, rules[choice.getD i 0]? = some r ∧
        r.head.rel = (atomAt body i).rel ∧ r.head.args.length = (atomAt body i).args.length) →
      choice ∈ cases rules body
  | [], choice, hl, _ => by
    have : choice = [] := List.eq_nil_of_length_eq_zero hl
    subst this
    simp [cases]
  | b :: bs, choice, hl, hall => by
    cases choice with
    | nil => simp at hl
    | cons m ms =>
      simp only [List.length_cons, Nat.add_right_cancel_iff] at hl
      obtain ⟨r0, hr0, hrel, harity⟩ := hall 0 (by simp)
      simp only [List.getD_cons_zero, atomAt] at hr0 hrel harity
      have hm : m < rules.length := by
        by_contra hc
        rw [List.getElem?_eq_none (by omega)] at hr0
        cases hr0
      simp only [cases, List.mem_flatMap, List.mem_filter, List.mem_range, List.mem_map]
      refine ⟨m, ⟨hm, ?_⟩, ms, cases_complete rules bs ms hl (fun i hi => ?_), rfl⟩
      · rw [hr0]
        simp [hrel, harity]
      · obtain ⟨r, hr, h1, h2⟩ := hall (i + 1) (by simp; omega)
        simp only [List.getD_cons_succ, atomAt] at hr h1 h2
        exact ⟨r, hr, by simpa [atomAt] using h1, by simpa [atomAt] using h2⟩

theorem cases_length (rules : List Rule) :
    ∀ (body : List Atom) (choice : List ℕ), choice ∈ cases rules body →
      choice.length = body.length
  | [], choice, h => by simp [cases] at h; simp [h]
  | b :: bs, choice, h => by
    simp only [cases, List.mem_flatMap, List.mem_map] at h
    obtain ⟨m, -, ms, hms, rfl⟩ := h
    simp [cases_length rules bs ms hms]

/-! ## The initial state is a model of the unfolding -/

section Initial

variable {D : Type} {I : Inst D}

theorem caseElem_decode {nB w i v : ℕ} (hw : 0 < w) (hv : v < w) :
    (caseElem nB w i v - nB) / w = i ∧ (caseElem nB w i v - nB) % w = v := by
  unfold caseElem
  have : nB + i * w + v - nB = v + w * i := by rw [Nat.mul_comm]; omega
  rw [this, Nat.add_mul_div_left _ _ hw, Nat.div_eq_of_lt hv, Nat.add_mul_mod_self_left,
    Nat.mod_eq_of_lt hv]
  simp

theorem caseElem_lt {nB w i v k : ℕ} (hv : v < w) (hi : i < k) :
    caseElem nB w i v < nB + k * w := by
  unfold caseElem
  have : i * w + w ≤ k * w := by
    have := Nat.mul_le_mul_right w (show i + 1 ≤ k by omega)
    rw [Nat.add_mul, Nat.one_mul] at this
    exact this
  omega

theorem zip_map_eq {α : Type} {f : ℕ → α} {g : ℕ → α} :
    ∀ {xs ys : List ℕ} {p : ℕ × ℕ}, xs.map f = ys.map g → p ∈ xs.zip ys → f p.1 = g p.2
  | [], _, _, _, hp => by simp at hp
  | _ :: _, [], _, _, hp => by simp at hp
  | x :: xs, y :: ys, p, he, hp => by
    simp only [List.map_cons, List.cons.injEq] at he
    rcases List.mem_cons.mp hp with rfl | hp
    · exact he.1
    · exact zip_map_eq he.2 hp

end Initial

/-- **Soundness of chase certificates.** If `check` accepts, the mapping
    preserves the target EGD: for every domain and every source instance that
    satisfies the source theory, the pulled-back target instance satisfies it. -/
theorem check_sound (theory : List Dep) (rules : List Rule) (body : List Atom) (l r : ℕ)
    (traces : List (List Step)) (hc : check theory rules body l r traces = true) :
    Preserves theory rules body l r := by
  intro D I hmod g hB
  unfold check at hc
  simp only [Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true] at hc
  obtain ⟨⟨⟨hl, hr⟩, hlen⟩, hall⟩ := hc
  -- Each body atom holds in the pulled-back instance: pick its producing rule.
  have hprod : ∀ i, ∃ m : ℕ, ∃ G : ℕ → D, ∀ hi : i < body.length,
      ∃ hm : m < rules.length,
        (rules[m]'hm).head.rel = (body[i]'hi).rel ∧
        AllHold I G (rules[m]'hm).body ∧
        (body[i]'hi).args.map g = (rules[m]'hm).head.args.map G := by
    intro i
    by_cases hi : i < body.length
    · obtain ⟨m, hm, hrel, G, hG, hargs⟩ := hB (body[i]'hi) (List.getElem_mem hi)
      obtain ⟨j, hj, rfl⟩ := List.mem_iff_getElem.mp hm
      exact ⟨j, G, fun _ => ⟨hj, hrel, hG, hargs⟩⟩
    · exact ⟨0, fun _ => g 0, fun h => absurd h hi⟩
  choose M G hMG using hprod
  let choice := (List.range body.length).map M
  have hchoice_len : choice.length = body.length := by simp [choice]
  have hgetD : ∀ i, i < body.length → choice.getD i 0 = M i := by
    intro i hi
    simp [choice, List.getD_eq_getElem?_getD, hi]
  have hatom : ∀ i (hi : i < body.length), atomAt body i = body[i]'hi := by
    intro i hi
    simp [atomAt, List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hi]
  have hcase : choice ∈ cases rules body := by
    refine cases_complete rules body choice hchoice_len (fun i hi => ?_)
    obtain ⟨hm, hrel, -, hargs⟩ := hMG i hi
    refine ⟨rules[M i]'hm, by rw [hgetD i hi, List.getElem?_eq_getElem hm], ?_, ?_⟩
    · rw [hatom i hi]
      exact hrel
    · rw [hatom i hi]
      have := congrArg List.length hargs
      simp only [List.length_map] at this
      exact this.symm
  obtain ⟨j, hj, hcj⟩ := List.mem_iff_getElem.mp hcase
  have hj' : j < traces.length := by omega
  have hz := hall (((cases rules body).zip traces)[j]'(by simp; omega))
    (List.getElem_mem _)
  simp only [List.getElem_zip, hcj] at hz
  cases hrun : run theory (initial rules body choice) (traces[j]'hj') with
  | none => simp [hrun] at hz
  | some s =>
    simp only [hrun, decide_eq_true_eq] at hz
    -- The homomorphism of the canonical state into I.
    let nB := width body
    let w := ruleWidth rules
    have hw := ruleWidth_pos rules
    let h : ℕ → D := fun e => if e < nB then g e else G ((e - nB) / w) ((e - nB) % w)
    have hchosen : ∀ i (hi : i < body.length),
        chosen rules choice i = rules[M i]'(hMG i hi).1 := by
      intro i hi
      simp only [chosen]
      rw [hgetD i hi]
      simp [List.getD_eq_getElem?_getD, List.getElem?_eq_getElem (hMG i hi).1]
    have hcaseh : ∀ i (hi : i < body.length) v, v ∈ vars ((chosen rules choice i).head ::
        (chosen rules choice i).body) → h (caseElem nB w i v) = G i v := by
      intro i hi v hv
      have hvw : v < w := lt_of_lt_of_le (lt_width hv) (by
        rw [hchosen i hi]
        exact width_le_ruleWidth (List.getElem_mem _))
      have ⟨hd1, hd2⟩ := caseElem_decode (nB := nB) (w := w) (i := i) hw hvw
      simp only [h, if_neg (show ¬ caseElem nB w i v < nB by unfold caseElem; omega), hd1, hd2]
    let baseAtoms : List (ℕ × List ℕ) := (List.range choice.length).flatMap (fun i =>
        (chosen rules choice i).body.map fun a => (a.rel, a.args.map (caseElem nB w i)))
    let base : State := State.mk baseAtoms id (nB + choice.length * w)
    have hbase : Good I h base := by
      refine ⟨?_, fun x y hxy => by simp only [base, id] at hxy; rw [hxy], fun x hx => hx,
        fun x _ => rfl, ?_⟩
      · intro a ha
        simp only [base, baseAtoms, List.mem_flatMap, List.mem_range, List.mem_map] at ha
        obtain ⟨i, hi, b, hb, rfl⟩ := ha
        rw [hchoice_len] at hi
        have : (b.args.map (caseElem nB w i)).map h = b.args.map (G i) := by
          rw [List.map_map]
          refine List.map_congr_left fun v hv => hcaseh i hi v ?_
          exact List.mem_flatMap.mpr ⟨b, List.mem_cons_of_mem _ hb, hv⟩
        simp only
        rw [this]
        have hG := (hMG i hi).2.2.1
        rw [← hchosen i hi] at hG
        exact hG b hb
      · intro a ha e he
        simp only [base, baseAtoms, List.mem_flatMap, List.mem_range, List.mem_map] at ha
        obtain ⟨i, hi, b, hb, rfl⟩ := ha
        obtain ⟨v, hv, rfl⟩ := List.mem_map.mp he
        have hvw : v < w := lt_of_lt_of_le
          (lt_width (List.mem_flatMap.mpr ⟨b, List.mem_cons_of_mem _ hb, hv⟩)) (by
            rw [hchosen i (by omega)]
            exact width_le_ruleWidth (List.getElem_mem _))
        exact caseElem_lt hvw hi
    have hinit := Good.foldMerge (I := I) (h := h)
      ((List.range choice.length).flatMap fun i =>
        ((chosen rules choice i).head.args.zip (atomAt body i).args).map fun p =>
          (caseElem nB w i p.1, p.2)) hbase (by
        intro p hp
        simp only [List.mem_flatMap, List.mem_range, List.mem_map] at hp
        obtain ⟨i, hi, q, hq, rfl⟩ := hp
        rw [hchoice_len] at hi
        have hq1 : q.1 ∈ (chosen rules choice i).head.args := (List.of_mem_zip hq).1
        have hq2 : q.2 ∈ (atomAt body i).args := (List.of_mem_zip hq).2
        have hat : atomAt body i = body[i]'hi := hatom i hi
        rw [hat] at hq2 hq
        have hq2v : q.2 < nB := lt_width (mem_vars (List.getElem_mem hi) hq2)
        have hq1w : q.1 < w := lt_of_lt_of_le
          (lt_width (List.mem_flatMap.mpr ⟨_, List.mem_cons_self .., hq1⟩)) (by
            rw [hchosen i hi]
            exact width_le_ruleWidth (List.getElem_mem _))
        refine ⟨caseElem_lt hq1w (by rw [hchoice_len]; exact hi), by simp only [base]; omega, ?_⟩
        rw [hcaseh i hi q.1 (List.mem_flatMap.mpr ⟨_, List.mem_cons_self .., hq1⟩)]
        simp only [h, if_pos hq2v]
        have hargs := (hMG i hi).2.2.2
        rw [← hchosen i hi] at hargs
        exact (zip_map_eq hargs.symm hq).trans rfl)
    obtain ⟨hgood, hnext⟩ := hinit
    have hinit_eq : initial rules body choice =
        ((List.range choice.length).flatMap fun i =>
          ((chosen rules choice i).head.args.zip (atomAt body i).args).map fun p =>
            (caseElem nB w i p.1, p.2)).foldl (fun s p => s.merge p.1 p.2) base := rfl
    rw [hinit_eq] at hrun
    obtain ⟨h', hg', hagree⟩ := Good.run hmod (traces[j]'hj') hgood hrun
    have hl' : l < nB := lt_width hl
    have hr' : r < nB := lt_width hr
    have hbn : nB ≤ base.next := Nat.le_add_right _ _
    have heq := hg'.resp l r hz
    rw [hagree l (by rw [hnext]; omega), hagree r (by rw [hnext]; omega)] at heq
    simpa only [h, if_pos hl', if_pos hr'] using heq

end Norn.Sem.Exchange
