/-
The production commit admission driver is the written order (#718 P8, lane K-A).

`norn_kernel_commit_order.run_stages` is Aeneas's translation of
`crates/norn-kernel/src/commit_order.rs::run_stages`, extracted from the
production file by `scripts/verify-kernel-commit-aeneas.sh`. The theorem below
holds for every implementation of `CommitStages`: every stage body, every
commit, every host state. Stage bodies are not proved here; they are the
`inst` the theorem quantifies over.
-/
import Aeneas
import CommitOrder.Funs
import Commit.Reference

open Aeneas Aeneas.Std Result
open norn_kernel_commit_order

namespace Norn.Kernel.Commit

/-- The production stage for each written stage. -/
def toCommitStage : Stage → CommitStage
  | .eventLimit => .EventLimit
  | .shape => .Shape
  | .accounting => .Accounting
  | .delivery => .Delivery
  | .immutable => .Immutable
  | .versions => .Versions
  | .linkAuthority => .LinkAuthority
  | .endpoints => .Endpoints
  | .linkValues => .LinkValues
  | .cardinality => .Cardinality
  | .requiredLinks => .RequiredLinks
  | .acyclic => .Acyclic
  | .invariants => .Invariants
  | .laws => .Laws

/-- A Rust `Result<(), E>` as the written semantics' outcome. -/
def toExcept {E : Type} : core.result.Result Unit E → Except E Unit
  | .Ok _ => .ok ()
  | .Err e => .error e

/-- One production stage body, seen through the written semantics. -/
def stageRun {E S : Type} (inst : CommitStages S E) (s : S) (stage : Stage) :
    Result (Except E Unit × S) := do
  let (r, s1) ← inst.run s (toCommitStage stage)
  ok (toExcept r, s1)

/-- The production driver's outcome, seen through the written semantics. -/
def project {E S : Type} (x : Result (core.result.Result Unit E × S)) :
    Result (Except E Unit × S) := do
  let (r, s1) ← x
  ok (toExcept r, s1)

/-- The shape of a straight-line `stage?;` driver over `stages`: every stage
but the last propagates its refusal with `?`, and the last stage's result is
returned. -/
def chain {E S : Type} (inst : CommitStages S E) :
    List CommitStage → S → Result (core.result.Result Unit E × S)
  | [], s => ok (.Ok (), s)
  | [stage], s => inst.run s stage
  | stage :: next :: rest, s => do
    let (r, s1) ← inst.run s stage
    let cf ← core.result.Result.Insts.CoreOpsTry.branch r
    match cf with
    | core.ops.control_flow.ControlFlow.Continue _ => chain inst (next :: rest) s1
    | core.ops.control_flow.ControlFlow.Break residual => do
      let r1 ← core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual
        Unit (core.convert.FromSame E) residual
      ok (r1, s1)

/-- The extracted production driver is the straight-line driver over the
written order, stage for stage. A reordered, skipped, repeated or swallowed
stage breaks this equation. -/
theorem run_stages_eq_chain {E S : Type} (inst : CommitStages S E) (s : S) :
    run_stages inst s = chain inst (commitOrder.map toCommitStage) s := by
  simp only [run_stages, chain, commitOrder, List.map, toCommitStage]
  rfl

/-- A straight-line driver over any stage list is the written semantics. -/
theorem chain_correspondence {E S : Type} (inst : CommitStages S E) :
    ∀ (stages : List Stage) (s : S),
      project (chain inst (stages.map toCommitStage) s) = admission (stageRun inst) stages s
  | [], _ => rfl
  | [stage], s => by
    show project (chain inst [toCommitStage stage] s) = admission (stageRun inst) [stage] s
    rw [chain, admission, stageRun]
    generalize inst.run s (toCommitStage stage) = x
    rcases x with ⟨r, s1⟩ | e | _
    · rcases r with u | e <;> rfl
    · rfl
    · rfl
  | stage :: next :: rest, s => by
    have ih := chain_correspondence inst (next :: rest)
    show project (chain inst (toCommitStage stage :: toCommitStage next ::
        rest.map toCommitStage) s) = admission (stageRun inst) (stage :: next :: rest) s
    rw [chain, admission, stageRun]
    generalize inst.run s (toCommitStage stage) = x
    rcases x with ⟨r, s1⟩ | e | _
    · rcases r with u | e
      · exact ih s1
      · rfl
    · rfl
    · rfl

/-- **K-A unit 1.** For every implementation of the stage bodies and every
starting state, production commit admission runs the stages in the written
order, ends at the first refusal with that refusal, and admits only when every
stage passed; a stage that panics or diverges makes admission panic or diverge.
-/
theorem commit_stages_correspondence {E S : Type} (inst : CommitStages S E) (s : S) :
    project (run_stages inst s) = admission (stageRun inst) commitOrder s := by
  rw [run_stages_eq_chain]
  exact chain_correspondence inst commitOrder s

deriving instance DecidableEq for CommitStage

/-- A recording implementation of the stage bodies: it logs every stage it
runs, in order, and refuses at `stop`. -/
def recorder (stop : Option CommitStage) : CommitStages (List CommitStage) CommitStage where
  run log stage := ok (if some stage = stop then .Err stage else .Ok (), log ++ [stage])

/-- What one run shows: its refusal, if any, and the stages that ran. -/
def observe (x : Result (core.result.Result Unit CommitStage × List CommitStage)) :
    Option (Option CommitStage × List CommitStage) :=
  match x with
  | ok (.Ok _, log) => some (none, log)
  | ok (.Err stage, log) => some (some stage, log)
  | _ => none

/-- The written expectation: with no refusal every stage runs in order; a
refusal at `stop` runs exactly the stages before it, then `stop`, and refuses
with it. -/
def expectedRun : Option Stage → Option (Option CommitStage × List CommitStage)
  | none => some (none, commitOrder.map toCommitStage)
  | some stop =>
    some (some (toCommitStage stop), (commitOrder.takeWhile (· ≠ stop) ++ [stop]).map toCommitStage)

/-- The production driver, evaluated on the recorder for every refusal point
(none, then each stage), runs exactly the written prefix and stops at the
refusing stage. This is a closed computation the kernel evaluates; a
reordered, skipped, repeated or swallowed stage makes it false outright. -/
theorem run_stages_records :
    (none :: commitOrder.map some).map
        (fun stop => observe (run_stages (recorder (stop.map toCommitStage)) [])) =
      (none :: commitOrder.map some).map expectedRun := by
  decide

/-- A refusal ends admission: no later stage runs, whatever the later stages
would do. -/
theorem admission_refusal_first {E S : Type} (run : S → Stage → Result (Except E Unit × S))
    (stage : Stage) (rest : List Stage) (s s1 : S) (e : E)
    (h : run s stage = ok (.error e, s1)) :
    admission run (stage :: rest) s = ok (.error e, s1) := by
  rw [admission, h]
  rfl

/-- A passing stage hands its state to the rest of the order. -/
theorem admission_pass_next {E S : Type} (run : S → Stage → Result (Except E Unit × S))
    (stage : Stage) (rest : List Stage) (s s1 : S)
    (h : run s stage = ok (.ok (), s1)) :
    admission run (stage :: rest) s = admission run rest s1 := by
  rw [admission, h]
  rfl

#print axioms commit_stages_correspondence
#print axioms run_stages_records
#print axioms admission_refusal_first
#print axioms admission_pass_next

end Norn.Kernel.Commit
