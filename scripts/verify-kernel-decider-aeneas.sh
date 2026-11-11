#!/usr/bin/env bash
# Kernel lane K-B (#718): Charon/Aeneas extract the production decider core,
# crates/norn-kernel/src/decider_fold.rs, standalone, and Lean proves it against
# the written fold, replay, step-order and envelope semantics in
# verification/kernel-proof-v1/Decider. Every seeded fold, order and envelope
# mutant must then extract and fail the same proof.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
proof_root="$repo_root/verification/kernel-proof-v1"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/kernel-decider-aeneas"
core_source="crates/norn-kernel/src/decider_fold.rs"
# The exact Lean sources this gate elaborates; nothing else may live there.
proof_files=(
  verification/kernel-proof-v1/Decider/FoldProof.lean
  verification/kernel-proof-v1/Decider/Probe.lean
  verification/kernel-proof-v1/Decider/Reference.lean
  verification/kernel-proof-v1/Decider/StepProof.lean
)
test "$(cd "$repo_root" && find verification/kernel-proof-v1/Decider -name '*.lean' | LC_ALL=C sort)" = \
  "$(printf '%s\n' "${proof_files[@]}")"

test "$(git -C "$aeneas_root" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
test "$(git -C "$aeneas_root/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
test "$("$aeneas" -version)" = 'aeneas 74a460a2'
test "$("$charon" version)" = '0.1.240'
lean_root="$aeneas_root/backends/lean"
test "$(tr -d '\r\n' < "$lean_root/lean-toolchain")" = leanprover/lean4:v4.31.0
export RUSTUP_HOME="$tool_root/rustup" CARGO_HOME="$tool_root/cargo" CARGO_INCREMENTAL=0
rm -rf "$result/baseline" "$result/mutants"
mkdir -p "$result"

# extract <workspace> <out>: translate the production core from <workspace>.
# Each step returns on failure explicitly: callers also run this under `if !`.
extract() {
  local tree="$1" out="$2"
  mkdir -p "$out/DeciderCore" || return
  # The production file itself is the crate root: it depends on core only.
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
    --start-from crate::fold_events --start-from crate::replay_events \
    --start-from crate::run_step --start-from crate::exceeded_cap \
    --start-from crate::exceeded_turn_cap \
    --dest-file "$out/decider_core.llbc" -- --crate-name norn_kernel_decider --crate-type lib \
    --edition 2024 --cap-lints allow "$core_source") || return
  "$aeneas" -backend lean -dest "$out/DeciderCore" -emit-json -split-files -warnings-as-errors \
    "$out/decider_core.llbc"
}

# lean_check <out> <root> <module>: elaborate one module into <out>. Every
# module a proof imports lives in <out> alone, so a mutant's proof can never
# load a baseline olean: the written semantics are elaborated into each <out>.
lean_check() {
  local out="$1" root="$2" module="$3"
  mkdir -p "$(dirname "$out/$module.olean")"
  (cd "$lean_root" && LEAN_PATH="$out" lake env lean -DwarningAsError=true \
    -R "$root" -o "$out/$module.olean" "$root/$module.lean")
}

# generated <out>: the extracted code must elaborate, and the written
# semantics (which never import it) are elaborated beside it.
generated() {
  local out="$1" module
  for module in Types Funs; do
    lean_check "$out" "$out" "DeciderCore/$module" || return
  done
  lean_check "$out" "$proof_root" Decider/Reference
}

(cd "$lean_root" && lake build Aeneas)

baseline="$result/baseline"
extract "$repo_root" "$baseline"
python3 - "$baseline" "$core_source" <<'PYMANIFEST'
import json
import sys
from pathlib import Path

out, source = Path(sys.argv[1]), sys.argv[2]
manifest = json.loads((out / "DeciderCore" / "translation.json").read_text(encoding="utf-8"))
if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
    "74a460a2", "0.1.240", "norn_kernel_decider"
):
    raise SystemExit("unexpected extraction identity")
local = {
    "norn_kernel_decider.fold_events",
    "norn_kernel_decider.fold_events_loop",
    "norn_kernel_decider.fold_events_loop.body",
    "norn_kernel_decider.replay_events",
    "norn_kernel_decider.count_of",
    "norn_kernel_decider.exceeded_cap",
    "norn_kernel_decider.turn_overflow",
    "norn_kernel_decider.exceeded_turn_cap",
    "norn_kernel_decider.run_step",
}
functions = manifest.get("functions", [])
own = {f["lean_name"] for f in functions if f.get("is_local")}
models = {f["lean_name"] for f in functions if not f.get("is_local")}
if own != local:
    raise SystemExit(f"unexpected production functions {sorted(own)}")
for function in functions:
    if function.get("is_local"):
        if function.get("is_opaque") or function.get("source", {}).get("file") != source:
            raise SystemExit(f"{function['lean_name']} is not the production body")
    elif not function.get("is_opaque"):
        raise SystemExit(f"{function['lean_name']} should be a library model")
if {t.get("lean_name") for t in manifest.get("trait_decls", [])} != {
    "norn_kernel_decider.Evolve", "norn_kernel_decider.StepTurn",
}:
    raise SystemExit("unexpected extracted traits")
if manifest.get("globals") not in ([], None) and any(
    g.get("is_local") for g in manifest.get("globals", [])
):
    raise SystemExit("unexpected production globals")
if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / "DeciderCore").glob("*.lean")):
    raise SystemExit("extraction contains sorry")
print("models:", " ".join(sorted(models)) or "none")
PYMANIFEST
generated "$baseline"
proof_output=""
for module in Decider/FoldProof Decider/StepProof; do
  if module_output="$(lean_check "$baseline" "$proof_root" "$module" 2>&1)"; then
    proof_output+="$module_output"$'\n'
  else
    proof_status=$?
    printf '%s\n' "$module_output" >&2
    exit "$proof_status"
  fi
done
printf '%s' "$proof_output"
full="depends on axioms: [propext, Classical.choice, Quot.sound]"
expected_axioms=""
for theorem in fold_events_correspondence replay_events_correspondence fold_events_append \
  replay_events_split; do
  expected_axioms+="'Norn.Kernel.Decider.$theorem' $full"$'\n'
done
expected_axioms+="'Norn.Kernel.Decider.exceeded_cap_correspondence' depends on axioms: [propext]"$'\n'
for theorem in exceeded_turn_cap_correspondence run_step_correspondence run_step_accepted \
  accepted_is_fold live_state_is_replay; do
  expected_axioms+="'Norn.Kernel.Decider.$theorem' $full"$'\n'
done
test "$proof_output" = "$expected_axioms"
# Check the exact propositions, not only the theorem names or an exit code.
cat <<'LEAN' | (cd "$lean_root" && LEAN_PATH="$baseline" lake env lean -DwarningAsError=true --stdin)
import Decider.StepProof
open Aeneas Aeneas.Std Result norn_kernel_decider Norn.Kernel.Decider
example {D S E : Type} (inst : Evolve D S E) (d : D) (events : Slice E) (state : S) :
    fold_events inst d state events = foldLedger (inst.evolve d) state events.val :=
  fold_events_correspondence inst d events state
example {D S E : Type} (inst : Evolve D S E) (d : D) (events : Slice E) :
    replay_events inst d events =
      replayLedger (inst.initial_state d) (inst.evolve d) events.val :=
  replay_events_correspondence inst d events
example {D S E : Type} (inst : Evolve D S E) (d : D) (pre suf whole : Slice E)
    (hsplit : whole.val = pre.val ++ suf.val) :
    (do let mid ← replay_events inst d pre; fold_events inst d mid suf) =
      replay_events inst d whole :=
  replay_events_split inst d pre suf whole hsplit
example (caps counts : Counts) :
    exceeded_cap caps counts = ok ((firstExceeded caps.tally counts.tally).map Cap.core) :=
  exceeded_cap_correspondence caps counts
example (used caps added : Counts) :
    ∃ r, exceeded_turn_cap used caps added = ok r ∧
      r.map (fun e => (e.cap.cap, e.actual.val)) =
        firstTurnExceeded used.tally caps.tally added.tally :=
  exceeded_turn_cap_correspondence used caps added
example {T D R S E : Type} (inst : StepTurn T D R S E) (t : T) (state : S) :
    (do let r ← run_step inst t state; pure (outcome r)) = step (inst.turn t) state :=
  run_step_correspondence inst t state
example {T D R S E Lg : Type} (inst : StepTurn T D R S E) (L : Evolve Lg S E) (l : Lg)
    (ev : S → E → Result S) (s0 last : S)
    (hev : ∀ t, inst.EvolveInst.evolve t = ev)
    (hclone : ∀ t s, inst.clone_state t s = ok s)
    (hlev : L.evolve l = ev) (hinit : L.initial_state l = ok s0)
    (ledger : Slice E) (h : Accepted inst s0 ledger.val last) :
    replay_events L l ledger = ok last :=
  live_state_is_replay inst L l ev s0 last hev hclone hlev hinit ledger h
LEAN
# The concrete probes hold on the production code by evaluation.
lean_check "$baseline" "$proof_root" Decider/Probe
echo "kernel-decider: production fold, replay, step order and envelope proved against the written semantics"

# Mutants are applied to a hard-linked copy of the core file; the edited file
# is replaced, never written through, so the checkout is untouched.
tree="$result/mutants/tree"
mkdir -p "$tree/$(dirname "$core_source")"
cp -l "$repo_root/$core_source" "$tree/$core_source"
python3 - "$tree" "$result/mutants" "$core_source" <<'PYMUTANTS' > "$result/mutants/list"
import sys
from pathlib import Path

tree, root, path = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
writable = (
    "    if !turn.writable() {\n        return Err(StepRefusal::ReadOnlyWorld);\n    }\n"
)
decide = (
    "    let decision = match turn.decide(state) {\n        Ok(decision) => decision,\n"
    "        Err(rejection) => return Err(StepRefusal::Rejected(rejection)),\n    };\n"
)
envelope = (
    "    if let Some(cap) = exceeded_cap(turn.caps(), counts) {\n"
    "        return Err(StepRefusal::EnvelopeExceeded {\n            cap,\n"
    "            actual: count_of(counts, cap),\n        });\n    }\n"
)
perform = (
    "    if counts.commands > 0 && !turn.may_perform() {\n"
    "        return Err(StepRefusal::PerformOutsideMain {\n"
    "            commands: counts.commands,\n        });\n    }\n"
)
events_check = (
    "    if counts.events > caps.events {\n        return Some(EnvelopeCap::Events);\n    }\n"
)
commands_check = (
    "    if counts.commands > caps.commands {\n        return Some(EnvelopeCap::Commands);\n    }\n"
)
mutants = [
    ("fold-drops-first-event", "let mut index: usize = 0;", "let mut index: usize = 1;"),
    ("fold-skips-alternate-events", "index.checked_add(1)", "index.checked_add(2)"),
    ("fold-evolves-each-event-twice", "state = decider.evolve(&state, event);",
     "state = decider.evolve(&decider.evolve(&state, event), event);"),
    ("fold-stops-after-first-event", "        index = next;\n",
     "        index = next;\n        if index > 0 {\n            break;\n        }\n"),
    ("replay-folds-ledger-twice", "fold_events(decider, decider.initial_state(), events)",
     "let state = fold_events(decider, decider.initial_state(), events);\n"
     "    fold_events(decider, state, events)"),
    ("step-decides-before-world-check", writable + decide, decide + writable),
    ("step-skips-envelope", envelope, ""),
    ("step-perform-check-before-envelope", envelope + perform, perform + envelope),
    ("step-perform-refusal-swallowed", "counts.commands > 0 &&", "counts.commands > 1 &&"),
    ("step-evolves-original-state", "fold_events(turn, turn.clone_state(state), turn.events(&decision))",
     "fold_events(turn, turn.clone_state(&turn.initial_state()), turn.events(&decision))"),
    ("envelope-cap-inclusive", "counts.events > caps.events", "counts.events >= caps.events"),
    ("envelope-order-swapped", events_check + commands_check, commands_check + events_check),
    ("turn-overflow-admitted", "None => Some(u64::MAX),", "None => None,"),
    ("turn-cap-inclusive", "if total > cap {", "if total >= cap {"),
]
for name, old, new in mutants:
    source = (tree / path).read_text(encoding="utf-8")
    if source.count(old) != 1:
        raise SystemExit(f"{name}: mutation site not found exactly once")
    staged = root / name / Path(path).name
    staged.parent.mkdir(parents=True)
    staged.write_text(source.replace(old, new), encoding="utf-8")
    print(name, path)
PYMUTANTS
survivors=0
while read -r name path; do
  pristine="$result/mutants/$name.pristine"
  cp "$tree/$path" "$pristine"
  rm "$tree/$path"
  cp "$result/mutants/$name/$(basename "$path")" "$tree/$path"
  out="$result/mutants/$name/out"
  # A mutant must extract and its code must elaborate: only the proof may fail.
  if ! extract "$tree" "$out" > "$result/mutants/$name/extract.log" 2>&1 ||
    ! generated "$out" >> "$result/mutants/$name/extract.log" 2>&1; then
    tail -n 40 "$result/mutants/$name/extract.log" >&2
    echo "mutant $name: did not extract and elaborate; it cannot test the proof" >&2
    exit 1
  fi
  rm "$tree/$path"
  cp "$pristine" "$tree/$path"
  # Record which correspondence proofs the mutant breaks (informational).
  for module in FoldProof StepProof; do
    if lean_check "$out" "$proof_root" "Decider/$module" > "$result/mutants/$name/$module.log" 2>&1; then
      echo "mutant $name: $module elaborates"
    else
      echo "mutant $name: $module fails"
    fi
  done
  # Detected means refuted, not merely unproved: a concrete probe must be
  # evaluated on the mutant and `decide` must prove its proposition false.
  if lean_check "$out" "$proof_root" Decider/Probe > "$result/mutants/$name/Probe.log" 2>&1; then
    echo "mutant $name: every probe still holds (survived)" >&2
    survivors=$((survivors + 1))
  elif grep -q "Probe.lean:[0-9]*:[0-9]*: error: Tactic \`decide\` proved that the proposition" \
    "$result/mutants/$name/Probe.log"; then
    echo "mutant $name: $(grep -o "error: Tactic \`decide\` proved that the proposition" \
      "$result/mutants/$name/Probe.log" | wc -l) probe(s) refuted (detected)"
  else
    tail -n 20 "$result/mutants/$name/Probe.log" >&2
    echo "mutant $name: a probe fails without a refutation; it does not test the proof" >&2
    survivors=$((survivors + 1))
  fi
done < "$result/mutants/list"
test "$(wc -l < "$result/mutants/list")" -eq 14
test "$survivors" -eq 0
echo "kernel-decider: every seeded fold, step-order and envelope mutant is refuted"
