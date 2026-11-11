#!/usr/bin/env bash
# #718 kernel lane K-C: Charon/Aeneas extract the production replay and receipt
# binding cores (crates/norn-kernel/src/replay_decision.rs, receipt_decision.rs)
# and Lean proves them against the written K6/K7 semantics in
# verification/kernel-proof-v1/Binding. Every seeded replay, receipt and commit
# pin mutant must then extract and fail the same proof.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
proof_root="$repo_root/verification/kernel-proof-v1/Binding"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/kernel-binding-aeneas"
replay_source="crates/norn-kernel/src/replay_decision.rs"
receipt_source="crates/norn-kernel/src/receipt_decision.rs"
# The exact Lean sources this gate elaborates; nothing else may live there.
proof_files=(
  verification/kernel-proof-v1/Binding/BindingReference.lean
  verification/kernel-proof-v1/Binding/ReceiptProof.lean
  verification/kernel-proof-v1/Binding/ReplayProof.lean
)
test "$(cd "$repo_root" && find verification/kernel-proof-v1/Binding -name '*.lean' | LC_ALL=C sort)" = \
  "$(printf '%s\n' "${proof_files[@]}")"
# No escape hatches in the proof sources.
if grep -nE '\b(sorry|admit|axiom|unsafe|implemented_by|extern)\b' \
  "${proof_files[@]/#/$repo_root/}"; then
  echo "kernel-binding: proof sources contain an escape hatch" >&2
  exit 1
fi

test "$(git -C "$aeneas_root" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
test "$(git -C "$aeneas_root/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
test "$("$aeneas" -version)" = 'aeneas 74a460a2'
test "$("$charon" version)" = '0.1.240'
lean_root="$aeneas_root/backends/lean"
test "$(tr -d '\r\n' < "$lean_root/lean-toolchain")" = leanprover/lean4:v4.31.0
export RUSTUP_HOME="$tool_root/rustup" CARGO_HOME="$tool_root/cargo" CARGO_INCREMENTAL=0
rm -rf "$result/baseline" "$result/common" "$result/mutants"
mkdir -p "$result/common"

# extract <workspace> <out>: translate both production files, each compiled
# standalone as its own crate root (core only, no surrounding kernel).
# Each step returns on failure explicitly: callers also run this under `if !`.
extract() {
  local tree="$1" out="$2"
  mkdir -p "$out/Replay" "$out/Receipt" || return
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
    --start-from crate::decide_replay --start-from crate::same_call \
    --dest-file "$out/replay.llbc" -- --crate-name norn_kernel_replay --crate-type lib \
    --edition 2024 "$replay_source") || return
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
    --start-from crate::pins_bind --start-from crate::commit_pins_bind \
    --dest-file "$out/receipt.llbc" -- --crate-name norn_kernel_receipt --crate-type lib \
    --edition 2024 "$receipt_source") || return
  "$aeneas" -backend lean -dest "$out/Replay" -emit-json -split-files -warnings-as-errors \
    "$out/replay.llbc" || return
  "$aeneas" -backend lean -dest "$out/Receipt" -emit-json -split-files -warnings-as-errors \
    "$out/receipt.llbc"
}

# lean_check <out> <root> <module>: elaborate one module into <out>.
lean_check() {
  local out="$1" root="$2" module="$3"
  mkdir -p "$(dirname "$out/$module.olean")"
  (cd "$lean_root" && LEAN_PATH="$out:$result/common" lake env lean -DwarningAsError=true \
    -R "$root" -o "$out/$module.olean" "$root/$module.lean")
}

# generated <out>: the extracted code must elaborate.
generated() {
  local out="$1" prefix module
  for prefix in Replay Receipt; do
    for module in Types Funs; do
      lean_check "$out" "$out" "$prefix/$module" || return
    done
  done
}

(cd "$lean_root" && lake build Aeneas)
# The written semantics never import the extraction.
lean_check "$result/common" "$proof_root" BindingReference

baseline="$result/baseline"
extract "$repo_root" "$baseline"
python3 - "$baseline" <<'PYMANIFEST'
import json
import sys
from pathlib import Path

out = Path(sys.argv[1])
expected = {
    "Replay": ("norn_kernel_replay", "crates/norn-kernel/src/replay_decision.rs", {
        "norn_kernel_replay.same_call",
        "norn_kernel_replay.decide_replay",
    }, {
        "norn_kernel_replay.CallIdentity",
        "norn_kernel_replay.ReplayDecision",
    }, {"norn_kernel_replay.RecordedCall"}),
    "Receipt": ("norn_kernel_receipt", "crates/norn-kernel/src/receipt_decision.rs", {
        "norn_kernel_receipt.delivery_is_result",
        "norn_kernel_receipt.state_accepted",
        "norn_kernel_receipt.pins_bind",
        "norn_kernel_receipt.commit_pins_bind",
    }, {
        "norn_kernel_receipt.AcceptedPins",
        "norn_kernel_receipt.ConsumedResult",
        "norn_kernel_receipt.BindingPins",
        "norn_kernel_receipt.CommitPins",
        "norn_kernel_receipt.CommitPinRefusal",
    }, set()),
}
for prefix, (crate, source, functions, types, traits) in expected.items():
    manifest = json.loads((out / prefix / "translation.json").read_text(encoding="utf-8"))
    if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
        "74a460a2", "0.1.240", crate
    ):
        raise SystemExit(f"{prefix}: unexpected extraction identity")
    extracted = manifest.get("functions", [])
    if {f["lean_name"] for f in extracted} != functions:
        raise SystemExit(f"{prefix}: unexpected functions {sorted(f['lean_name'] for f in extracted)}")
    for function in extracted:
        if (not function.get("is_local") or function.get("is_opaque")
                or function.get("source", {}).get("file") != source):
            raise SystemExit(f"{prefix}: {function['lean_name']} is not the production body")
    if {t.get("lean_name") for t in manifest.get("types", [])} != types:
        raise SystemExit(f"{prefix}: unexpected types")
    if {t.get("lean_name") for t in manifest.get("trait_decls", [])} != traits:
        raise SystemExit(f"{prefix}: unexpected trait declarations")
    for collection in ("globals", "trait_impls"):
        if manifest.get(collection) != []:
            raise SystemExit(f"{prefix}: unexpected extracted {collection}")
    if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / prefix).glob("*.lean")):
        raise SystemExit(f"{prefix}: extraction contains sorry")
PYMANIFEST
generated "$baseline"
proof_output=""
for module in ReplayProof ReceiptProof; do
  if module_output="$(lean_check "$baseline" "$proof_root" "$module" 2>&1)"; then
    proof_output+="$module_output"$'\n'
  else
    proof_status=$?
    printf '%s\n' "$module_output" >&2
    exit "$proof_status"
  fi
done
printf '%s' "$proof_output"
expected_axioms="$(printf '%s\n' \
  "'Norn.Kernel.Binding.decide_replay_correspondence' depends on axioms: [propext]" \
  "'Norn.Kernel.Binding.same_call_correct' depends on axioms: [propext, Quot.sound]" \
  "'Norn.Kernel.Binding.replay_decision_k6' depends on axioms: [propext, Quot.sound]" \
  "'Norn.Kernel.Binding.replay_fresh_iff' depends on axioms: [propext]" \
  "'Norn.Kernel.Binding.replay_returns_recorded' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Kernel.Binding.replay_same_iff' depends on axioms: [propext, Quot.sound]" \
  "'Norn.Kernel.Binding.pins_bind_correspondence' depends on axioms: [propext, Quot.sound]" \
  "'Norn.Kernel.Binding.pins_bind_binds' depends on axioms: [propext, Quot.sound]" \
  "'Norn.Kernel.Binding.commit_pins_bind_correct' depends on axioms: [propext, Quot.sound]" \
  "'Norn.Kernel.Binding.commit_pins_bind_ok_iff' depends on axioms: [propext, Quot.sound]")"
test "${proof_output%$'\n'}" = "$expected_axioms"
# Check the exact propositions, not only the theorem names or an exit code.
cat <<'LEAN' | (cd "$lean_root" && LEAN_PATH="$baseline:$result/common" lake env lean -DwarningAsError=true --stdin)
import ReplayProof
import ReceiptProof
open Aeneas.Std Norn.Kernel.Binding
open norn_kernel_replay in
example {R : Type} (inst : RecordedCall R) (same : R → R → Bool)
    (hsame : ∀ a b, inst.same_call a b = .ok (same a b)) (stored : Option (Option R)) (call : R) :
    decide_replay inst stored call = .ok (toDecision (replay same stored call)) :=
  decide_replay_correspondence inst same hsame stored call
open norn_kernel_replay in
example {R : Type} (inst : RecordedCall R) (same : R → R → Bool)
    (hsame : ∀ a b, inst.same_call a b = .ok (same a b)) (stored : Option (Option R)) (call : R) :
    decide_replay inst stored call = .ok .Fresh ↔ stored = none :=
  replay_fresh_iff inst same hsame stored call
open norn_kernel_replay in
example {R : Type} (inst : RecordedCall R) (same : R → R → Bool)
    (hsame : ∀ a b, inst.same_call a b = .ok (same a b))
    (stored : Option (Option R)) (call outcome : R) :
    decide_replay inst stored call = .ok (.Replay outcome) ↔
      stored = some (some outcome) ∧ same outcome call = true :=
  replay_returns_recorded inst same hsame stored call outcome
open norn_kernel_replay in
example {R A O I B : Type} [DecidableEq A] [DecidableEq O] [DecidableEq I] [DecidableEq B]
    (iA : core.cmp.PartialEq A A) (iO : core.cmp.PartialEq O O) (iI : core.cmp.PartialEq I I)
    (iB : core.cmp.PartialEq B B)
    (hA : ∀ a b, iA.eq a b = .ok (decide (a = b))) (hO : ∀ a b, iO.eq a b = .ok (decide (a = b)))
    (hI : ∀ a b, iI.eq a b = .ok (decide (a = b))) (hB : ∀ a b, iB.eq a b = .ok (decide (a = b)))
    (ident : R → CallIdentity A O I B) (inst : RecordedCall R)
    (hinst : ∀ a b, inst.same_call a b = same_call iA iO iI iB (ident a) (ident b))
    (recorded call : R) :
    decide_replay inst (some (some recorded)) call =
      .ok (if callOf (ident recorded) = callOf (ident call) then .Replay recorded else .Conflict) :=
  replay_same_iff iA iO iI iB hA hO hI hB ident inst hinst recorded call
open norn_kernel_receipt in
example {D C : Type} (iD : core.cmp.PartialEq D D) (iC : core.cmp.PartialEq C C)
    (eqD : D → D → Bool) (eqC : C → C → Bool)
    (hD : ∀ a b, iD.eq a b = .ok (eqD a b)) (hC : ∀ a b, iC.eq a b = .ok (eqC a b))
    (p : BindingPins D C) :
    pins_bind iD iC p = .ok (bindsWith eqD eqC (pinsOf p)) :=
  pins_bind_correspondence iD iC eqD eqC hD hC p
open norn_kernel_receipt in
example {D C : Type} [DecidableEq D] [DecidableEq C]
    (iD : core.cmp.PartialEq D D) (iC : core.cmp.PartialEq C C)
    (hD : ∀ a b, iD.eq a b = .ok (decide (a = b))) (hC : ∀ a b, iC.eq a b = .ok (decide (a = b)))
    (p : BindingPins D C) :
    ∃ b, pins_bind iD iC p = .ok b ∧ (b = true ↔ Binds (pinsOf p)) :=
  pins_bind_binds iD iC hD hC p
open norn_kernel_receipt in
example {D R : Type} [DecidableEq D] [DecidableEq R]
    (iD : core.cmp.PartialEq D D) (iR : core.cmp.PartialEq R R)
    (hD : ∀ a b, iD.eq a b = .ok (decide (a = b))) (hR : ∀ a b, iR.eq a b = .ok (decide (a = b)))
    (c : CommitPins D R) :
    commit_pins_bind iD iR c = .ok (toCommitResult (commitBinds (commitOf c))) :=
  commit_pins_bind_correct iD iR hD hR c
LEAN
echo "kernel-binding: production replay and receipt binding cores proved against K6/K7"

# Mutants are applied to a hard-linked copy of the two production files; the
# edited file is replaced, never written through, so the checkout is untouched.
tree="$result/mutants/tree"
mkdir -p "$tree/crates/norn-kernel/src"
ln "$repo_root/$replay_source" "$tree/$replay_source"
ln "$repo_root/$receipt_source" "$tree/$receipt_source"
python3 - "$tree" "$result/mutants" <<'PYMUTANTS' > "$result/mutants/list"
import sys
from pathlib import Path

tree, root = Path(sys.argv[1]), Path(sys.argv[2])
replay = "crates/norn-kernel/src/replay_decision.rs"
receipt = "crates/norn-kernel/src/receipt_decision.rs"
mutants = [
    # K6 replay
    ("replay-empty-key-conflicts", replay,
     "None => ReplayDecision::Fresh,", "None => ReplayDecision::Conflict,"),
    ("replay-unreadable-record-reexecutes", replay,
     "Some(None) => ReplayDecision::Conflict,", "Some(None) => ReplayDecision::Fresh,"),
    ("replay-different-call-replays", replay,
     "if record.same_call(call) {", "if !record.same_call(call) {"),
    ("replay-ignores-input", replay,
     "        && recorded.input == call.input\n", ""),
    ("replay-ignores-actor", replay,
     "        && recorded.actor == call.actor\n", ""),
    ("replay-ignores-rule-binding", replay,
     "        && recorded.rule_binding == call.rule_binding\n", ""),
    # K7 receipt binding
    ("receipt-ignores-requested-digest", receipt,
     "    pins.receipt_digest == pins.requested\n        && ", "    "),
    ("receipt-ignores-attempt", receipt,
     "        && pins.record_attempt == pins.receipt_attempt\n", ""),
    ("receipt-ignores-state-generation", receipt,
     "        && pins.state_generation == pins.record_generation\n", ""),
    ("receipt-accepts-cancelled", receipt,
     "        && !pins.state_cancelled\n", ""),
    ("receipt-consumes-other-generation", receipt,
     "consumed.command == pins.record_command && consumed.generation == pins.record_generation",
     "consumed.command == pins.record_command"),
    ("receipt-binds-unaccepted-state", receipt,
     "                && accepted.generation == pins.record_generation\n        }\n        None => false,",
     "                && accepted.generation == pins.record_generation\n        }\n        None => true,"),
    # RUN-BIND-1 commit pins: a check reordered, dropped or misreported
    ("commit-run-checked-before-manifest", receipt,
     "    let manifest_bound = pins.manifest == pins.declared_manifest;\n"
     "    if !manifest_bound {\n"
     "        return Err(CommitPinRefusal::ManifestMismatch);\n"
     "    }\n"
     "    let run_bound = pins.run == pins.target;\n"
     "    if !run_bound {\n"
     "        return Err(CommitPinRefusal::RunMismatch);\n"
     "    }\n",
     "    let run_bound = pins.run == pins.target;\n"
     "    if !run_bound {\n"
     "        return Err(CommitPinRefusal::RunMismatch);\n"
     "    }\n"
     "    let manifest_bound = pins.manifest == pins.declared_manifest;\n"
     "    if !manifest_bound {\n"
     "        return Err(CommitPinRefusal::ManifestMismatch);\n"
     "    }\n"),
    ("commit-ignores-run", receipt,
     "    let run_bound = pins.run == pins.target;\n    if !run_bound {\n"
     "        return Err(CommitPinRefusal::RunMismatch);\n    }\n", ""),
    ("commit-ignores-state-generation", receipt,
     "pins.record_generation == pins.run_generation\n        && pins.state_generation == pins.run_generation;",
     "pins.record_generation == pins.run_generation;"),
    ("commit-run-mismatch-misreported", receipt,
     "    if !run_bound {\n        return Err(CommitPinRefusal::RunMismatch);",
     "    if !run_bound {\n        return Err(CommitPinRefusal::ManifestMismatch);"),
]
for name, path, old, new in mutants:
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
  refuted=false
  for module in ReplayProof ReceiptProof; do
    if ! lean_check "$out" "$proof_root" "$module" > "$result/mutants/$name/$module.log" 2>&1; then
      refuted=true
    fi
  done
  rm "$tree/$path"
  cp "$pristine" "$tree/$path"
  if $refuted; then
    echo "mutant $name: proof fails (detected)"
  else
    echo "mutant $name: proof still holds (survived)" >&2
    survivors=$((survivors + 1))
  fi
done < "$result/mutants/list"
test "$survivors" -eq 0
echo "kernel-binding: every seeded replay, receipt and commit pin mutant fails the proof"
