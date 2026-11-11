#!/usr/bin/env bash
# #717 (D78): Charon/Aeneas extract the production exact-arithmetic core and
# Lean proves it against the written VAL-EXACT-1 / FOLD-ARITHMETIC semantics
# in verification/exact-arith-v1. Every seeded rounding and overflow mutant
# must then extract and fail the same proof.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
proof_root="$repo_root/verification/exact-arith-v1"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/exact-arith-aeneas"
decimal_source="crates/norn-checker/src/quantity_ledger/decimal.rs"
# The exact Lean sources this gate elaborates; nothing else may live there.
proof_files=(
  verification/exact-arith-v1/Decimal/FunsExternal.lean
  verification/exact-arith-v1/DecimalProof.lean
  verification/exact-arith-v1/DecimalReference.lean
  verification/exact-arith-v1/QuotientProof.lean
  verification/exact-arith-v1/RoundingMath.lean
  verification/exact-arith-v1/RoundingReference.lean
  verification/exact-arith-v1/ScaledQuotient/FunsExternal.lean
)
test "$(cd "$repo_root" && find verification/exact-arith-v1 -name '*.lean' | LC_ALL=C sort)" = \
  "$(printf '%s\n' "${proof_files[@]}")"

test "$(git -C "$aeneas_root" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
test "$(git -C "$aeneas_root/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
test "$("$aeneas" -version)" = 'aeneas 74a460a2'
test "$("$charon" version)" = '0.1.240'
lean_root="$aeneas_root/backends/lean"
test "$(tr -d '\r\n' < "$lean_root/lean-toolchain")" = leanprover/lean4:v4.31.0
export RUSTUP_HOME="$tool_root/rustup" CARGO_HOME="$tool_root/cargo" CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="$result/target"
rm -rf "$result/baseline" "$result/common" "$result/mutants"
mkdir -p "$result/common"

# extract <workspace> <out>: translate both fragments from <workspace>'s sources.
# Each step returns on failure explicitly: callers also run this under `if !`.
extract() {
  local tree="$1" out="$2"
  mkdir -p "$out/Decimal" "$out/ScaledQuotient" || return
  # decimal.rs uses pub(super), so it is compiled as the module it is in production.
  printf '#[path = "%s"]\npub mod decimal;\n' "$tree/$decimal_source" > "$out/decimal_root.rs" || return
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
    --start-from crate::decimal::canonical_parts --start-from crate::decimal::checked_pow10 \
    --dest-file "$out/decimal.llbc" -- --crate-name norn_checker_decimal --crate-type lib \
    --edition 2024 "$out/decimal_root.rs") || return
  (cd "$tree" && "$charon" cargo --preset=aeneas --error-on-warnings \
    --start-from crate::scaled::rounded_quotient \
    --dest-file "$out/scaled_quotient.llbc" -- -p zen-expression --lib) || return
  "$aeneas" -backend lean -dest "$out/Decimal" -emit-json -split-files -warnings-as-errors \
    "$out/decimal.llbc" || return
  "$aeneas" -backend lean -dest "$out/ScaledQuotient" -emit-json -split-files \
    -warnings-as-errors "$out/scaled_quotient.llbc" || return
  cp "$proof_root/Decimal/FunsExternal.lean" "$out/Decimal/FunsExternal.lean" || return
  cp "$proof_root/ScaledQuotient/FunsExternal.lean" "$out/ScaledQuotient/FunsExternal.lean"
}

# lean_check <out> <root> <module>: elaborate one module into <out>.
lean_check() {
  local out="$1" root="$2" module="$3"
  mkdir -p "$(dirname "$out/$module.olean")"
  (cd "$lean_root" && LEAN_PATH="$out:$result/common" lake env lean -DwarningAsError=true \
    -R "$root" -o "$out/$module.olean" "$root/$module.lean")
}

# generated <out>: the extracted code and its core models must elaborate.
generated() {
  local out="$1" prefix module
  for prefix in Decimal ScaledQuotient; do
    for module in Types FunsExternal Funs; do
      lean_check "$out" "$out" "$prefix/$module" || return
    done
  done
}

(cd "$lean_root" && lake build Aeneas)
# The written semantics never import the extraction.
for module in DecimalReference RoundingReference RoundingMath; do
  lean_check "$result/common" "$proof_root" "$module"
done

baseline="$result/baseline"
extract "$repo_root" "$baseline"
python3 - "$baseline" <<'PYMANIFEST'
import json
import sys
from pathlib import Path

out = Path(sys.argv[1])
expected = {
    "Decimal": ("norn_checker_decimal", "crates/norn-checker/src/quantity_ledger/decimal.rs", {
        "norn_checker_decimal.decimal.canonical_parts",
        "norn_checker_decimal.decimal.canonical_parts_loop",
        "norn_checker_decimal.decimal.canonical_parts_loop.body",
        "norn_checker_decimal.decimal.checked_pow10",
        "norn_checker_decimal.decimal.checked_pow10_loop",
        "norn_checker_decimal.decimal.checked_pow10_loop.body",
    }, {
        "core.option.Option.Insts.CoreOpsTry_traitTry.branch",
        "core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual",
    }),
    "ScaledQuotient": ("zen_expression", "vendor/zen/core/expression/src/scaled.rs", {
        "zen_expression.scaled.rounded_quotient",
        "zen_expression.scaled.Rounding.Insts.CoreCmpPartialEqRounding.eq",
    }, {
        "core.num.I128.unsigned_abs",
        "core.option.Option.ok_or",
    }),
}
for prefix, (crate, source, local, external) in expected.items():
    manifest = json.loads((out / prefix / "translation.json").read_text(encoding="utf-8"))
    if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
        "74a460a2", "0.1.240", crate
    ):
        raise SystemExit(f"{prefix}: unexpected extraction identity")
    functions = manifest.get("functions", [])
    own = {f["lean_name"] for f in functions if f.get("is_local")}
    models = {f["lean_name"] for f in functions if not f.get("is_local")}
    if own != local or models != external:
        raise SystemExit(f"{prefix}: unexpected functions {sorted(own)} / {sorted(models)}")
    for function in functions:
        if function.get("is_local"):
            if function.get("is_opaque") or function.get("source", {}).get("file") != source:
                raise SystemExit(f"{prefix}: {function['lean_name']} is not the production body")
        elif not function.get("is_opaque"):
            raise SystemExit(f"{prefix}: {function['lean_name']} should be a modelled external")
    for collection in ("globals", "trait_decls", "trait_impls"):
        if manifest.get(collection) != []:
            raise SystemExit(f"{prefix}: unexpected extracted {collection}")
    if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / prefix).glob("*.lean")):
        raise SystemExit(f"{prefix}: extraction contains sorry")
PYMANIFEST
generated "$baseline"
proof_output=""
for module in DecimalProof QuotientProof; do
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
  "'Norn.QuantityLedger.canonical_parts_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.QuantityLedger.canonical_parts_unique' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.QuantityLedger.checked_pow10_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.rounded_quotient_correct' depends on axioms: [propext, Classical.choice, Quot.sound]")"
test "${proof_output%$'\n'}" = "$expected_axioms"
# Check the exact propositions, not only the theorem names or an exit code.
printf '%s\n' \
  'import DecimalProof' \
  'import QuotientProof' \
  'open Aeneas.Std in' \
  'example (u : I128) (s : U8) : ∃ c s'"'"', norn_checker_decimal.decimal.canonical_parts u s = .ok (c, s'"'"') ∧' \
  '    Norn.QuantityLedger.IsCanonicalForm u.val s.val c.val s'"'"'.val :=' \
  '  Norn.QuantityLedger.canonical_parts_correct u s' \
  'open Aeneas.Std in' \
  'example (e : U8) : ∃ r, norn_checker_decimal.decimal.checked_pow10 e = .ok r ∧' \
  '    r.map (fun x => x.val) = Norn.QuantityLedger.pow10Spec e.val :=' \
  '  Norn.QuantityLedger.checked_pow10_correct e' \
  'open Aeneas.Std in' \
  'example (rounding : zen_expression.scaled.Rounding) (n d : I128) :' \
  '    ∃ r, zen_expression.scaled.rounded_quotient rounding n d = .ok r ∧' \
  '      Norn.OrderedFold.project r =' \
  '        Norn.OrderedFold.quantize (Norn.OrderedFold.modeOf rounding) n.val d.val :=' \
  '  Norn.OrderedFold.rounded_quotient_correct rounding n d' \
  | (cd "$lean_root" && LEAN_PATH="$baseline:$result/common" lake env lean -DwarningAsError=true --stdin)
echo "exact-arith: production fragments proved against the written semantics"

# Mutants are applied to a hard-linked copy of the workspace; the edited file is
# replaced, never written through, so the checkout is untouched.
tree="$result/mutants/tree"
mkdir -p "$tree"
cp "$repo_root/Cargo.toml" "$repo_root/Cargo.lock" "$tree/"
cp -al "$repo_root/crates" "$repo_root/vendor" "$repo_root/third_party" "$tree/"
if [[ -d "$repo_root/.cargo" ]]; then cp -al "$repo_root/.cargo" "$tree/"; fi
python3 - "$tree" "$result/mutants" <<'PYMUTANTS' > "$result/mutants/list"
import sys
from pathlib import Path

tree, root = Path(sys.argv[1]), Path(sys.argv[2])
scaled = "vendor/zen/core/expression/src/scaled.rs"
decimal = "crates/norn-checker/src/quantity_ledger/decimal.rs"
mutants = [
    ("rounding-half-even-ties-away", scaled, "magnitude > complement", "magnitude >= complement"),
    ("rounding-parity-inverted", scaled, "quotient % 2 != 0", "quotient % 2 == 0"),
    ("rounding-direction-inverted", scaled,
     "if (numerator < 0) == (denominator < 0) {", "if (numerator < 0) != (denominator < 0) {"),
    ("rounding-toward-zero-rounds", scaled,
     "Rounding::TowardZero => false,", "Rounding::TowardZero => true,"),
    ("rounding-canonical-drops-digit", decimal, "unscaled % 10 == 0", "unscaled % 10 < 5"),
    ("overflow-quotient-unchecked-division", scaled,
     "let quotient = numerator\n        .checked_div(denominator)\n"
     "        .ok_or(QuotientRefusal::DivisorOrOverflow)?;",
     "let quotient = numerator / denominator;"),
    ("overflow-pow10-wrapping", decimal,
     "value = value.checked_mul(10)?;", "value = value.wrapping_mul(10);"),
    ("overflow-pow10-clamped", decimal,
     "value = value.checked_mul(10)?;", "value = value.checked_mul(10).unwrap_or(i128::MAX);"),
    ("overflow-canonical-intermediate-product", decimal,
     "unscaled /= 10;", "unscaled = unscaled * 10 / 100;"),
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
  for module in DecimalProof QuotientProof; do
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
echo "exact-arith: every seeded rounding and overflow mutant fails the proof"
