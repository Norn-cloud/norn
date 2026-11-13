#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
output_dir="$repo_root/target/verification-results/artifact-governance-hax"
proof_dir="$repo_root/verification/stripmine-2026-08-22/proofs/fstar"

export CARGO_HOME="$tool_root/cargo"
export RUSTUP_HOME="$tool_root/rustup"
export RUSTUP_TOOLCHAIN="nightly-2025-11-08-x86_64-unknown-linux-gnu"
export PATH="$tool_root/bin:$CARGO_HOME/bin:$PATH"
# Match Hax's normal target location without its unlocked cargo-metadata lookup.
export CARGO_TARGET_DIR="$repo_root/target/hax"

mkdir -p "$output_dir"

cd "$repo_root"
# cargo-hax streams the crate's id table into hax-engine, which held about
# 3.9 GB resident on norn-ir beside the frontend; on an 8 GB machine the kernel
# killed it and cargo-hax reported only "Broken pipe" (#718). Refuse up front.
required_kib=$((12 * 1024 * 1024))
available_kib=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo 2> /dev/null || echo 0)
if (( available_kib < required_kib )); then
  echo "artifact-governance extraction needs at least 12 GiB available memory (hax-engine alone held ~3.9 GB on norn-ir); this machine has $((available_kib / 1024)) MiB. The formal phase runs on a large machine: on Boat, --type large (16 GB), for any change touching formal-covered crates (#718)." >&2
  exit 1
fi
# Attribute pre-existing drift before invoking an extractor.
git -C "$repo_root" diff --exit-code HEAD -- Cargo.lock
cargo-hax --no-custom-target-directory -C --locked -p norn-ir ';' into \
  -i '-** +norn_ir::direct_artifact_write_allowed' \
  --output-dir "$output_dir" \
  fstar
git -C "$repo_root" diff --exit-code HEAD -- Cargo.lock

generated="$output_dir/Norn_ir.fst"
test -s "$generated"
test "$(rg -c '^let direct_artifact_write_allowed ' "$generated")" -eq 1
python3 scripts/check-proof-holes.py --fstar-generated "$generated"

# The generated selection imports two hax compatibility modules. The selected
# function only uses Boolean negation, so this lane supplies the smallest
# reviewed model instead of loading hax's entire, version-sensitive Rust model.
fstar.exe --include "$proof_dir" "$proof_dir/FStar.Mul.fst"
fstar.exe --include "$proof_dir" "$proof_dir/Core_models.fst"
fstar.exe --include "$proof_dir" --include "$output_dir" "$generated"
fstar.exe --include "$proof_dir" --include "$output_dir" \
  "$proof_dir/ArtifactPromotionProof.fst"
