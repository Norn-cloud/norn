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
      # A pull request plan may split the rest into fewer parts.
      local parts=${NORN_CI_REST_SHARDS:-$rest_shards}
      [[ $parts =~ ^[1-9][0-9]*$ && $parts -le rest_shards ]] || { printf 'bad NORN_CI_REST_SHARDS: %s\n' "$parts" >&2; exit 2; }
      [[ $index =~ ^[1-9][0-9]*$ && $index -le parts ]] || { printf 'unknown test shard: %s\n' "$1" >&2; exit 2; }
      expression='not ('
      for filter in "${heavy[@]}"; do expression+="$separator($filter)"; separator=' | '; done
      selection+=(-E "$expression)" --partition "hash:$index/$parts")
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
  bun tools/norn-ts/src/cli.ts compile crates/norn-metrics/tests/demand.source.ts --org org_demo --env dev --out "$upstream/metric-demand.json"
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
      # A pull request plan narrows each shard to the affected packages; the
      # merge queue and main always run every test.
      if [[ -n ${NORN_CI_PACKAGES:-} ]]; then
        local package packages='' separator='' index narrowed=''
        for package in $NORN_CI_PACKAGES; do
          [[ $package =~ ^[a-z0-9_-]+$ ]] || { printf 'bad package: %s\n' "$package" >&2; exit 2; }
          packages+="${separator}package(=$package)"; separator=' | '
        done
        for index in "${!selection[@]}"; do
          if [[ ${selection[index]} == -E ]]; then selection[index + 1]="(${selection[index + 1]}) & ($packages)"; narrowed=1; fi
        done
        [[ -n $narrowed ]] || selection+=(-E "$packages")
        # Build only the reached packages instead of the whole workspace.
        if [[ ${selection[0]} == --workspace ]]; then
          selection=("${selection[@]:1}")
          for package in $NORN_CI_PACKAGES; do selection+=(-p "$package"); done
        fi
        selection+=(--no-tests=pass)
      fi
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
    *) printf 'Usage: %s [all|plan [--affected BASE]|shards|PHASE [SHARD]] with PHASE one of: %s test-shards\n' "$0" "${phases[*]}" >&2; exit 2 ;;
  esac
}
case "${1:-all}" in
  # CI's job plan. The full plan (merge queue, main, manual runs): every test
  # shard, and every other phase except `cheap` (the `core` job), plus the
  # shard partition proof. `formal` stays a Boat gate: on hosted runners its
  # Apalache server stalls without a verdict (Norn-cloud/norn-platform#3541).
  # `plan --affected BASE` (pull requests): tests narrow, the full set in the
  # queue. Only the non-heavy tests of the packages a change can reach (by
  # workspace reverse dependencies, dev-dependencies included; every package
  # when a change is outside the workspace crates, other than Markdown outside
  # crates and the provenance manifest), with the skeleton guard always, and
  # no other phase.
  plan)
    others=()
    for phase in "${phases[@]}"; do [[ $phase == cheap || $phase == test || $phase == formal ]] || others+=("$phase"); done
    case "${2:-}" in
      '') [[ $# -le 1 ]] || run_phase usage; changed='' ;;
      --affected) [[ $# -eq 3 && $3 =~ ^[0-9a-f]{40}$ ]] || run_phase usage; changed=$(git diff --name-only --no-renames "$3" HEAD --) || exit 2 ;;
      *) run_phase usage ;;
    esac
    NORN_CI_CHANGED=$changed python3 - "${2:-full}" "$(list_shards | tr '\n' ' ')" "${others[*]}" "$rest_shards" <<'PY'
import json, os, sys, tomllib
from pathlib import Path
mode, shards, phases, rest = sys.argv[1:]
rest = int(rest)
def done(plan):
    print(json.dumps(plan))
    sys.exit(0)
if mode == "full":
    done({"mode": "full", "shard": shards.split(), "phase": phases.split() + ["test-shards"], "packages": "", "rest_shards": rest})
changed = [line for line in os.environ["NORN_CI_CHANGED"].splitlines() if line]
root = Path(".")
workspace = tomllib.loads((root / "Cargo.toml").read_text())
shared = workspace["workspace"].get("dependencies", {})
members = {m.rstrip("/"): tomllib.loads((root / m / "Cargo.toml").read_text()) for m in workspace["workspace"]["members"]}
names = {path: manifest["package"]["name"] for path, manifest in members.items()}
known = set(names.values())
def target(key, spec):
    if isinstance(spec, dict) and spec.get("workspace"):
        spec = shared.get(key, {})
    return spec.get("package", key) if isinstance(spec, dict) else key
users = {name: set() for name in known}
for path, manifest in members.items():
    tables = [manifest] + [t for t in manifest.get("target", {}).values() if isinstance(t, dict)]
    for table in tables:
        for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
            for key, spec in table.get(kind, {}).items():
                if target(key, spec) in known:
                    users[target(key, spec)].add(names[path])
touched, outside = set(), []
for path in changed:
    owner = max((m for m in names if path.startswith(m + "/")), key=len, default=None)
    if owner:
        touched.add(names[owner])
    # The provenance manifest changes with every file; core checks it.
    elif path != ".github/ci/shipped-artifacts.json" and not (
            path.endswith(".md") and (path.startswith("docs/") or "/" not in path)):
        outside.append(path)
reached, stack = set(touched) | {"norn-skeleton-guard"}, list(touched)
while stack:
    for user in users[stack.pop()]:
        if user not in reached:
            reached.add(user)
            stack.append(user)
if outside:
    reached, reason = known, f"{len(outside)} changed paths outside workspace crates, e.g. {outside[0]}"
else:
    reason = f"changed: {', '.join(sorted(touched)) or 'Markdown only'}"
parts = rest if "norn-cell" in reached else 1
done({"mode": "affected", "shard": [f"rest-{i}" for i in range(1, parts + 1)], "phase": [],
      "packages": "" if reached >= known else " ".join(sorted(reached)), "rest_shards": parts, "reason": reason})
PY
    ;;
  all) [[ $# -le 1 ]] || run_phase usage; for phase in "${phases[@]}"; do run_phase "$phase"; done ;;
  shards) list_shards ;;
  test) [[ $# -le 2 ]] || run_phase usage; run_phase test "${2:-all}" ;;
  *) [[ $# -le 1 ]] || run_phase usage; run_phase "$1" ;;
esac
