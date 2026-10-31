#!/usr/bin/env bash
set -euo pipefail
# `all` runs every phase, in this order, in one checkout. CI runs each phase as
# its own job and the `test` phase as the shards below; `test-shards` proves
# that the shards together select exactly the full test set, each test once.
phases=(cheap test doctest features wasm clippy doc guards integration formal provenance)
# Each heavy shard isolates the slowest tests (nextest timings, #3541). The
# rest shards hash-partition every other test.
heavy=(
  'binary_id(norn-expr::check) & test(=generated_accepted_expressions_are_total_and_typed)'
  'binary_id(norn-testkit::kernel_quint) & test(=quantity_v2_quint_traces)'
  'binary_id(norn-cell::quantity_reservations) & test(=partial_resolution_interleaved_lots_conserve_independent_scalar_fold)'
  'binary_id(norn-cell::definitions_install) & test(=first_overflow_column_charges_the_same_empty_object_bytes_in_check_and_install)'
  'binary_id(norn-cell::definitions_install) & (test(=overflow_backfill_refuses_more_than_the_row_limit_in_check_and_install) | test(=removing_an_empty_overflow_property_refuses_an_incomplete_row_scan))'
  '(binary_id(norn-cell::quantity_reservations) & test(=generated_reservation_sequences_match_independent_scalar_fold)) | (binary_id(norn-cell::cell_host) & test(=cell_host_laws_hold_across_reopen_for_the_restricted_engine))'
)
rest_shards=3
# Selection comes only from here: every run ignores nextest's configurable
# default filter and uses the pinned `ci` profile (no retries).
nextest=(cargo nextest --profile ci)
# The pinned nextest, installed like the other tools when absent.
ensure_nextest() {
  cargo nextest --version 2> /dev/null | grep -qF 'cargo-nextest 0.9.150 ' \
    || cargo install --locked cargo-nextest --version 0.9.150
}
list_shards() {
  local index
  for ((index = 1; index <= ${#heavy[@]}; index++)); do printf 'heavy-%s\n' "$index"; done
  for ((index = 1; index <= rest_shards; index++)); do printf 'rest-%s\n' "$index"; done
}
# Sets `selection` to the nextest arguments of shard $1 (`all`: no filter).
select_shard() {
  local index=${1#*-} expression separator=''
  selection=(--workspace --locked --ignore-default-filter)
  case "$1" in
    all) return ;;
    heavy-*) [[ $index =~ ^[1-9][0-9]*$ && $index -le ${#heavy[@]} ]] && selection+=(-E "${heavy[index - 1]}") && return ;;
    rest-*)
      [[ $index =~ ^[1-9][0-9]*$ && $index -le rest_shards ]] || { printf 'unknown test shard: %s\n' "$1" >&2; exit 2; }
      expression='not ('
      for filter in "${heavy[@]}"; do expression+="$separator($filter)"; separator=' | '; done
      selection+=(-E "$expression)" --partition "hash:$index/$rest_shards")
      return ;;
  esac
  printf 'unknown test shard: %s\n' "$1" >&2
  exit 2
}
upstream=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/norn-upstream.XXXXXX")
trap 'rm -rf -- "$upstream"' EXIT
export NORN_CHECKER_WASM_PARITY_OUT="$upstream/native-parity"
# Every phase but `cheap` runs over these generated inputs.
prepared=''
prepare_inputs() {
  [[ -z $prepared ]] || return 0
  prepared=1
  python3 scripts/importers/erpnext_runtime.py --output "$upstream" --interop-only
  export NORN_UPSTREAM_FIXTURE_DIR="$upstream"
  (cd tools/norn-ts && bun install --frozen-lockfile --backend copyfile)
  bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/core --org org_demo --env dev --out "$upstream/core-artifacts.json"
  bun -e 'const artifacts = await Bun.file(process.argv[1]).json(); for (const [id, path] of [["toy.contacts.source", "fixtures/ir/synthetic-contact.source_schema.json"], ["toy.rows", "fixtures/ir/synthetic-rows.mapping.json"]]) { const artifact = artifacts.find((value) => value.artifactId === id); if (!artifact) throw new Error(`missing ${id}`); await Bun.write(path, JSON.stringify(artifact, null, 2) + "\n"); }' "$upstream/core-artifacts.json"
}
use_jsonschema() {
  [[ -x $upstream/venv/bin/python3 ]] || python3 -m venv "$upstream/venv"
  export PATH="$upstream/venv/bin:$PATH"
  python3 -m pip install --disable-pip-version-check --no-input jsonschema==4.25.1
}
run_phase() {
  if [[ $1 != cheap && $1 != test-shards && " ${phases[*]} " == *" $1 "* ]]; then prepare_inputs; fi
  case "$1" in
    cheap)
      cargo metadata --locked --format-version 1 > /dev/null
      cargo fmt --all -- --check
      (cd tools/norn-ts && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck)
      (cd packages/norn-sdk && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test --timeout 60000)
      python3 scripts/proof_enrollment.py
      python3 scripts/test_proof_enrollment.py
      cargo xtask ci policy
      cargo xtask ci provenance
      ;;
    test)
      select_shard "${2:-all}"
      ensure_nextest
      "${nextest[@]}" run --no-fail-fast "${selection[@]}"
      ;;
    test-shards)
      # Every test of the full selection is in exactly one shard.
      ensure_nextest
      select_shard all
      "${nextest[@]}" list "${selection[@]}" --message-format json > "$upstream/all.json"
      local shard
      for shard in $(list_shards); do
        select_shard "$shard"
        "${nextest[@]}" list "${selection[@]}" --message-format json > "$upstream/shard-$shard.json"
      done
      python3 - "$upstream" $(list_shards) <<'PY'
import json, sys
from collections import Counter
def tests(path):
    suites = json.load(open(path))["rust-suites"]
    return [(binary, name) for binary, suite in suites.items()
            for name, case in suite["testcases"].items() if case["filter-match"]["status"] == "matches"]
root, shards = sys.argv[1], sys.argv[2:]
full = tests(f"{root}/all.json")
counts = Counter(test for shard in shards for test in tests(f"{root}/shard-{shard}.json"))
missing = set(full) - set(counts)
extra = set(counts) - set(full)
twice = {test for test, count in counts.items() if count > 1}
for label, found in (("in no shard", missing), ("not in the full set", extra), ("in several shards", twice)):
    for binary, name in sorted(found):
        print(f"test {label}: {binary} {name}", file=sys.stderr)
if not full or missing or extra or twice:
    sys.exit(1)
print(f"{len(shards)} shards select exactly the {len(full)} tests of the full set")
PY
      ;;
    doctest)
      cargo test --locked --workspace --doc
      ;;
    features)
      cargo test --locked -p norn-canonical -p norn-checker
      cargo test --locked -p norn-canonical -p norn-checker --features norn-canonical/preserve-order-probe
      cargo test --locked -p norn-canonical -p norn-ir -p norn-checker --features serde_json/arbitrary_precision
      ;;
    wasm)
      cargo check --locked --target wasm32-unknown-unknown -p norn-canonical -p norn-ir -p norn-expr -p norn-checker -p norn-graph -p norn-authz -p norn-kernel -p norn-projections -p norn-decisions -p norn-api
      cargo check --locked -p norn-cell --no-default-features --target wasm32-unknown-unknown
      cargo clippy --locked -p norn-checker-wasm --target wasm32-unknown-unknown -- -D warnings
      ;;
    clippy) cargo clippy --locked --workspace --all-targets -- -D warnings ;;
    doc) RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps ;;
    guards)
      crates/norn-skeleton-guard/scripts/zen-dependency-tree.sh
      crates/norn-skeleton-guard/scripts/baseline-shrink-only.sh "${NORN_CI_BASE:-HEAD}"
      crates/norn-checker/scripts/non-code-literals-shrink-only.sh "${NORN_CI_BASE:-HEAD}"
      rustup toolchain install nightly-2026-08-18 --profile minimal --no-self-update
      cargo install --locked cargo-public-api --version 0.52.0
      crates/norn-skeleton-guard/scripts/public-api-boundary.sh nightly-2026-08-18
      use_jsonschema
      python3 scripts/proof_enrollment.py
      python3 scripts/test_proof_enrollment.py
      cargo install --locked cargo-deny --version 0.19.0
      cargo deny --locked check
      cargo xtask ci policy
      ;;
    integration)
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
      ;;
    formal)
      export CI=true
      use_jsonschema
      scripts/bootstrap-verification-tools.sh
      scripts/bootstrap-ontology-tools.sh
      scripts/bootstrap-rust-extraction-tools.sh
      scripts/verify-promoted-subsystems.sh full
      ;;
    provenance)
      export CI=true
      cargo xtask ci provenance
      mkdir -p "$upstream/distribution"
      git archive HEAD | tar -x -C "$upstream/distribution"
      cargo xtask ci provenance --root "$upstream/distribution" --distribution --base-root "$PWD"
      ;;
    *) printf 'Usage: %s [all|plan|shards|PHASE [SHARD]] with PHASE one of: %s test-shards\n' "$0" "${phases[*]}" >&2; exit 2 ;;
  esac
}
case "${1:-all}" in
  # CI's job plan: every test shard, and every other phase except `cheap`
  # (the `core` job), plus the shard partition proof. `formal` stays a Boat
  # gate: on hosted runners its Apalache server stalls without a verdict
  # (Norn-cloud/norn-platform#3541).
  plan)
    [[ $# -le 1 ]] || run_phase usage
    others=()
    for phase in "${phases[@]}"; do [[ $phase == cheap || $phase == test || $phase == formal ]] || others+=("$phase"); done
    python3 -c 'import json, sys; print(json.dumps({"shard": sys.argv[1].split(), "phase": sys.argv[2].split() + ["test-shards"]}))' \
      "$(list_shards | tr '\n' ' ')" "${others[*]}"
    ;;
  all) [[ $# -le 1 ]] || run_phase usage; for phase in "${phases[@]}"; do run_phase "$phase"; done ;;
  shards) list_shards ;;
  test) [[ $# -le 2 ]] || run_phase usage; run_phase test "${2:-all}" ;;
  *) [[ $# -le 1 ]] || run_phase usage; run_phase "$1" ;;
esac
