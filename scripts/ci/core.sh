#!/usr/bin/env bash
set -euo pipefail
phase=${1:-all}
case "$phase" in
  cheap)
    cargo metadata --locked --format-version 1 > /dev/null
    cargo fmt --all -- --check
    (cd tools/norn-ts && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck)
    (cd packages/norn-sdk && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test --timeout 60000)
    python3 scripts/proof_enrollment.py
    python3 scripts/test_proof_enrollment.py
    cargo xtask ci policy
    cargo xtask ci provenance
    exit 0
    ;;
  all) ;;
  *) printf 'Usage: %s [cheap|all]\n' "$0" >&2; exit 2 ;;
esac
upstream=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/norn-upstream.XXXXXX")
trap 'rm -rf -- "$upstream"' EXIT
python3 scripts/importers/erpnext_runtime.py --output "$upstream" --interop-only
export NORN_UPSTREAM_FIXTURE_DIR="$upstream"
(cd tools/norn-ts && bun install --frozen-lockfile --backend copyfile)
bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/core --org org_demo --env dev --out "$upstream/core-artifacts.json"
bun -e 'const artifacts = await Bun.file(process.argv[1]).json(); for (const [id, path] of [["toy.contacts.source", "fixtures/ir/synthetic-contact.source_schema.json"], ["toy.rows", "fixtures/ir/synthetic-rows.mapping.json"]]) { const artifact = artifacts.find((value) => value.artifactId === id); if (!artifact) throw new Error(`missing ${id}`); await Bun.write(path, JSON.stringify(artifact, null, 2) + "\n"); }' "$upstream/core-artifacts.json"
cargo metadata --locked --format-version 1 > /dev/null
cargo fmt --all -- --check
export NORN_CHECKER_WASM_PARITY_OUT="$upstream/native-parity"
cargo test --locked --workspace --no-fail-fast -- --test-threads=1
cargo test --locked -p norn-canonical -p norn-checker
cargo test --locked -p norn-canonical -p norn-checker --features norn-canonical/preserve-order-probe
cargo test --locked -p norn-canonical -p norn-ir -p norn-checker --features serde_json/arbitrary_precision
cargo check --locked --target wasm32-unknown-unknown -p norn-canonical -p norn-ir -p norn-expr -p norn-checker -p norn-graph -p norn-authz -p norn-kernel -p norn-projections -p norn-decisions -p norn-api
cargo clippy --locked --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
cargo check --locked -p norn-cell --no-default-features --target wasm32-unknown-unknown
cargo clippy --locked -p norn-checker-wasm --target wasm32-unknown-unknown -- -D warnings
crates/norn-skeleton-guard/scripts/zen-dependency-tree.sh
crates/norn-skeleton-guard/scripts/baseline-shrink-only.sh "${NORN_CI_BASE:-HEAD}"
crates/norn-checker/scripts/non-code-literals-shrink-only.sh "${NORN_CI_BASE:-HEAD}"
rustup toolchain install nightly-2026-08-18 --profile minimal --no-self-update
cargo install --locked cargo-public-api --version 0.52.0
crates/norn-skeleton-guard/scripts/public-api-boundary.sh nightly-2026-08-18
python3 -m venv "$upstream/venv"
export PATH="$upstream/venv/bin:$PATH"
python3 -m pip install --disable-pip-version-check --no-input jsonschema==4.25.1
python3 scripts/proof_enrollment.py
python3 scripts/test_proof_enrollment.py
cargo install --locked cargo-deny --version 0.19.0
cargo deny --locked check
cargo xtask ci policy
cargo build --locked -p norn-cli
cargo install --locked wasm-bindgen-cli --version 0.2.129
crates/norn-checker-wasm/scripts/build-pkg.sh
export CI=true
export NORN_BIN="${CARGO_TARGET_DIR:-$PWD/target}/debug/norn"
export NORN_CHECKER_WASM_PKG="${CARGO_TARGET_DIR:-$PWD/target}/norn-checker-wasm/pkg"
bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/quickstart --org example --env local --out "$upstream/task.json"
printf "{}\n" > "$upstream/context.json"
"$NORN_BIN" check --artifacts "$upstream/task.json" --context "$upstream/context.json" > "$upstream/check.json"
cargo test --locked -p norn-checker-wasm --test parity -- --test-threads=1
node crates/norn-checker-wasm/tests/parity/parity.mjs "$NORN_CHECKER_WASM_PKG" "$NORN_CHECKER_WASM_PARITY_OUT"
(cd tools/norn-ts && bun install --frozen-lockfile --backend copyfile && bun run typecheck && bun test --timeout 60000)
(cd packages/norn-sdk && bun install --frozen-lockfile --backend copyfile && bun run typecheck && bun test --timeout 60000)
cargo run --locked -p norn-cli -- openapi > sdks/openapi/norn-api.openapi.json
(cd packages/norn-sdk && bun run generate)
python3 -m venv "$upstream/openapi-venv"
"$upstream/openapi-venv/bin/python" -m pip install --disable-pip-version-check --no-input openapi-spec-validator==0.9.0
"$upstream/openapi-venv/bin/python" -m openapi_spec_validator sdks/openapi/norn-api.openapi.json
git diff --exit-code -- sdks/openapi/norn-api.openapi.json packages/norn-sdk/src/generated/norn-api.ts
scripts/bootstrap-verification-tools.sh
scripts/bootstrap-ontology-tools.sh
scripts/bootstrap-rust-extraction-tools.sh
scripts/verify-promoted-subsystems.sh full
cargo xtask ci provenance
mkdir -p "$upstream/distribution"
git archive HEAD | tar -x -C "$upstream/distribution"
cargo xtask ci provenance --root "$upstream/distribution" --distribution --base-root "$PWD"
