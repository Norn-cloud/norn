#!/usr/bin/env bash
# #718 P8 lane K-A: Charon/Aeneas extract the production commit admission
# driver (crates/norn-kernel/src/commit_order.rs) and compare-and-swap core
# (crates/norn-kernel/src/commit_versions.rs), and Lean proves them against the
# written semantics in verification/kernel-proof-v1/Commit for every
# implementation of the stage bodies and of the host lookup. Every seeded
# mutant must then extract and fail its proof.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
proof_root="$repo_root/verification/kernel-proof-v1"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/kernel-commit-aeneas"
order_source="crates/norn-kernel/src/commit_order.rs"
versions_source="crates/norn-kernel/src/commit_versions.rs"
# The exact Lean sources this gate elaborates; nothing else may live in Commit/.
proof_files=(
  verification/kernel-proof-v1/Commit/CasProof.lean
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

# extract <workspace> <out>: translate both production files from <workspace>.
# Each depends on core alone, so it is compiled standalone as its own crate:
# nothing is substituted for it and no other kernel code is extracted.
extract() {
  local tree="$1" out="$2"
  mkdir -p "$out/CommitOrder" "$out/CommitVersions" || return
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
    --start-from crate::run_stages --dest-file "$out/commit_order.llbc" -- \
    --crate-name norn_kernel_commit_order --crate-type lib --edition 2024 "$order_source") || return
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
    --start-from crate::first_conflict --dest-file "$out/commit_versions.llbc" -- \
    --crate-name norn_kernel_commit_versions --crate-type lib --edition 2024 \
    "$versions_source") || return
  "$aeneas" -backend lean -dest "$out/CommitOrder" -emit-json -split-files -warnings-as-errors \
    "$out/commit_order.llbc" || return
  "$aeneas" -backend lean -dest "$out/CommitVersions" -emit-json -split-files \
    -warnings-as-errors "$out/commit_versions.llbc"
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
  local out="$1" prefix module
  for prefix in CommitOrder CommitVersions; do
    for module in Types Funs; do
      lean_check "$out" "$out" "$prefix/$module" "$out/$prefix/$module.olean" || return
    done
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
python3 - "$baseline" "$order_source" "$versions_source" <<'PYMANIFEST'
import json
import sys
from pathlib import Path

out, order_source, versions_source = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
expected = {
    "CommitOrder": ("norn_kernel_commit_order", order_source, [
        "norn_kernel_commit_order.run_stages",
    ], {
        "types": ["norn_kernel_commit_order.CommitStage"],
        "trait_decls": ["norn_kernel_commit_order.CommitStages"],
    }),
    "CommitVersions": ("norn_kernel_commit_versions", versions_source, [
        "norn_kernel_commit_versions.first_conflict_in_loop.body",
        "norn_kernel_commit_versions.first_conflict_in_loop",
        "norn_kernel_commit_versions.first_conflict_in",
        "norn_kernel_commit_versions.first_conflict",
    ], {
        "types": [],
        "trait_decls": ["norn_kernel_commit_versions.CurrentVersion"],
    }),
}
for prefix, (crate, source, functions, items) in expected.items():
    manifest = json.loads((out / prefix / "translation.json").read_text(encoding="utf-8"))
    if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
        "74a460a2", "0.1.240", crate
    ):
        raise SystemExit(f"{prefix}: unexpected extraction identity")
    found = manifest.get("functions", [])
    if sorted((f.get("lean_name"), f.get("is_local"), f.get("is_opaque")) for f in found) != sorted(
        (name, True, False) for name in functions
    ):
        raise SystemExit(f"{prefix}: unexpected extracted functions {found}")
    for collection, names in items.items():
        if [item.get("lean_name") for item in manifest.get(collection, [])] != names:
            raise SystemExit(f"{prefix}: unexpected extracted {collection}")
    for item in [*found, *(i for c in items for i in manifest.get(c, []))]:
        if item.get("source", {}).get("file") != source:
            raise SystemExit(f"{prefix}: {item.get('lean_name')} is not bound to {source}")
    for collection in ("globals", "trait_impls"):
        if manifest.get(collection) != []:
            raise SystemExit(f"{prefix}: unexpected extracted {collection}")
    if any((out / prefix / name).exists() for name in ("FunsExternal_Template.lean",
                                                      "TypesExternal_Template.lean")):
        raise SystemExit(f"{prefix}: extraction needs an unmodelled external")
    if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / prefix).glob("*.lean")):
        raise SystemExit(f"{prefix}: extraction contains sorry")
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
if cas_output="$(lean_check "$baseline" "$proof_root" Commit/CasProof \
  "$result/common/Commit/CasProof.olean" 2>&1)"; then
  printf '%s\n' "$cas_output"
else
  cas_status=$?
  printf '%s\n' "$cas_output" >&2
  exit "$cas_status"
fi
test "$cas_output" = \
  "'Norn.Kernel.Commit.first_conflict_correspondence' depends on axioms: [propext, Classical.choice, Quot.sound]"
printf '%s\n' \
  'import Commit.CasProof' \
  'open Aeneas.Std Norn.Kernel.Commit in' \
  'example {K E C : Type} (copy : core.marker.Copy K)' \
  '    (inst : norn_kernel_commit_versions.CurrentVersion C K E) (versions : C)' \
  '    (reads writes : Slice (K × U64)) :' \
  '    projectCas (norn_kernel_commit_versions.first_conflict copy inst versions reads writes) =' \
  '      firstConflict (currentRun inst) (expectations reads.val ++ expectations writes.val)' \
  '        versions :=' \
  '  first_conflict_correspondence copy inst versions reads writes' \
  | (cd "$lean_root" && LEAN_PATH="$baseline:$result/common" lake env lean -DwarningAsError=true --stdin)
echo "kernel-commit: production compare-and-swap is the written first-conflict scan"

# cas_vectors <extraction> <log>: elaborate the concrete compare-and-swap
# vectors against one extraction. The baseline must satisfy them; a mutant must
# be refuted by them.
vectors="$result/vectors/CasVectors.lean"
mkdir -p "$result/vectors"
cat > "$vectors" <<'LEANVECTORS'
/-
Refutation vectors for the compare-and-swap mutants (#718 P8 K-A). This file is
generated by the gate outside the proof tree. It is evidence that a seeded
mutant differs from the proved core on a concrete input, never a proof:
`native_decide` trusts the compiler (`Lean.ofReduceBool`). Aeneas `loop` is a
`partial_fixpoint`, so the kernel cannot evaluate these runs with `decide`.
-/
import CommitVersions.Funs
open Aeneas Aeneas.Std Result norn_kernel_commit_versions

/-- A recording host: logs each key it reads; `none` in the table is a read failure. -/
def casRecorder (table : List (Nat × Option U64)) : CurrentVersion (List Nat) Nat Unit where
  current log key := ok (match table.lookup key with
    | some (some v) => .Ok v
    | _ => .Err (), log ++ [key])

def natCopy : core.marker.Copy Nat := { cloneInst := { clone := fun x => ok x } }

def reads : Slice (Nat × U64) := ⟨[(0, 1#u64), (1, 2#u64)], by simp; scalar_tac⟩
def writes : Slice (Nat × U64) := ⟨[(2, 3#u64), (3, 4#u64)], by simp; scalar_tac⟩

/-- One run, flattened: `[0] ++ log` for no conflict, `[1, key, expected, current] ++ log`
for a conflict, `[2] ++ log` for a host error, `[3]` for a panic or divergence. -/
def observe (x : Result (core.result.Result (Option (Nat × U64 × U64)) Unit × List Nat)) :
    List Nat :=
  match x with
  | .ok (.Ok none, log) => 0 :: log
  | .ok (.Ok (some (k, e, f)), log) => [1, k, e.val, f.val] ++ log
  | .ok (.Err _, log) => 2 :: log
  | _ => [3]

def run (table : List (Nat × Option U64)) : List Nat :=
  observe (first_conflict natCopy (casRecorder table) [] reads writes)

theorem cas_vectors :
    [run [(0, some 1#u64), (1, some 2#u64), (2, some 3#u64), (3, some 4#u64)],
     run [(0, some 1#u64), (1, some 2#u64), (2, some 3#u64), (3, some 9#u64)],
     run [(0, some 1#u64), (1, some 7#u64), (2, some 3#u64), (3, some 4#u64)],
     run [(0, some 1#u64), (1, some 2#u64), (2, none), (3, some 4#u64)]] =
    [[0, 0, 1, 2, 3],
     [1, 3, 4, 9, 0, 1, 2, 3],
     [1, 1, 2, 7, 0, 1],
     [2, 0, 1, 2]] := by
  native_decide
LEANVECTORS
cas_vectors() {
  local out="$1" log="$2"
  (cd "$lean_root" && LEAN_PATH="$out:$result/common" lake env lean -DwarningAsError=true \
    "$vectors") > "$log" 2>&1
}
if ! cas_vectors "$baseline" "$result/vectors/baseline.log"; then
  cat "$result/vectors/baseline.log" >&2
  echo "kernel-commit: the proved compare-and-swap core fails its own vectors" >&2
  exit 1
fi
echo "kernel-commit: the production compare-and-swap core satisfies the refutation vectors"

# Mutants are applied to a hard-linked copy of the source; the edited file is
# replaced, never written through, so the checkout is untouched.
tree="$result/mutants/tree"
mkdir -p "$tree/crates/norn-kernel"
cp -al "$repo_root/crates/norn-kernel/src" "$tree/crates/norn-kernel/"
python3 - "$tree" "$result/mutants" "$order_source" "$versions_source" <<'PYMUTANTS' \
  > "$result/mutants/list"
import sys
from pathlib import Path

tree, root, path, versions_path = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3], sys.argv[4]
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
cas = [
    ("cas-equal-is-conflict", "if current != expectation.1 {", "if current == expectation.1 {"),
    ("cas-writes-before-reads",
     "first_conflict_in(versions, reads)? {\n        return Ok(Some(conflict));\n    }\n"
     "    first_conflict_in(versions, writes)",
     "first_conflict_in(versions, writes)? {\n        return Ok(Some(conflict));\n    }\n"
     "    first_conflict_in(versions, reads)"),
    ("cas-skip-first", "let mut index: usize = 0;", "let mut index: usize = 1;"),
    ("cas-report-expected", "Some((expectation.0, expectation.1, current))",
     "Some((expectation.0, expectation.1, expectation.1))"),
    ("cas-ignore-writes", "    first_conflict_in(versions, writes)\n", "    let _ = writes;\n    Ok(None)\n"),
    ("cas-swallow-host-error", "let current = versions.current(&expectation.0)?;",
     "let Ok(current) = versions.current(&expectation.0) else {\n            return Ok(None);\n        };"),
]
for file, group in ((path, mutants), (versions_path, cas)):
    source = (tree / file).read_text(encoding="utf-8")
    for name, old, new in group:
        if source.count(old) != 1:
            raise SystemExit(f"{name}: mutation site not found exactly once")
        staged = root / name / Path(file).name
        staged.parent.mkdir(parents=True)
        staged.write_text(source.replace(old, new), encoding="utf-8")
        print(name, file)
PYMUTANTS
test "$(wc -l < "$result/mutants/list")" -eq 12
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
  case "$name" in
    cas-*)
      # Compare-and-swap mutants: the correspondence proof must fail with a
      # Lean error, not by running out of heartbeats, and a concrete vector
      # must show the mutant differs from the proved core.
      if lean_check "$out" "$proof_root" Commit/CasProof "$out/CasProof.olean" \
        > "$result/mutants/$name/proof.log" 2>&1; then
        echo "mutant $name: proof still holds (survived)" >&2
        survivors=$((survivors + 1))
      elif ! grep -q "CasProof.lean:[0-9]*:[0-9]*: error" "$result/mutants/$name/proof.log" ||
        grep -q "maximum number of heartbeats" "$result/mutants/$name/proof.log"; then
        tail -n 20 "$result/mutants/$name/proof.log" >&2
        echo "mutant $name: proof fails only by timeout or outside the proof" >&2
        survivors=$((survivors + 1))
      elif cas_vectors "$out" "$result/mutants/$name/vectors.log" ||
        ! grep -q "CasVectors.lean:[0-9]*:[0-9]*: error: .*native_decide.*evaluated that the proposition" \
          "$result/mutants/$name/vectors.log"; then
        tail -n 20 "$result/mutants/$name/vectors.log" >&2
        echo "mutant $name: unproved but not refuted by a concrete vector" >&2
        survivors=$((survivors + 1))
      else
        echo "mutant $name: first_conflict_correspondence fails and cas_vectors refutes it (detected)"
      fi
      ;;
    *)
      # Detected means refuted, not merely unproved: the kernel must evaluate
      # the recorder proposition for the mutant and find it false.
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
      ;;
  esac
done < "$result/mutants/list"
test "$survivors" -eq 0
echo "kernel-commit: every seeded mutant fails its proof and is refuted"
