import Mathlib

/-!
# RFC 8259 number grammar: written semantics

`JsonNumberShape l integer` says that the byte string `l` is a JSON number
(RFC 8259 §6):

    number = [ minus ] int [ frac ] [ exp ]
    int    = zero / ( digit1-9 *DIGIT )
    frac   = decimal-point 1*DIGIT
    exp    = e [ minus / plus ] 1*DIGIT

and that `integer` holds exactly when it has neither a fraction nor an
exponent. The statement is a decomposition of the bytes into those parts; it
mentions neither indices nor the Rust scan.

`shapeOf` is a reference decision procedure over lists. `shapeOf_iff` shows it
decides the grammar: it returns `some b` exactly when the bytes are a JSON
number whose integer flag is `b`, and `none` exactly when they are not one.
-/

namespace Norn.Codec

/-- An ASCII digit `0`–`9`. -/
def isDigit (b : ℕ) : Bool := 48 ≤ b && b ≤ 57

/-- `1*DIGIT`. -/
def Digits (l : List ℕ) : Prop := l ≠ [] ∧ ∀ b ∈ l, isDigit b = true

/-- `int = zero / ( digit1-9 *DIGIT )`. -/
def IntPart (l : List ℕ) : Prop :=
  l = [48] ∨ ∃ d ds, l = d :: ds ∧ 49 ≤ d ∧ d ≤ 57 ∧ ∀ b ∈ ds, isDigit b = true

/-- `[ frac ]`. -/
def FracPart (l : List ℕ) : Prop := l = [] ∨ ∃ ds, l = 46 :: ds ∧ Digits ds

/-- `[ exp ]`. -/
def ExpPart (l : List ℕ) : Prop :=
  l = [] ∨ ∃ e s ds, l = e :: (s ++ ds) ∧ (e = 101 ∨ e = 69) ∧
    (s = [] ∨ s = [43] ∨ s = [45]) ∧ Digits ds

/-- RFC 8259: `l` is a JSON number, and `integer` says it has no fraction and
    no exponent. -/
def JsonNumberShape (l : List ℕ) (integer : Bool) : Prop :=
  ∃ sign int frac exp, l = sign ++ (int ++ (frac ++ exp)) ∧ (sign = [] ∨ sign = [45]) ∧
    IntPart int ∧ FracPart frac ∧ ExpPart exp ∧ (integer = true ↔ frac = [] ∧ exp = [])

/-- The rest after an optional sign. -/
def afterSign (l : List ℕ) : List ℕ :=
  match l with
  | c :: t => if c = 45 then t else c :: t
  | [] => []

/-- The rest after `int`, or `none`. -/
def afterInt (l : List ℕ) : Option (List ℕ) :=
  match l with
  | d :: r => if d = 48 then some r else if isDigit d then some (r.dropWhile isDigit) else none
  | [] => none

/-- The rest after an optional `frac`, and whether there was one. -/
def afterFrac (l : List ℕ) : Option (List ℕ × Bool) :=
  match l with
  | c :: t =>
      if c = 46 then
        if (t.takeWhile isDigit).length = 0 then none else some (t.dropWhile isDigit, true)
      else some (c :: t, false)
  | [] => some ([], false)

/-- The rest after the exponent's optional sign. -/
def afterExpSign (l : List ℕ) : List ℕ :=
  match l with
  | c :: t => if c = 43 ∨ c = 45 then t else c :: t
  | [] => []

/-- The rest after an optional `exp`, and whether there was one. -/
def afterExp (l : List ℕ) : Option (List ℕ × Bool) :=
  match l with
  | c :: t =>
      if c = 101 ∨ c = 69 then
        if ((afterExpSign t).takeWhile isDigit).length = 0 then none
        else some ((afterExpSign t).dropWhile isDigit, true)
      else some (c :: t, false)
  | [] => some ([], false)

/-- Reference decision procedure for the grammar. -/
def shapeOf (l : List ℕ) : Option Bool :=
  match afterInt (afterSign l) with
  | none => none
  | some r1 =>
    match afterFrac r1 with
    | none => none
    | some (r2, f) =>
      match afterExp r2 with
      | none => none
      | some (r3, e) => if r3 = [] then some (!f && !e) else none

/-! ### `shapeOf` decides the grammar -/

private theorem digits_of_takeWhile (l : List ℕ) : ∀ b ∈ l.takeWhile isDigit, isDigit b = true :=
  fun _ hb => List.mem_takeWhile_imp hb

private theorem split_digits (l : List ℕ) : l = l.takeWhile isDigit ++ l.dropWhile isDigit :=
  (List.takeWhile_append_dropWhile).symm

private theorem afterSign_split (l : List ℕ) :
    ∃ sign, l = sign ++ afterSign l ∧ (sign = [] ∨ sign = [45]) := by
  cases l with
  | nil => exact ⟨[], rfl, Or.inl rfl⟩
  | cons c t =>
    by_cases hc : c = 45
    · subst hc; exact ⟨[45], by simp [afterSign], Or.inr rfl⟩
    · exact ⟨[], by simp [afterSign, hc], Or.inl rfl⟩

private theorem afterExpSign_split (l : List ℕ) :
    ∃ s, l = s ++ afterExpSign l ∧ (s = [] ∨ s = [43] ∨ s = [45]) := by
  cases l with
  | nil => exact ⟨[], rfl, Or.inl rfl⟩
  | cons c t =>
    by_cases hc : c = 43 ∨ c = 45
    · rcases hc with rfl | rfl
      · exact ⟨[43], by simp [afterExpSign], Or.inr (Or.inl rfl)⟩
      · exact ⟨[45], by simp [afterExpSign], Or.inr (Or.inr rfl)⟩
    · exact ⟨[], by simp [afterExpSign, hc], Or.inl rfl⟩

private theorem afterInt_some {x r : List ℕ} (h : afterInt x = some r) :
    ∃ int, x = int ++ r ∧ IntPart int := by
  cases x with
  | nil => simp [afterInt] at h
  | cons d t =>
    by_cases h0 : d = 48
    · simp only [afterInt, if_pos h0, Option.some.injEq] at h
      subst h; subst h0
      exact ⟨[48], rfl, Or.inl rfl⟩
    · by_cases h1 : isDigit d = true
      · simp only [afterInt, if_neg h0, if_pos h1, Option.some.injEq] at h
        subst h
        have hd : 48 ≤ d ∧ d ≤ 57 := by simpa [isDigit] using h1
        refine ⟨d :: t.takeWhile isDigit, ?_, Or.inr ⟨d, _, rfl, by omega, hd.2,
          digits_of_takeWhile t⟩⟩
        rw [List.cons_append, ← split_digits]
      · simp [afterInt, h0, h1] at h

private theorem afterFrac_some {x r : List ℕ} {f : Bool} (h : afterFrac x = some (r, f)) :
    ∃ frac, x = frac ++ r ∧ FracPart frac ∧ (f = true ↔ frac ≠ []) := by
  cases x with
  | nil =>
    simp only [afterFrac, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    exact ⟨[], rfl, Or.inl rfl, by simp⟩
  | cons c t =>
    by_cases hc : c = 46
    · subst hc
      by_cases hl : (t.takeWhile isDigit).length = 0
      · simp [afterFrac, hl] at h
      · simp only [afterFrac, if_neg hl] at h
        obtain ⟨rfl, rfl⟩ := h
        refine ⟨46 :: t.takeWhile isDigit, ?_, Or.inr ⟨_, rfl, ?_, digits_of_takeWhile t⟩, by simp⟩
        · rw [List.cons_append, ← split_digits]
        · intro hnil; exact hl (by rw [hnil]; rfl)
    · simp only [afterFrac, if_neg hc, Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      exact ⟨[], rfl, Or.inl rfl, by simp⟩

private theorem afterExp_some {x r : List ℕ} {e : Bool} (h : afterExp x = some (r, e)) :
    ∃ exp, x = exp ++ r ∧ ExpPart exp ∧ (e = true ↔ exp ≠ []) := by
  cases x with
  | nil =>
    simp only [afterExp, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    exact ⟨[], rfl, Or.inl rfl, by simp⟩
  | cons c t =>
    by_cases hc : c = 101 ∨ c = 69
    · by_cases hl : ((afterExpSign t).takeWhile isDigit).length = 0
      · simp [afterExp, hc, hl] at h
      · simp only [afterExp, if_pos hc, if_neg hl, Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, rfl⟩ := h
        obtain ⟨s, hs, hs'⟩ := afterExpSign_split t
        refine ⟨c :: (s ++ (afterExpSign t).takeWhile isDigit), ?_,
          Or.inr ⟨c, s, _, rfl, hc, hs', ?_, digits_of_takeWhile _⟩, by simp⟩
        · conv_lhs => rw [hs, split_digits (afterExpSign t)]
          simp
        · intro hnil; exact hl (by rw [hnil]; rfl)
    · simp only [afterExp, if_neg hc, Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      exact ⟨[], rfl, Or.inl rfl, by simp⟩

/-- Soundness: whatever `shapeOf` accepts is a JSON number with that flag. -/
theorem shapeOf_sound {l : List ℕ} {b : Bool} (h : shapeOf l = some b) :
    JsonNumberShape l b := by
  unfold shapeOf at h
  rcases h1 : afterInt (afterSign l) with _ | r1
  · simp [h1] at h
  rcases h2 : afterFrac r1 with _ | ⟨r2, f⟩
  · simp [h1, h2] at h
  rcases h3 : afterExp r2 with _ | ⟨r3, e⟩
  · simp [h1, h2, h3] at h
  simp only [h1, h2, h3] at h
  by_cases hr3 : r3 = []
  · rw [if_pos hr3, Option.some.injEq] at h
    subst hr3
    obtain ⟨sign, hl, hsign⟩ := afterSign_split l
    obtain ⟨int, hx, hint⟩ := afterInt_some h1
    obtain ⟨frac, hr1, hfrac, hf⟩ := afterFrac_some h2
    obtain ⟨exp, hr2, hexp, he⟩ := afterExp_some h3
    refine ⟨sign, int, frac, exp, ?_, hsign, hint, hfrac, hexp, ?_⟩
    · rw [hl, hx, hr1, hr2]; simp
    · rw [← h]
      clear h hl hx hr1 hr2 h1 h2 h3
      cases f <;> cases e <;> simp at hf he ⊢ <;> tauto
  · rw [if_neg hr3] at h; cases h

private theorem dropWhile_digits {ds r : List ℕ} (hd : ∀ b ∈ ds, isDigit b = true)
    (hr : ∀ c t, r = c :: t → isDigit c = false) : (ds ++ r).dropWhile isDigit = r := by
  induction ds with
  | nil =>
    cases r with
    | nil => rfl
    | cons c t => simp [hr c t rfl]
  | cons x xs ih =>
    rw [List.cons_append, List.dropWhile_cons, if_pos (hd x (by simp))]
    exact ih (fun b hb => hd b (by simp [hb]))

private theorem takeWhile_digits {ds r : List ℕ} (hd : ∀ b ∈ ds, isDigit b = true)
    (hr : ∀ c t, r = c :: t → isDigit c = false) : (ds ++ r).takeWhile isDigit = ds := by
  induction ds with
  | nil =>
    cases r with
    | nil => rfl
    | cons c t => simp [hr c t rfl]
  | cons x xs ih =>
    rw [List.cons_append, List.takeWhile_cons, if_pos (hd x (by simp))]
    rw [ih (fun b hb => hd b (by simp [hb]))]

private theorem exp_head {exp : List ℕ} (h : ExpPart exp) :
    ∀ c t, exp = c :: t → c = 101 ∨ c = 69 := by
  intro c t he
  rcases h with rfl | ⟨e, s, ds, rfl, he', -, -⟩
  · cases he
  · cases he; exact he'

private theorem rest_not_digit {frac exp : List ℕ} (hf : FracPart frac) (he : ExpPart exp) :
    ∀ c t, frac ++ exp = c :: t → isDigit c = false := by
  intro c t h
  rcases hf with rfl | ⟨ds, rfl, -⟩
  · rcases exp_head he c t h with rfl | rfl <;> rfl
  · cases h; rfl

/-- Completeness: every JSON number is accepted with its flag. -/
theorem shapeOf_complete {l : List ℕ} {b : Bool} (h : JsonNumberShape l b) :
    shapeOf l = some b := by
  obtain ⟨sign, int, frac, exp, rfl, hsign, hint, hfrac, hexp, hb⟩ := h
  have hrest := rest_not_digit hfrac hexp
  -- the integer part
  have hInt : afterInt (int ++ (frac ++ exp)) = some (frac ++ exp) := by
    rcases hint with rfl | ⟨d, ds, rfl, hd1, hd2, hds⟩
    · simp [afterInt]
    · have hd48 : d ≠ 48 := by omega
      have hdig : isDigit d = true := by simp [isDigit]; omega
      simp only [List.cons_append, afterInt, if_neg hd48, if_pos hdig]
      rw [dropWhile_digits hds hrest]
  have hSign : afterSign (sign ++ (int ++ (frac ++ exp))) = int ++ (frac ++ exp) := by
    rcases hsign with rfl | rfl
    · rcases hint with rfl | ⟨d, ds, rfl, hd1, hd2, -⟩
      · simp [afterSign]
      · have : d ≠ 45 := by omega
        simp [afterSign, this]
    · simp [afterSign]
  -- the fraction
  have hFrac : afterFrac (frac ++ exp) = some (exp, decide (frac ≠ [])) := by
    rcases hfrac with rfl | ⟨ds, rfl, hne, hds⟩
    · cases exp with
      | nil => simp [afterFrac]
      | cons c t =>
        have : c ≠ 46 := by rcases exp_head hexp c t rfl with rfl | rfl <;> decide
        simp [afterFrac, this]
    · have hexp' : ∀ c t, exp = c :: t → isDigit c = false := by
        intro c t h; rcases exp_head hexp c t h with rfl | rfl <;> rfl
      simp only [List.cons_append, afterFrac]
      rw [takeWhile_digits hds hexp', dropWhile_digits hds hexp']
      have : ds.length ≠ 0 := by simpa using hne
      simp [this]
  -- the exponent
  have hExp : afterExp exp = some ([], decide (exp ≠ [])) := by
    rcases hexp with rfl | ⟨e, s, ds, rfl, he, hs, hne, hds⟩
    · simp [afterExp]
    · have hnil : ∀ c t, ([] : List ℕ) = c :: t → isDigit c = false := by
        intro c t h; cases h
      have hsign' : afterExpSign (s ++ ds) = ds := by
        rcases hs with rfl | rfl | rfl
        · cases ds with
          | nil => exact absurd rfl hne
          | cons x xs =>
            have hx : isDigit x = true := hds x (by simp)
            have : ¬ (x = 43 ∨ x = 45) := by
              simp [isDigit] at hx; omega
            simp [afterExpSign, this]
        · simp [afterExpSign]
        · simp [afterExpSign]
      have hds' := takeWhile_digits hds hnil
      have hdd' := dropWhile_digits hds hnil
      rw [List.append_nil] at hds' hdd'
      have : ds.length ≠ 0 := by simpa using hne
      simp [afterExp, he, hsign', hds', hdd', this]
  unfold shapeOf
  rw [hSign, hInt]
  dsimp only
  rw [hFrac]
  dsimp only
  rw [hExp]
  dsimp only
  rw [if_pos rfl, Option.some.injEq]
  clear hInt hSign hFrac hExp hrest
  cases b <;> simp at hb ⊢ <;> tauto

/-- `shapeOf` decides the RFC 8259 number grammar, with the integer flag. -/
theorem shapeOf_iff (l : List ℕ) (b : Bool) : shapeOf l = some b ↔ JsonNumberShape l b :=
  ⟨shapeOf_sound, shapeOf_complete⟩

/-- `shapeOf` refuses exactly the byte strings that are not JSON numbers. -/
theorem shapeOf_none_iff (l : List ℕ) : shapeOf l = none ↔ ∀ b, ¬ JsonNumberShape l b := by
  constructor
  · intro h b hb; rw [shapeOf_complete hb] at h; cases h
  · intro h
    cases hs : shapeOf l with
    | none => rfl
    | some b => exact absurd (shapeOf_sound hs) (h b)

end Norn.Codec
