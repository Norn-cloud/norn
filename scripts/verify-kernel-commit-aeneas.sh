#!/usr/bin/env bash
# #718 P8 lane K-A: Charon/Aeneas extract the production commit admission
# driver (crates/norn-kernel/src/commit_order.rs) and Lean proves it equals the
# written order in verification/kernel-proof-v1/Commit for every implementation
# of the stage bodies. Every seeded ordering mutant must then extract and fail
# the same proof.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
proof_root="$repo_root/verification/kernel-proof-v1"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/kernel-commit-aeneas"
order_source="crates/norn-kernel/src/commit_order.rs"
# The exact Lean sources this gate elaborates; nothing else may live in Commit/.
proof_files=(
  verification/kernel-proof-v1/Commit/OrderProof.lean
  verification/kernel-proof-v1/Commit/Reference.lean
)
test "$(cd "$repo_root" && find verification/kernel-proof-v1/Commit -name '*.lean' | LC_ALL=C sort)" = \
  "$(printf '%s\n' "${proof_files[@]}")"

test "$(git -C "$aeneas_root" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
test "$(git -C "$aeneas_root/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
test "$("$aeneas" -version)" = 'aeneas 74a460a2'
test "$("$charon" version)" = '0.1.240'
lean_root="$aeneas_root/backends/lean"
test "$(tr -d '\r\n' < "$lean_root/lean-toolchain")" = leanprover/lean4:v4.31.0
export RUSTUP_HOME="$tool_root/rustup" CARGO_HOME="$tool_root/cargo" CARGO_INCREMENTAL=0
rm -rf "$result/baseline" "$result/common" "$result/mutants"
mkdir -p "$result/common"

# extract <workspace> <out>: translate the production driver from <workspace>.
# The file depends on core alone, so it is compiled standalone as its own crate:
# nothing is substituted for it and no other kernel code is extracted.
extract() {
  local tree="$1" out="$2"
  mkdir -p "$out/CommitOrder" || return
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
    --start-from crate::run_stages --dest-file "$out/commit_order.llbc" -- \
    --crate-name norn_kernel_commit_order --crate-type lib --edition 2024 "$order_source") || return
  "$aeneas" -backend lean -dest "$out/CommitOrder" -emit-json -split-files -warnings-as-errors \
    "$out/commit_order.llbc"
}

# lean_check <search> <root> <module> <olean>: elaborate one module against the
# extraction in <search> and the shared written semantics. Each module's olean
# path is explicit, so no output directory shadows a module of another.
lean_check() {
  local search="$1" root="$2" module="$3" olean="$4"
  mkdir -p "$(dirname "$olean")"
  (cd "$lean_root" && LEAN_PATH="$search:$result/common" lake env lean -DwarningAsError=true \
    -R "$root" -o "$olean" "$root/$module.lean")
}

# generated <out>: the extracted code must elaborate.
generated() {
  local out="$1" module
  for module in Types Funs; do
    lean_check "$out" "$out" "CommitOrder/$module" "$out/CommitOrder/$module.olean" || return
  done
}

(cd "$lean_root" && lake build Aeneas)
# The written semantics never import the extraction.
reference_output="$(lean_check "$result/common" "$proof_root" Commit/Reference \
  "$result/common/Commit/Reference.olean")"
printf '%s\n' "$reference_output"
test "$reference_output" = "$(printf '%s\n' \
  "'Norn.Kernel.Commit.commitOrder_nodup' does not depend on any axioms" \
  "'Norn.Kernel.Commit.commitOrder_complete' depends on axioms: [propext]" \
  "'Norn.Kernel.Commit.documented_order' depends on axioms: [propext, Classical.choice, Quot.sound]")"

baseline="$result/baseline"
extract "$repo_root" "$baseline"
python3 - "$baseline" "$order_source" <<'PYMANIFEST'
import json
import sys
from pathlib import Path

out, source = Path(sys.argv[1]), sys.argv[2]
manifest = json.loads((out / "CommitOrder" / "translation.json").read_text(encoding="utf-8"))
if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
    "74a460a2", "0.1.240", "norn_kernel_commit_order"
):
    raise SystemExit("unexpected extraction identity")
functions = manifest.get("functions", [])
if [(f.get("lean_name"), f.get("is_local"), f.get("is_opaque")) for f in functions] != [
    ("norn_kernel_commit_order.run_stages", True, False)
]:
    raise SystemExit(f"unexpected extracted functions {functions}")
expected_items = {
    "types": "norn_kernel_commit_order.CommitStage",
    "trait_decls": "norn_kernel_commit_order.CommitStages",
}
for collection, name in expected_items.items():
    items = manifest.get(collection, [])
    if [item.get("lean_name") for item in items] != [name]:
        raise SystemExit(f"unexpected extracted {collection}: {items}")
for item in [*functions, *(manifest[c][0] for c in expected_items)]:
    if item.get("source", {}).get("file") != source:
        raise SystemExit(f"{item.get('lean_name')} is not bound to {source}")
for collection in ("globals", "trait_impls"):
    if manifest.get(collection) != []:
        raise SystemExit(f"unexpected extracted {collection}")
if any((out / "CommitOrder" / name).exists() for name in ("FunsExternal_Template.lean",
                                                          "TypesExternal_Template.lean")):
    raise SystemExit("extraction needs an unmodelled external")
if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / "CommitOrder").glob("*.lean")):
    raise SystemExit("extraction contains sorry")
PYMANIFEST
generated "$baseline"
if proof_output="$(lean_check "$baseline" "$proof_root" Commit/OrderProof \
  "$result/common/Commit/OrderProof.olean" 2>&1)"; then
  printf '%s\n' "$proof_output"
else
  proof_status=$?
  printf '%s\n' "$proof_output" >&2
  exit "$proof_status"
fi
test "$proof_output" = "$(printf '%s\n' \
  "'Norn.Kernel.Commit.commit_stages_correspondence' does not depend on any axioms" \
  "'Norn.Kernel.Commit.run_stages_records' depends on axioms: [propext]" \
  "'Norn.Kernel.Commit.admission_refusal_first' does not depend on any axioms" \
  "'Norn.Kernel.Commit.admission_pass_next' does not depend on any axioms")"
# Check the exact propositions, not only the theorem names or an exit code.
printf '%s\n' \
  'import Commit.OrderProof' \
  'open Aeneas.Std Norn.Kernel.Commit in' \
  'example {E S : Type} (inst : norn_kernel_commit_order.CommitStages S E) (s : S) :' \
  '    project (norn_kernel_commit_order.run_stages inst s) =' \
  '      admission (stageRun inst) commitOrder s :=' \
  '  commit_stages_correspondence inst s' \
  'open Norn.Kernel.Commit norn_kernel_commit_order in' \
  'example : (none :: commitOrder.map some).map' \
  '    (fun stop => observe (run_stages (recorder (stop.map toCommitStage)) [])) =' \
  '      (none :: commitOrder.map some).map expectedRun := run_stages_records' \
  'open Norn.Kernel.Commit in' \
  'example : commitOrder = [.eventLimit, .shape, .accounting, .delivery, .immutable, .versions,' \
  '    .linkAuthority, .endpoints, .linkValues, .cardinality, .requiredLinks, .acyclic,' \
  '    .invariants, .laws] := rfl' \
  'open Norn.Kernel.Commit in' \
  'example : [Stage.shape, .delivery, .versions, .endpoints, .cardinality].Sublist commitOrder :=' \
  '  documented_order' \
  | (cd "$lean_root" && LEAN_PATH="$baseline:$result/common" lake env lean -DwarningAsError=true --stdin)
echo "kernel-commit: the production admission driver is the written stage order"

# Mutants are applied to a hard-linked copy of the source; the edited file is
# replaced, never written through, so the checkout is untouched.
tree="$result/mutants/tree"
mkdir -p "$tree/crates/norn-kernel"
cp -al "$repo_root/crates/norn-kernel/src" "$tree/crates/norn-kernel/"
python3 - "$tree" "$result/mutants" "$order_source" <<'PYMUTANTS' > "$result/mutants/list"
import sys
from pathlib import Path

tree, root, path = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
shape = "    stages.run(CommitStage::Shape)?;\n"
delivery = "    stages.run(CommitStage::Delivery)?;\n"
versions = "    stages.run(CommitStage::Versions)?;\n"
endpoints = "    stages.run(CommitStage::Endpoints)?;\n"
cardinality = "    stages.run(CommitStage::Cardinality)?;\n"
immutable = "    stages.run(CommitStage::Immutable)?;\n"
laws = "    stages.run(CommitStage::Laws)\n"
mutants = [
    ("order-delivery-before-shape", shape + "    stages.run(CommitStage::Accounting)?;\n" + delivery,
     delivery + shape + "    stages.run(CommitStage::Accounting)?;\n"),
    ("order-cardinality-before-endpoints",
     endpoints + "    stages.run(CommitStage::LinkValues)?;\n" + cardinality,
     cardinality + "    stages.run(CommitStage::LinkValues)?;\n" + endpoints),
    ("skip-immutable", immutable, ""),
    ("swallow-versions-refusal", versions, "    let _ = stages.run(CommitStage::Versions);\n"),
    ("repeat-cardinality", cardinality, cardinality + cardinality),
    ("swallow-laws-refusal", laws, "    let _ = stages.run(CommitStage::Laws);\n    Ok(())\n"),
]
source = (tree / path).read_text(encoding="utf-8")
for name, old, new in mutants:
    if source.count(old) != 1:
        raise SystemExit(f"{name}: mutation site not found exactly once")
    staged = root / name / Path(path).name
    staged.parent.mkdir(parents=True)
    staged.write_text(source.replace(old, new), encoding="utf-8")
    print(name, path)
PYMUTANTS
test "$(wc -l < "$result/mutants/list")" -eq 6
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
  # Detected means refuted, not merely unproved: the kernel must evaluate the
  # recorder proposition for the mutant and find it false.
  if lean_check "$out" "$proof_root" Commit/OrderProof "$out/OrderProof.olean" \
    > "$result/mutants/$name/proof.log" 2>&1; then
    echo "mutant $name: proof still holds (survived)" >&2
    survivors=$((survivors + 1))
  elif grep -q "OrderProof.lean:[0-9]*:[0-9]*: error: Tactic \`decide\` proved that the proposition" \
    "$result/mutants/$name/proof.log"; then
    echo "mutant $name: run_stages_records refuted (detected)"
  else
    tail -n 20 "$result/mutants/$name/proof.log" >&2
    echo "mutant $name: proof fails without a refutation; it does not test the proof" >&2
    survivors=$((survivors + 1))
  fi
done < "$result/mutants/list"
test "$survivors" -eq 0
echo "kernel-commit: every seeded ordering mutant is refuted"
