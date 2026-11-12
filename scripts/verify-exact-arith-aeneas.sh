#!/usr/bin/env bash
# #717 (D78): Charon/Aeneas extract the production exact-arithmetic core and
# Lean proves it against the written VAL-EXACT-1 / FOLD-ARITHMETIC /
# FOLD-SCALED / VAL-ROUND / EXACT-RATIONAL-1 semantics in
# verification/exact-arith-v1. Every seeded rounding and overflow mutant must
# then extract, fail the same proof, and be refuted by a concrete vector.
#
# Refutation convention (#718): a mutant counts as detected only when a closed
# proposition about a concrete run evaluates false. Gates whose code the
# kernel can reduce use `decide` in the proof file (#1228). This code cannot:
# the pinned Aeneas machine-integer library and its `partial_fixpoint` loops
# are not kernel-reducible. So this gate generates concrete vectors and decides
# them with `native_decide`, then greps for "Tactic `native_decide` evaluated
# that the proposition ... is false". Every vector must hold on production.
# The vectors live outside the proof tree and are never part of a claim; the
# compiler is trusted only for "this mutant is wrong".
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
proof_root="$repo_root/verification/exact-arith-v1"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/exact-arith-aeneas"
decimal_source="crates/norn-checker/src/quantity_ledger/decimal.rs"
rational_source="crates/norn-checker/src/exact_rational.rs"
# The exact Lean sources this gate elaborates; nothing else may live there.
proof_files=(
  verification/exact-arith-v1/CoreModels.lean
  verification/exact-arith-v1/Decimal/FunsExternal.lean
  verification/exact-arith-v1/DecimalProof.lean
  verification/exact-arith-v1/DecimalReference.lean
  verification/exact-arith-v1/ExactRational/FunsExternal.lean
  verification/exact-arith-v1/LedgerProof.lean
  verification/exact-arith-v1/LedgerReference.lean
  verification/exact-arith-v1/QuotientProof.lean
  verification/exact-arith-v1/RationalProof.lean
  verification/exact-arith-v1/RationalReference.lean
  verification/exact-arith-v1/RoundingMath.lean
  verification/exact-arith-v1/RoundingReference.lean
  verification/exact-arith-v1/Scaled/FunsExternal.lean
  verification/exact-arith-v1/Scaled/TypesExternal.lean
  verification/exact-arith-v1/ScaledProof.lean
  verification/exact-arith-v1/ScaledReference.lean
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

# extract_fragment <workspace> <out> <fragment>: translate one fragment from
# <workspace>'s sources. Each step returns on failure explicitly: callers also
# run this under `if !`.
extract_fragment() {
  local tree="$1" out="$2" fragment="$3"
  mkdir -p "$out/$fragment" || return
  case "$fragment" in
    Decimal)
      # decimal.rs uses pub(super), so it is compiled as the module it is in production.
      printf '#[path = "%s"]\npub mod decimal;\n' "$tree/$decimal_source" > "$out/decimal_root.rs" || return
      (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
        --start-from crate::decimal::canonical_parts --start-from crate::decimal::checked_pow10 \
        --start-from crate::decimal::canonical --start-from crate::decimal::rescale \
        --start-from crate::decimal::checked_add --start-from crate::decimal::checked_sub \
        --start-from crate::decimal::checked_mul \
        --dest-file "$out/decimal.llbc" -- --crate-name norn_checker_decimal --crate-type lib \
        --edition 2024 "$out/decimal_root.rs") || return
      "$aeneas" -backend lean -dest "$out/Decimal" -emit-json -split-files -warnings-as-errors \
        "$out/decimal.llbc" || return
      cp "$proof_root/Decimal/FunsExternal.lean" "$out/Decimal/FunsExternal.lean" ;;
    ExactRational)
      printf '#[path = "%s"]\npub mod exact_rational;\n' "$tree/$rational_source" \
        > "$out/rational_root.rs" || return
      # The whole module: Charon's --start-from does not take inherent methods.
      (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
        --start-from crate::exact_rational \
        --dest-file "$out/exact_rational.llbc" -- --crate-name norn_checker_rational \
        --crate-type lib --edition 2024 "$out/rational_root.rs") || return
      "$aeneas" -backend lean -dest "$out/ExactRational" -emit-json -split-files \
        -warnings-as-errors "$out/exact_rational.llbc" || return
      cp "$proof_root/ExactRational/FunsExternal.lean" "$out/ExactRational/FunsExternal.lean" ;;
    Scaled)
      local core=(rounding_site exact_result_scale exact_binary exact_compare coefficient rescale
        divide binary rounded_quotient)
      (cd "$tree" && "$charon" cargo --preset=aeneas --error-on-warnings \
        "${core[@]/#/--start-from=crate::scaled::}" \
        --dest-file "$out/scaled.llbc" -- -p zen-expression --lib) || return
      "$aeneas" -backend lean -dest "$out/Scaled" -emit-json -split-files \
        -warnings-as-errors "$out/scaled.llbc" || return
      cp "$proof_root/Scaled/TypesExternal.lean" "$out/Scaled/TypesExternal.lean" || return
      cp "$proof_root/Scaled/FunsExternal.lean" "$out/Scaled/FunsExternal.lean" ;;
    *) return 1 ;;
  esac
}

fragments=(Decimal Scaled ExactRational)

# lean_check <out> <root> <module>: elaborate one module into <out>.
lean_check() {
  local out="$1" root="$2" module="$3"
  mkdir -p "$(dirname "$out/$module.olean")"
  (cd "$lean_root" && LEAN_PATH="$out:$result/common" lake env lean -DwarningAsError=true \
    -R "$root" -o "$out/$module.olean" "$root/$module.lean")
}

# generated <out> <fragment>: the extracted code and its core models must elaborate.
generated() {
  local out="$1" fragment="$2" module
  for module in TypesExternal Types FunsExternal Funs; do
    if [[ "$module" != TypesExternal || -f "$out/$fragment/TypesExternal.lean" ]]; then
      lean_check "$out" "$out" "$fragment/$module" || return
    fi
  done
}

(cd "$lean_root" && lake build Aeneas)
# The written semantics never import the extraction.
for module in CoreModels DecimalReference LedgerReference RoundingReference RoundingMath \
  RationalReference ScaledReference; do
  lean_check "$result/common" "$proof_root" "$module"
done

baseline="$result/baseline"
for fragment in "${fragments[@]}"; do
  extract_fragment "$repo_root" "$baseline" "$fragment"
done
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
        "norn_checker_decimal.decimal.canonical",
        "norn_checker_decimal.decimal.rescale",
        "norn_checker_decimal.decimal.align",
        "norn_checker_decimal.decimal.checked_add",
        "norn_checker_decimal.decimal.checked_sub",
        "norn_checker_decimal.decimal.checked_mul",
    }, {
        "core.option.Option.Insts.CoreOpsTry_traitTry.branch",
        "core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual",
    }),
    "Scaled": ("zen_expression", "vendor/zen/core/expression/src/scaled.rs", {
        f"zen_expression.scaled.{name}" for name in (
            "Rounding.Insts.CoreCmpPartialEqRounding.eq", "ScaledArithmetic.quotient",
            "ScaledRefusal.Insts.CoreConvertFromQuotientRefusal.from", "binary", "coefficient",
            "divide", "exact_binary", "exact_compare", "exact_result_scale", "rescale",
            "rounded_quotient", "rounding_site", "to_decimal",
        )
    }, {
        "I64.Insts.CoreConvertFromU32.from",
        "U32.Insts.CoreConvertTryFromU64TryFromIntError.try_from",
        "core.num.I128.pow",
        "core.num.I128.unsigned_abs",
        "core.num.I64.unsigned_abs",
        "core.option.Option.ok_or",
        "core.result.Result.ok",
        "rust_decimal.decimal.Decimal.mantissa",
        "rust_decimal.decimal.Decimal.normalize",
        "rust_decimal.decimal.Decimal.scale",
        "rust_decimal.decimal.Decimal.try_from_i128_with_scale",
    }),
    "ExactRational": ("norn_checker_rational", "crates/norn-checker/src/exact_rational.rs", {
        f"norn_checker_rational.exact_rational.Rational.{name}" for name in (
            "Insts.CoreFmtDebug.fmt", "Insts.CoreCloneClone.clone",
            "Insts.CoreCmpPartialEqRational.eq", "Insts.CoreCmpEq.assert_fields_are_eq",
            "normalize_loop0.body", "normalize_loop0", "normalize_loop1.body", "normalize_loop1",
            "normalize", "new", "add", "mul", "checked_add", "checked_sub", "checked_mul",
            "checked_div", "checked_neg", "cmp",
        )
    }, {
        "I64.Insts.CoreConvertTryFromI128TryFromIntError.try_from",
        "core.num.I128.checked_neg",
        "core.num.I128.checked_abs",
        "core.option.Option.Insts.CoreOpsTry_traitTry.branch",
        "core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual",
        "core.result.Result.ok",
    }),
}
# Only the derived impls of the stored type itself may be extracted.
derived = {
    "Scaled": {"zen_expression.scaled.ScaledRefusal.Insts.CoreConvertFromQuotientRefusal"},
    "ExactRational": {
        f"norn_checker_rational.exact_rational.Rational.Insts.{name}" for name in (
            "CoreFmtDebug", "CoreCloneClone", "CoreMarkerCopy", "CoreMarkerStructuralPartialEq",
            "CoreCmpPartialEqRational", "CoreCmpEq",
        )
    },
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
    for collection in ("globals", "trait_decls"):
        if manifest.get(collection) != []:
            raise SystemExit(f"{prefix}: unexpected extracted {collection}")
    impls = {impl.get("lean_name") for impl in manifest.get("trait_impls", [])}
    if impls != derived.get(prefix, set()):
        raise SystemExit(f"{prefix}: unexpected trait impls {sorted(impls)}")
    if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / prefix).glob("*.lean")):
        raise SystemExit(f"{prefix}: extraction contains sorry")
PYMANIFEST
for fragment in "${fragments[@]}"; do
  generated "$baseline" "$fragment"
done
proof_output=""
for module in DecimalProof LedgerProof QuotientProof RationalProof ScaledProof; do
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
  "'Norn.QuantityLedger.rescale_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.QuantityLedger.checked_add_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.QuantityLedger.checked_sub_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.QuantityLedger.checked_mul_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.rounded_quotient_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.ExactRational.normalize_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.ExactRational.new_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.ExactRational.checked_add_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.ExactRational.checked_sub_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.ExactRational.checked_mul_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.ExactRational.checked_div_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.ExactRational.checked_neg_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.ExactRational.cmp_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.coefficient_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.to_decimal_correct' depends on axioms: [propext, Quot.sound]" \
  "'Norn.OrderedFold.exact_result_scale_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.exact_compare_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.exact_binary_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.rescale_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.divide_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.binary_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.OrderedFold.rounding_site_correct' depends on axioms: [propext, Classical.choice, Quot.sound]")"
test "${proof_output%$'\n'}" = "$expected_axioms"
# Check the exact propositions, not only the theorem names or an exit code.
printf '%s\n' \
  'import DecimalProof' \
  'import LedgerProof' \
  'import QuotientProof' \
  'import RationalProof' \
  'import ScaledProof' \
  'open Aeneas.Std in' \
  'example (u : I128) (s : U8) : ∃ c s'"'"', norn_checker_decimal.decimal.canonical_parts u s = .ok (c, s'"'"') ∧' \
  '    Norn.QuantityLedger.IsCanonicalForm u.val s.val c.val s'"'"'.val :=' \
  '  Norn.QuantityLedger.canonical_parts_correct u s' \
  'open Aeneas.Std in' \
  'example (e : U8) : ∃ r, norn_checker_decimal.decimal.checked_pow10 e = .ok r ∧' \
  '    r.map (fun x => x.val) = Norn.QuantityLedger.pow10Spec e.val :=' \
  '  Norn.QuantityLedger.checked_pow10_correct e' \
  'open Aeneas.Std in' \
  'example (v : norn_checker_decimal.decimal.ScaledInteger) (t : U8) :' \
  '    ∃ r, norn_checker_decimal.decimal.rescale v t = .ok r ∧ Norn.QuantityLedger.projRescale r =' \
  '      Norn.QuantityLedger.rescaleSpec v.unscaled.val v.scale.val t.val :=' \
  '  Norn.QuantityLedger.rescale_correct v t' \
  'example (a b : norn_checker_decimal.decimal.ScaledInteger) :' \
  '    ∃ r, norn_checker_decimal.decimal.checked_add a b = .ok r ∧ Norn.QuantityLedger.Outcome r' \
  '      (Norn.QuantityLedger.addSpec a.unscaled.val a.scale.val b.unscaled.val b.scale.val) :=' \
  '  Norn.QuantityLedger.checked_add_correct a b' \
  'example (a b : norn_checker_decimal.decimal.ScaledInteger) :' \
  '    ∃ r, norn_checker_decimal.decimal.checked_sub a b = .ok r ∧ Norn.QuantityLedger.Outcome r' \
  '      (Norn.QuantityLedger.subSpec a.unscaled.val a.scale.val b.unscaled.val b.scale.val) :=' \
  '  Norn.QuantityLedger.checked_sub_correct a b' \
  'open Aeneas.Std in' \
  'example (a b : norn_checker_decimal.decimal.ScaledInteger) (m : U8) :' \
  '    ∃ r, norn_checker_decimal.decimal.checked_mul a b m = .ok r ∧ Norn.QuantityLedger.Outcome r' \
  '      (Norn.QuantityLedger.mulSpec a.unscaled.val a.scale.val b.unscaled.val b.scale.val m.val) :=' \
  '  Norn.QuantityLedger.checked_mul_correct a b m' \
  'open Aeneas.Std in' \
  'example (rounding : zen_expression.scaled.Rounding) (n d : I128) :' \
  '    ∃ r, zen_expression.scaled.rounded_quotient rounding n d = .ok r ∧' \
  '      Norn.OrderedFold.project r =' \
  '        Norn.OrderedFold.quantize (Norn.OrderedFold.modeOf rounding) n.val d.val :=' \
  '  Norn.OrderedFold.rounded_quotient_correct rounding n d' \
  'open Aeneas.Std in' \
  'example (n d : I128) : ∃ o, norn_checker_rational.exact_rational.Rational.normalize n d = .ok o ∧' \
  '    o.map Norn.ExactRational.components = Norn.ExactRational.normalizeSpec n.val d.val :=' \
  '  Norn.ExactRational.normalize_correct n d' \
  'example (r s : norn_checker_rational.exact_rational.Rational) (hr : 0 < r.denominator.val)' \
  '    (hs : 0 < s.denominator.val) :' \
  '    ∃ o, norn_checker_rational.exact_rational.Rational.checked_div r s = .ok o ∧' \
  '      o.map Norn.ExactRational.components = (if Norn.ExactRational.value s = 0 then none' \
  '        else Norn.ExactRational.represent (Norn.ExactRational.value r / Norn.ExactRational.value s)) :=' \
  '  Norn.ExactRational.checked_div_correct r s hr hs' \
  'example (r s : norn_checker_rational.exact_rational.Rational) (hr : 0 < r.denominator.val)' \
  '    (hs : 0 < s.denominator.val) : norn_checker_rational.exact_rational.Rational.cmp r s =' \
  '      .ok (some (compare (Norn.ExactRational.value r) (Norn.ExactRational.value s))) :=' \
  '  Norn.ExactRational.cmp_correct r s hr hs' \
  'example (profile : zen_expression.scaled.ScaledArithmetic) (v : rust_decimal.decimal.Decimal) :' \
  '    ∃ r, zen_expression.scaled.rescale profile v = .ok r ∧ Norn.OrderedFold.projDec r =' \
  '      Norn.OrderedFold.rescaleSpec (Norn.OrderedFold.modeOf profile.rounding)' \
  '        profile.scale.val (Norn.OrderedFold.decOf v) :=' \
  '  Norn.OrderedFold.rescale_correct profile v' \
  'example (profile : zen_expression.scaled.ScaledArithmetic) (a b : rust_decimal.decimal.Decimal) :' \
  '    ∃ r, zen_expression.scaled.divide profile a b = .ok r ∧ Norn.OrderedFold.projDec r =' \
  '      Norn.OrderedFold.divideSpec (Norn.OrderedFold.modeOf profile.rounding) profile.scale.val' \
  '        a.coeff a.places b.coeff b.places :=' \
  '  Norn.OrderedFold.divide_correct profile a b' \
  | (cd "$lean_root" && LEAN_PATH="$baseline:$result/common" lake env lean -DwarningAsError=true --stdin)
# Refutation vectors: concrete runs of the extracted code, decided by compiled
# evaluation (`native_decide`; the machine-integer library is not
# kernel-reducible, so `decide` cannot run it). They hold on production and
# refute every seeded mutant. They are generated here, never part of a claim,
# and outside the proof tree, whose hole scan rejects `native_decide`.
mkdir -p "$result/vectors"
cat > "$result/vectors/ExactArithVectors.lean" <<'LEANVECTORS'
import Decimal.Funs
import Scaled.Funs
import ExactRational.Funs

open Aeneas Aeneas.Std Result

namespace ExactArithVectors

def pair : Result (Std.I128 × Std.U8) → Option (ℤ × ℕ)
  | ok (c, s) => some (c.val, s.val)
  | _ => none

def optInt : Result (Option Std.I128) → Option (Option ℤ)
  | ok (some v) => some (some v.val)
  | ok none => some none
  | _ => none

def refusable {E : Type} : Result (core.result.Result Std.I128 E) → Option (Option ℤ)
  | ok (.Ok v) => some (some v.val)
  | ok (.Err _) => some none
  | _ => none

def decimal {E : Type} :
    Result (core.result.Result rust_decimal.decimal.Decimal E) → Option (Option (ℤ × ℕ))
  | ok (.Ok d) => some (some (d.coeff, d.places))
  | ok (.Err _) => some none
  | _ => none

def rational :
    Result (Option norn_checker_rational.exact_rational.Rational) → Option (Option (ℤ × ℤ))
  | ok (some r) => some (some (r.numerator.val, r.denominator.val))
  | ok none => some none
  | _ => none

def ordering : Result (Option Ordering) → Option (Option Ordering)
  | ok o => some o
  | _ => none

def dec (c : ℤ) (s : ℕ) (h : -2 ^ 96 < c ∧ c < 2 ^ 96 := by decide) (hs : s ≤ 28 := by decide) :
    rust_decimal.decimal.Decimal := ⟨c, s, h, hs⟩

def amount :
    Result (core.result.Result norn_checker_decimal.decimal.ScaledInteger
      norn_checker_decimal.decimal.ScaledIntegerRefusal) → Option (Option (ℤ × ℕ))
  | ok (.Ok v) => some (some (v.unscaled.val, v.scale.val))
  | ok (.Err _) => some none
  | _ => none

def si (u : Std.I128) (s : Std.U8) : norn_checker_decimal.decimal.ScaledInteger := ⟨u, s⟩

def rat (n d : Std.I64) : norn_checker_rational.exact_rational.Rational := ⟨n, d⟩

open norn_checker_decimal zen_expression norn_checker_rational

theorem canonical_strips : pair (decimal.canonical_parts 1200#i128 3#u8) = some (12, 1) := by
  native_decide
theorem canonical_negative : pair (decimal.canonical_parts (-1200)#i128 3#u8) = some (-12, 1) := by
  native_decide
theorem canonical_zero : pair (decimal.canonical_parts 0#i128 7#u8) = some (0, 0) := by
  native_decide
theorem canonical_keeps_digit : pair (decimal.canonical_parts 1203#i128 3#u8) = some (1203, 3) := by
  native_decide
theorem canonical_wide :
    pair (decimal.canonical_parts 170141183460469231731687303715884105720#i128 1#u8) =
      some (17014118346046923173168730371588410572, 0) := by
  native_decide
theorem pow10_zero : optInt (decimal.checked_pow10 0#u8) = some (some 1) := by native_decide
theorem pow10_max : optInt (decimal.checked_pow10 38#u8) = some (some (10 ^ 38)) := by native_decide
theorem pow10_overflow : optInt (decimal.checked_pow10 39#u8) = some none := by native_decide
theorem ledger_add_aligns :
    amount (decimal.checked_add (si 15#i128 1#u8) (si 2#i128 0#u8)) = some (some (35, 1)) := by
  native_decide
theorem ledger_add_canonical :
    amount (decimal.checked_add (si 15#i128 1#u8) (si 5#i128 1#u8)) = some (some (2, 0)) := by
  native_decide
theorem ledger_sub_order :
    amount (decimal.checked_sub (si 5#i128 0#u8) (si 2#i128 0#u8)) = some (some (3, 0)) := by
  native_decide
theorem ledger_rescale_overflow :
    refusable (decimal.rescale (si 10000000000000000000000000000000000000#i128 0#u8) 2#u8) =
      some none := by
  native_decide
theorem ledger_mul_max_scale :
    amount (decimal.checked_mul (si 1#i128 14#u8) (si 3#i128 14#u8) 28#u8) =
      some (some (3, 28)) := by
  native_decide
theorem ledger_mul_scale_above_max :
    amount (decimal.checked_mul (si 1#i128 14#u8) (si 1#i128 15#u8) 28#u8) = some none := by
  native_decide
theorem ledger_mul_overflow :
    amount (decimal.checked_mul (si 85070591730234615865843651857942052864#i128 0#u8)
      (si 2#i128 0#u8) 28#u8) = some none := by
  native_decide
theorem half_even_tie :
    refusable (scaled.rounded_quotient .HalfEven 5#i128 2#i128) = some (some 2) := by native_decide
theorem half_even_odd_tie :
    refusable (scaled.rounded_quotient .HalfEven 7#i128 2#i128) = some (some 4) := by native_decide
theorem half_away_tie :
    refusable (scaled.rounded_quotient .HalfAwayFromZero 5#i128 2#i128) = some (some 3) := by
  native_decide
theorem half_away_negative_tie :
    refusable (scaled.rounded_quotient .HalfAwayFromZero (-5)#i128 2#i128) = some (some (-3)) := by
  native_decide
theorem toward_zero :
    refusable (scaled.rounded_quotient .TowardZero 7#i128 2#i128) = some (some 3) := by
  native_decide
theorem refuse_inexact :
    refusable (scaled.rounded_quotient .Refuse 7#i128 2#i128) = some none := by native_decide
theorem zero_divisor_refuses :
    refusable (scaled.rounded_quotient .HalfEven 1#i128 0#i128) = some none := by native_decide
theorem coefficient_scale_limit :
    refusable (scaled.coefficient 19#u32 (dec 1 0)) = some none := by native_decide
theorem binary_multiply :
    decimal (scaled.binary ⟨2#u32, .HalfEven⟩ .Multiply (dec 150 2) (dec 200 2)) =
      some (some (300, 2)) := by
  native_decide
theorem divide_aligns :
    decimal (scaled.divide ⟨2#u32, .HalfEven⟩ (dec 10 1) (dec 3 0)) = some (some (33, 2)) := by
  native_decide
theorem normalize_sign :
    rational (exact_rational.Rational.normalize 6#i128 (-4)#i128) = some (some (-3, 2)) := by
  native_decide
theorem normalize_min_denominator :
    rational (exact_rational.Rational.normalize 1#i128
      (-170141183460469231731687303715884105728)#i128) = some none := by
  native_decide
theorem normalize_wide :
    rational (exact_rational.Rational.normalize 9223372036854775808#i128 1#i128) = some none := by
  native_decide
theorem add_exact :
    rational (exact_rational.Rational.checked_add (rat 1#i64 2#i64) (rat 1#i64 3#i64)) =
      some (some (5, 6)) := by
  native_decide
theorem cmp_exact :
    ordering (exact_rational.Rational.cmp (rat 1#i64 3#i64) (rat 1#i64 2#i64)) =
      some (some .lt) := by
  native_decide

end ExactArithVectors
LEANVECTORS
vector_pattern='ExactArithVectors.lean:[0-9]*:[0-9]*: error: Tactic `native_decide` evaluated that the proposition'
# Every vector holds on the production extraction.
lean_check "$baseline" "$result/vectors" ExactArithVectors
echo "exact-arith: production fragments proved against the written semantics"

# Mutants are applied to a hard-linked copy of the workspace; the edited file is
# replaced, never written through, so the checkout is untouched.
tree="$result/mutants/tree"
mkdir -p "$tree"
cp "$repo_root/Cargo.toml" "$repo_root/Cargo.lock" "$tree/"
cp -al "$repo_root/crates" "$repo_root/vendor" "$repo_root/third_party" "$tree/"
if [[ -d "$repo_root/.cargo" ]]; then cp -al "$repo_root/.cargo" "$tree/"; fi
python3 - "$repo_root" "$result/mutants" <<'PYMUTANTS' > "$result/mutants/list"
import sys
from pathlib import Path

source_root, root = Path(sys.argv[1]), Path(sys.argv[2])
scaled = "vendor/zen/core/expression/src/scaled.rs"
decimal = "crates/norn-checker/src/quantity_ledger/decimal.rs"
rational = "crates/norn-checker/src/exact_rational.rs"
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
    ("ledger-add-aligns-to-smaller-scale", decimal,
     "if lhs.scale >= rhs.scale {", "if lhs.scale <= rhs.scale {"),
    ("ledger-add-not-canonical", decimal,
     "Some(unscaled) => Ok(canonical(ScaledInteger { unscaled, scale })),\n"
     "        None => Err(ScaledIntegerRefusal::AdditionOverflow),",
     "Some(unscaled) => Ok(ScaledInteger { unscaled, scale }),\n"
     "        None => Err(ScaledIntegerRefusal::AdditionOverflow),"),
    ("ledger-sub-swapped", decimal, "match lhs.checked_sub(rhs) {", "match rhs.checked_sub(lhs) {"),
    ("ledger-mul-max-scale-off-by-one", decimal,
     "if scale > max_scale {", "if scale >= max_scale {"),
    ("overflow-ledger-rescale-wrapping", decimal,
     "value.unscaled.checked_mul(multiplier)", "Some(value.unscaled.wrapping_mul(multiplier))"),
    ("overflow-ledger-mul-wrapping", decimal,
     "lhs.unscaled.checked_mul(rhs.unscaled)", "Some(lhs.unscaled.wrapping_mul(rhs.unscaled))"),
    ("rational-not-reduced", rational, "break Some(a);", "break Some(1);"),
    ("rational-sign-not-normalized", rational,
     "numerator = numerator.checked_neg()?;", "numerator = numerator.checked_abs()?;"),
    ("rational-add-subtracts", rational,
     ".checked_add(i128::from(rhs.numerator).checked_mul(i128::from(self.denominator))?)?;",
     ".checked_sub(i128::from(rhs.numerator).checked_mul(i128::from(self.denominator))?)?;"),
    ("rational-cmp-wrong-cross-product", rational,
     ".cmp(&i128::from(rhs.numerator).checked_mul(i128::from(self.denominator))?),",
     ".cmp(&i128::from(rhs.numerator).checked_mul(i128::from(rhs.denominator))?),"),
    ("overflow-rational-clamped-store", rational,
     "numerator: i64::try_from(numerator.checked_div(divisor)?).ok()?,",
     "numerator: i64::try_from(numerator.checked_div(divisor)?).ok().unwrap_or(i64::MAX),"),
    ("overflow-rational-unchecked-negation", rational,
     "denominator = denominator.checked_neg()?;", "denominator = -denominator;"),
    ("scaled-coefficient-scale-limit", scaled,
     "    if scale > 18 {\n        return Err(ScaledRefusal::OutsideDeclaredScale);\n    }\n    let places",
     "    if scale > 19 {\n        return Err(ScaledRefusal::OutsideDeclaredScale);\n    }\n    let places"),
    ("scaled-binary-multiply-unscaled", scaled,
     "            factor,\n        )?),\n        ArithmeticOp::Divide",
     "            1,\n        )?),\n        ArithmeticOp::Divide"),
    ("scaled-divide-exponent-swapped", scaled,
     "let exponent = i64::from(profile.scale) + i64::from(b.scale()) - i64::from(a.scale());",
     "let exponent = i64::from(profile.scale) + i64::from(a.scale()) - i64::from(b.scale());"),
]
for index, (name, path, old, new) in enumerate(mutants):
    source = (source_root / path).read_text(encoding="utf-8")
    if source.count(old) != 1:
        raise SystemExit(f"{name}: mutation site not found exactly once")
    staged = root / name / Path(path).name
    staged.parent.mkdir(parents=True)
    staged.write_text(source.replace(old, new), encoding="utf-8")
    print(index, name, path)
PYMUTANTS
# Mutant mode (NORN_EXACT_ARITH_MUTANTS):
# - `affected` (the default, and what CI runs): a mutant edits one source file,
#   which feeds exactly one fragment. Only that fragment is re-extracted and
#   re-elaborated, and only the proofs in its transitive import closure (read
#   from the proof sources) are re-checked. The other fragments are the
#   baseline's, built from the same sources. A mutant whose file feeds no known
#   fragment, or whose fragment no proof imports, counts as unrefuted.
# - `full`: every mutant re-extracts every fragment and re-checks every proof.
#   It is kept to audit `affected`; both gave the same verdict for every mutant
#   on Boat (#717).
# Running the mutants in parallel on a 4-vCPU, 7.8 GiB VM was 2.1 times slower
# than this serial loop: concurrent Mathlib elaboration exhausts memory, not cores.
mutant_mode="${NORN_EXACT_ARITH_MUTANTS:-affected}"
[[ "$mutant_mode" == affected || "$mutant_mode" == full ]]
declare -A fragment_of=(
  ["$decimal_source"]=Decimal
  ["vendor/zen/core/expression/src/scaled.rs"]=Scaled
  ["$rational_source"]=ExactRational
)
proof_modules=(DecimalProof LedgerProof QuotientProof RationalProof ScaledProof)
declare -A proofs_of
while read -r fragment module; do
  proofs_of[$fragment]+="$module "
done < <(python3 - "$proof_root" "${proof_modules[@]}" <<'PYIMPORTS'
import re
import sys
from pathlib import Path

root, modules = Path(sys.argv[1]), sys.argv[2:]
fragments = ("Decimal", "Scaled", "ExactRational")

def imports(module):
    path = root / (module.replace(".", "/") + ".lean")
    if not path.is_file():
        return set()
    return set(re.findall(r"^import (\S+)", path.read_text(encoding="utf-8"), re.M))

def closure(module, seen):
    for name in imports(module):
        if name not in seen:
            seen.add(name)
            closure(name, seen)
    return seen

for module in modules:  # dependency order, as the proof loop above
    for name in sorted(closure(module, set())):
        fragment = name.split(".")[0]
        if fragment in fragments and name.startswith(fragment + "."):
            print(fragment, module)
PYIMPORTS
)
mutant_started=$SECONDS
survivors=0
while read -r _ name path; do
  dir="$result/mutants/$name"
  started=$SECONDS
  fragment="${fragment_of[$path]:-}"
  if [[ -z "$fragment" || -z "${proofs_of[$fragment]:-}" ]]; then
    echo "mutant $name: $path feeds no proved fragment; it cannot be refuted" >&2
    survivors=$((survivors + 1))
    continue
  fi
  if [[ "$mutant_mode" == full ]]; then
    rebuilt=("${fragments[@]}")
    checked="${proof_modules[*]}"
  else
    rebuilt=("$fragment")
    checked="${proofs_of[$fragment]}"
  fi
  pristine="$result/mutants/$name.pristine"
  cp "$tree/$path" "$pristine"
  rm "$tree/$path"
  cp "$dir/$(basename "$path")" "$tree/$path"
  out="$dir/out"
  mkdir -p "$out"
  : > "$dir/extract.log"
  for other in "${fragments[@]}"; do
    if [[ " ${rebuilt[*]} " == *" $other "* ]]; then
      # A mutant must extract and its code must elaborate: only the proof may fail.
      if ! extract_fragment "$tree" "$out" "$other" >> "$dir/extract.log" 2>&1 ||
        ! generated "$out" "$other" >> "$dir/extract.log" 2>&1; then
        tail -n 40 "$dir/extract.log" >&2
        echo "mutant $name: did not extract and elaborate; it cannot test the proof" >&2
        exit 1
      fi
    else
      cp -a "$baseline/$other" "$out/$other"
    fi
  done
  unproved=false
  for module in $checked; do
    if ! lean_check "$out" "$proof_root" "$module" > "$dir/$module.log" 2>&1; then
      unproved=true
    fi
  done
  # Detected means refuted, not merely unproved: a vector must evaluate false.
  # Expected to fail on a mutant; the log is inspected below.
  lean_check "$out" "$result/vectors" ExactArithVectors > "$dir/vectors.log" 2>&1 || true
  rm "$tree/$path"
  cp "$pristine" "$tree/$path"
  if ! $unproved; then
    echo "mutant $name: proof still holds (survived)" >&2
    survivors=$((survivors + 1))
  elif grep -q "$vector_pattern" "$dir/vectors.log"; then
    echo "mutant $name: refuted by $(grep -o "$vector_pattern" "$dir/vectors.log" | wc -l) vector(s) in $((SECONDS - started))s"
  else
    tail -n 20 "$dir/vectors.log" >&2
    echo "mutant $name: unproved but not refuted by any vector" >&2
    survivors=$((survivors + 1))
  fi
done < "$result/mutants/list"
test "$survivors" -eq 0
echo "exact-arith: $(wc -l < "$result/mutants/list") mutants in $((SECONDS - mutant_started))s ($mutant_mode mode)"
echo "exact-arith: every seeded rounding and overflow mutant is refuted"
