import Decider.FoldProof

/-!
# Production `run_step` and the envelope decisions meet the written step

`run_step` is the production step driver in
`crates/norn-kernel/src/decider_fold.rs`; `crates/norn-kernel/src/decider.rs`
calls it from the public `step`, and `envelope.rs` calls `exceeded_cap` and
`exceeded_turn_cap` from every commit path. The theorems quantify over every
`StepTurn` implementation (every decider, input, world and cap) and every
count: the production driver runs the checks in the written order, and an
accepted turn's state is the fold of its own events. From that, a live state
reached by accepted turns is the replay of the ledger they wrote.
-/

set_option linter.unusedSimpArgs false

namespace Norn.Kernel.Decider
open Aeneas Aeneas.Std Result
open norn_kernel_decider

/-- A production cap as the written semantics names it. -/
def _root_.norn_kernel_decider.EnvelopeCap.cap : EnvelopeCap → Cap
  | .Events => .events
  | .Commands => .commands
  | .Timers => .timers

/-- A written cap as the production enum names it. -/
def Cap.core : Cap → EnvelopeCap
  | .events => .Events
  | .commands => .Commands
  | .timers => .Timers

/-- Production counts as natural numbers. -/
def _root_.norn_kernel_decider.Counts.tally (c : Counts) : Tally := ⟨c.events.val, c.commands.val, c.timers.val⟩

/-- A production refusal as the written semantics states it. -/
def _root_.norn_kernel_decider.StepRefusal.refusal {R : Type} : StepRefusal R → Refusal R
  | .ReadOnlyWorld => .readOnlyWorld
  | .Rejected r => .rejected r
  | .EnvelopeExceeded cap actual => .envelopeExceeded cap.cap actual.val
  | .PerformOutsideMain commands => .performOutsideMain commands.val

/-- A production `Result` as `Except`. -/
def toExcept {α β : Type} : core.result.Result α β → Except β α
  | .Ok a => .ok a
  | .Err b => .error b

/-- A production step outcome as the written semantics states it. -/
def outcome {S D R : Type} : core.result.Result (S × D) (StepRefusal R) → Except (Refusal R) (S × D)
  | .Ok sd => .ok sd
  | .Err refusal => .error refusal.refusal

/-- What a production turn offers the written step: its methods, with counts
    as natural numbers and slices as lists. -/
def _root_.norn_kernel_decider.StepTurn.turn {T D R S E : Type} (inst : StepTurn T D R S E) (t : T) :
    Turn Result S E D R where
  writable := inst.writable t
  mayPerform := inst.may_perform t
  decide s := do let r ← inst.decide t s; pure (toExcept r)
  counts dec := do let c ← inst.counts t dec; pure c.tally
  caps := do let c ← inst.caps t; pure c.tally
  events dec := do let es ← inst.events t dec; pure es.val
  cloneState := inst.clone_state t
  evolve := inst.EvolveInst.evolve t

section Envelope

/-- K6 envelope: for every count and cap, the production `exceeded_cap`
    reports exactly the first kind, in the order events, commands, timers,
    above its cap, and never panics. -/
theorem exceeded_cap_correspondence (caps counts : Counts) :
    exceeded_cap caps counts = ok ((firstExceeded caps.tally counts.tally).map Cap.core) := by
  unfold exceeded_cap firstExceeded
  simp only [Counts.tally, gt_iff_lt, UScalar.lt_equiv]
  split_ifs <;> rfl

theorem count_of_correspondence (counts : Counts) (cap : EnvelopeCap) :
    ∃ n, count_of counts cap = ok n ∧ n.val = counts.tally.get cap.cap := by
  cases cap <;> exact ⟨_, rfl, rfl⟩

private theorem u64_max_val : core.num.U64.MAX.val = u64Max := by
  simp [core.num.U64.MAX, U64.rMax, u64Max]

private theorem turn_overflow_correspondence (used cap added : U64) :
    ∃ r, turn_overflow used cap added = ok r ∧
      r.map (fun x => x.val) =
        if cap.val < used.val + added.val then some (turnTotal used.val added.val) else none := by
  have hcap := cap.hBounds
  have hspec := U64.checked_add_bv_spec used added
  simp only [U64.max_eq] at hspec
  simp only [UScalarTy.numBits] at hcap
  unfold turn_overflow
  cases hca : U64.checked_add used added with
  | none =>
    rw [hca] at hspec
    refine ⟨some core.num.U64.MAX, by simp [lift], ?_⟩
    have hlt : cap.val < used.val + added.val := by omega
    simp only [Option.map_some, if_pos hlt, u64_max_val, turnTotal, u64Max]
    congr 1
    omega
  | some total =>
    rw [hca] at hspec
    obtain ⟨hle, hval, -⟩ := hspec
    by_cases hgt : cap.val < total.val
    · refine ⟨some total, ?_, ?_⟩
      · simp [lift, gt_iff_lt, UScalar.lt_equiv, hgt]
      · rw [hval] at hgt
        simp only [Option.map_some, if_pos hgt, turnTotal, u64Max]
        congr 1
        omega
    · refine ⟨none, ?_, ?_⟩
      · simp [lift, gt_iff_lt, UScalar.lt_equiv, hgt]
      · rw [hval] at hgt
        simp [hgt]

/-- K6 turn envelope: for every used total, cap and added count, the production
    `exceeded_turn_cap` reports exactly the first kind, in the order events,
    commands, timers, whose total would be above its cap, with the total
    saturated at `u64::MAX` on overflow, and never panics. -/
theorem exceeded_turn_cap_correspondence (used caps added : Counts) :
    ∃ r, exceeded_turn_cap used caps added = ok r ∧
      r.map (fun e => (e.cap.cap, e.actual.val)) =
        firstTurnExceeded used.tally caps.tally added.tally := by
  obtain ⟨re, hre, hspe⟩ := turn_overflow_correspondence used.events caps.events added.events
  obtain ⟨rc, hrc, hspc⟩ := turn_overflow_correspondence used.commands caps.commands added.commands
  obtain ⟨rt, hrt, hspt⟩ := turn_overflow_correspondence used.timers caps.timers added.timers
  unfold exceeded_turn_cap firstTurnExceeded
  simp only [Counts.tally]
  rw [hre]
  cases re with
  | some ae =>
    simp only [Option.map_some] at hspe
    split_ifs at hspe with he
    · simp only [Option.some.injEq] at hspe
      refine ⟨_, rfl, ?_⟩
      simp [EnvelopeCap.cap, hspe, if_pos he]
  | none =>
    simp only [Option.map_none] at hspe
    split_ifs at hspe with he
    simp only [bind_tc_ok, if_neg he]
    rw [hrc]
    cases rc with
    | some ac =>
      simp only [Option.map_some] at hspc
      split_ifs at hspc with hc
      · simp only [Option.some.injEq] at hspc
        refine ⟨_, rfl, ?_⟩
        simp [EnvelopeCap.cap, hspc, if_pos hc]
    | none =>
      simp only [Option.map_none] at hspc
      split_ifs at hspc with hc
      simp only [bind_tc_ok, if_neg hc]
      rw [hrt]
      cases rt with
      | some at' =>
        simp only [Option.map_some] at hspt
        split_ifs at hspt with ht
        · simp only [Option.some.injEq] at hspt
          refine ⟨_, rfl, ?_⟩
          simp [EnvelopeCap.cap, hspt, if_pos ht]
      | none =>
        simp only [Option.map_none] at hspt
        split_ifs at hspt with ht
        exact ⟨none, rfl, by simp [if_neg ht]⟩

end Envelope

section Step

variable {T D R S E : Type} (inst : StepTurn T D R S E) (t : T)

private theorem cap_round_trip (cap : Cap) : cap.core.cap = cap := by
  cases cap <;> rfl

private theorem zero_lt_commands (counts : Counts) :
    (counts.commands > 0#u64) ↔ 0 < counts.tally.commands := by
  simp [Counts.tally, gt_iff_lt, UScalar.lt_equiv]

/-- K6 step order: for every turn, the production `run_step` is the written
    step. The world is checked first, then the decider, then the caps, then
    perform-outside-main (consulting the world only when a command is
    requested), and only then are the decision's events folded into a copy of
    the state. Every observer failure surfaces exactly where the written step
    consults it. -/
theorem run_step_correspondence (state : S) :
    (do let r ← run_step inst t state; pure (outcome r)) = step (inst.turn t) state := by
  unfold run_step step StepTurn.turn
  dsimp only
  cases hw : inst.writable t with
  | fail e => simp [hw]
  | div => simp [hw]
  | ok w =>
    cases w with
    | false => simp [hw, outcome, StepRefusal.refusal]
    | true =>
      cases hd : inst.decide t state with
      | fail e => simp [hw, hd]
      | div => simp [hw, hd]
      | ok r =>
        cases r with
        | Err rejection => simp [hw, hd, toExcept, outcome, StepRefusal.refusal]
        | Ok dec =>
          cases hc : inst.counts t dec with
          | fail e => simp [hw, hd, hc, toExcept]
          | div => simp [hw, hd, hc, toExcept]
          | ok counts =>
            cases hcaps : inst.caps t with
            | fail e => simp [hw, hd, hc, hcaps, toExcept]
            | div => simp [hw, hd, hc, hcaps, toExcept]
            | ok caps =>
              simp only [hw, hd, hc, hcaps, toExcept, bind_tc_ok, result_pure,
                exceeded_cap_correspondence, if_true, Bool.true_eq_false, if_false]
              cases hex : firstExceeded caps.tally counts.tally with
              | some cap =>
                obtain ⟨n, hn, hnv⟩ := count_of_correspondence counts cap.core
                rw [cap_round_trip] at hnv
                simp [hn, outcome, StepRefusal.refusal, cap_round_trip, hnv]
              | none =>
                simp only [Option.map_none]
                by_cases hcmd : 0 < counts.tally.commands
                · have hgt : counts.commands > 0#u64 := (zero_lt_commands counts).mpr hcmd
                  simp only [hgt, if_true, if_pos hcmd]
                  cases hm : inst.may_perform t with
                  | fail e => simp [hm]
                  | div => simp [hm]
                  | ok mp =>
                    cases mp with
                    | false =>
                      simp [hm, outcome, StepRefusal.refusal, Counts.tally]
                    | true =>
                      cases hcl : inst.clone_state t state with
                      | fail e => simp [hm, hcl]
                      | div => simp [hm, hcl]
                      | ok copy =>
                        cases hes : inst.events t dec with
                        | fail e => simp [hm, hcl, hes]
                        | div => simp [hm, hcl, hes]
                        | ok es =>
                          simp only [hm, hcl, hes, bind_tc_ok, result_pure, if_true,
                            Bool.true_eq_false, if_false, fold_events_correspondence]
                          cases foldLedger (inst.EvolveInst.evolve t) copy es.val <;>
                            simp [outcome]
                · have hngt : ¬ counts.commands > 0#u64 :=
                    fun h => hcmd ((zero_lt_commands counts).mp h)
                  simp only [hngt, if_false, if_neg hcmd]
                  cases hcl : inst.clone_state t state with
                  | fail e => simp [hcl]
                  | div => simp [hcl]
                  | ok copy =>
                    cases hes : inst.events t dec with
                    | fail e => simp [hcl, hes]
                    | div => simp [hcl, hes]
                    | ok es =>
                      simp only [hcl, hes, bind_tc_ok, result_pure, fold_events_correspondence]
                      cases foldLedger (inst.EvolveInst.evolve t) copy es.val <;>
                        simp [outcome]

/-- An accepted production turn passed every check, and its state is the fold
    of its own events from a copy of the input state. A refused turn has no
    state and no events: `run_step` returns only the refusal. -/
theorem run_step_accepted (state next : S) (dec : D)
    (h : run_step inst t state = ok (.Ok (next, dec))) :
    inst.writable t = ok true ∧ inst.decide t state = ok (.Ok dec) ∧
      (∃ counts caps, inst.counts t dec = ok counts ∧ inst.caps t = ok caps ∧
        firstExceeded caps.tally counts.tally = none ∧
        (counts.tally.commands = 0 ∨ inst.may_perform t = ok true)) ∧
      ∃ copy es, inst.clone_state t state = ok copy ∧ inst.events t dec = ok es ∧
        foldLedger (inst.EvolveInst.evolve t) copy es.val = ok next := by
  unfold run_step at h
  cases hw : inst.writable t with
  | fail e => simp [hw] at h
  | div => simp [hw] at h
  | ok w =>
    cases w with
    | false => simp [hw] at h
    | true =>
      cases hd : inst.decide t state with
      | fail e => simp [hw, hd] at h
      | div => simp [hw, hd] at h
      | ok r =>
        cases r with
        | Err rejection => simp [hw, hd] at h
        | Ok dec' =>
          cases hc : inst.counts t dec' with
          | fail e => simp [hw, hd, hc] at h
          | div => simp [hw, hd, hc] at h
          | ok counts =>
            cases hcaps : inst.caps t with
            | fail e => simp [hw, hd, hc, hcaps] at h
            | div => simp [hw, hd, hc, hcaps] at h
            | ok caps =>
              simp only [hw, hd, hc, hcaps, bind_tc_ok, if_true,
                exceeded_cap_correspondence] at h
              cases hex : firstExceeded caps.tally counts.tally with
              | some cap =>
                obtain ⟨n, hn, -⟩ := count_of_correspondence counts cap.core
                simp [hex, hn] at h
              | none =>
                simp only [hex, Option.map_none] at h
                cases hcl : inst.clone_state t state with
                | fail e =>
                  by_cases hgt : counts.commands > 0#u64
                  · cases hm : inst.may_perform t with
                    | ok mp => cases mp <;> simp [hgt, hm, hcl] at h
                    | fail e => simp [hgt, hm] at h
                    | div => simp [hgt, hm] at h
                  · simp [hgt, hcl] at h
                | div =>
                  by_cases hgt : counts.commands > 0#u64
                  · cases hm : inst.may_perform t with
                    | ok mp => cases mp <;> simp [hgt, hm, hcl] at h
                    | fail e => simp [hgt, hm] at h
                    | div => simp [hgt, hm] at h
                  · simp [hgt, hcl] at h
                | ok copy =>
                  cases hes : inst.events t dec' with
                  | fail e =>
                    by_cases hgt : counts.commands > 0#u64
                    · cases hm : inst.may_perform t with
                      | ok mp => cases mp <;> simp [hgt, hm, hcl, hes] at h
                      | fail e => simp [hgt, hm] at h
                      | div => simp [hgt, hm] at h
                    · simp [hgt, hcl, hes] at h
                  | div =>
                    by_cases hgt : counts.commands > 0#u64
                    · cases hm : inst.may_perform t with
                      | ok mp => cases mp <;> simp [hgt, hm, hcl, hes] at h
                      | fail e => simp [hgt, hm] at h
                      | div => simp [hgt, hm] at h
                    · simp [hgt, hcl, hes] at h
                  | ok es =>
                    have hfold := fold_events_correspondence inst.EvolveInst t es copy
                    by_cases hgt : counts.commands > 0#u64
                    · cases hm : inst.may_perform t with
                      | fail e => simp [hgt, hm] at h
                      | div => simp [hgt, hm] at h
                      | ok mp =>
                        cases mp with
                        | false => simp [hgt, hm] at h
                        | true =>
                          simp only [hgt, hm, hcl, hes, if_true, bind_tc_ok, Bool.true_eq_false,
                            if_false] at h
                          cases hf : fold_events inst.EvolveInst t copy es with
                          | fail e => simp [hf] at h
                          | div => simp [hf] at h
                          | ok n =>
                            simp [hf] at h
                            obtain ⟨rfl, rfl⟩ := h
                            exact ⟨rfl, rfl, ⟨counts, caps, hc, rfl, hex, Or.inr rfl⟩,
                              copy, es, rfl, hes, by rw [← hfold, hf]⟩
                    · simp only [hgt, hcl, hes, if_false, bind_tc_ok] at h
                      cases hf : fold_events inst.EvolveInst t copy es with
                      | fail e => simp [hf] at h
                      | div => simp [hf] at h
                      | ok n =>
                        simp [hf] at h
                        obtain ⟨rfl, rfl⟩ := h
                        have h0 : counts.tally.commands = 0 := by
                          have := (zero_lt_commands counts).not.mp hgt
                          omega
                        exact ⟨rfl, rfl, ⟨counts, caps, hc, rfl, hex, Or.inl h0⟩,
                          copy, es, rfl, hes, by rw [← hfold, hf]⟩

/-- Turns the production driver accepted, in order, from state `s`: their
    events, concatenated, are the ledger they wrote, and `last` is the state
    the last one returned. -/
inductive Accepted : S → List E → S → Prop
  | nil (s : S) : Accepted s [] s
  | turn (t : T) {s mid last : S} {dec : D} {es : Slice E} {rest : List E} :
      run_step inst t s = ok (.Ok (mid, dec)) → inst.events t dec = ok es →
      Accepted mid rest last → Accepted s (es.val ++ rest) last

/-- Step then evolve is consistent: when every turn evolves with the same
    decider `ev` and copies the state faithfully, the state reached by accepted
    turns is the ledger fold of the events they wrote. -/
theorem accepted_is_fold (ev : S → E → Result S)
    (hev : ∀ t, inst.EvolveInst.evolve t = ev)
    (hclone : ∀ t s, inst.clone_state t s = ok s)
    {s last : S} {ledger : List E} (h : Accepted inst s ledger last) :
    foldLedger ev s ledger = ok last := by
  induction h with
  | nil s => simp [foldLedger]
  | turn t hstep hevents _ ih =>
    obtain ⟨-, -, -, copy, es', hcopy, hes', hfold⟩ := run_step_accepted inst t _ _ _ hstep
    rw [hclone] at hcopy
    cases hcopy
    rw [hevents] at hes'
    cases hes'
    rw [hev] at hfold
    unfold foldLedger at hfold ih ⊢
    rw [foldlM_append, hfold]
    simpa using ih

/-- K6 live state: when the ledger replay evolves with the same decider and
    starts from the state the turns started from, replaying the ledger the
    accepted turns wrote reaches exactly the live state. -/
theorem live_state_is_replay {Lg : Type} (L : Evolve Lg S E) (l : Lg)
    (ev : S → E → Result S) (s0 last : S)
    (hev : ∀ t, inst.EvolveInst.evolve t = ev)
    (hclone : ∀ t s, inst.clone_state t s = ok s)
    (hlev : L.evolve l = ev) (hinit : L.initial_state l = ok s0)
    (ledger : Slice E) (h : Accepted inst s0 ledger.val last) :
    replay_events L l ledger = ok last := by
  rw [replay_events_correspondence]
  unfold replayLedger
  rw [hinit, hlev]
  simpa using accepted_is_fold inst ev hev hclone h

end Step

end Norn.Kernel.Decider
#print axioms Norn.Kernel.Decider.exceeded_cap_correspondence
#print axioms Norn.Kernel.Decider.exceeded_turn_cap_correspondence
#print axioms Norn.Kernel.Decider.run_step_correspondence
#print axioms Norn.Kernel.Decider.run_step_accepted
#print axioms Norn.Kernel.Decider.accepted_is_fold
#print axioms Norn.Kernel.Decider.live_state_is_replay
