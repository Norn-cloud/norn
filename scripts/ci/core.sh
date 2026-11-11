#!/usr/bin/env bash
set -euo pipefail
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
# `all` runs every phase, in this order, in one checkout. CI packs its
# selected tasks in sequential bundles; `test-shards` proves
# that the shards together select exactly the full test set, each test once.
phases=(cheap test doctest features wasm clippy doc guards integration browser formal provenance)
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
      # An affected plan may split the rest into fewer parts.
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
  bun -e 'const artifacts = await Bun.file(process.argv[1]).json(); for (const [id, path] of [["toy.contacts.source", "fixtures/ir/synthetic-contact.source_schema.json"], ["toy.rows", "fixtures/ir/synthetic-rows.mapping.json"]]) { const artifact = artifacts.find((value) => value.artifactId === id); if (!artifact) throw new Error(`missing ${id}`); await Bun.write(path, JSON.stringify(artifact, null, 2) + "\n"); }' "$upstream/core-artifacts.json"
}
use_jsonschema() {
  [[ -x $upstream/venv/bin/python3 ]] || python3 -m venv "$upstream/venv"
  export PATH="$upstream/venv/bin:$PATH"
  python3 -m pip install --disable-pip-version-check --no-input jsonschema==4.25.1 PyYAML==6.0.3
}
run_phase() {
  if [[ $1 != cheap && $1 != test-shards && $1 != browser && " ${phases[*]} " == *" $1 "* ]]; then prepare_inputs; fi
  case "$1" in
    cheap)
      use_jsonschema
      cargo metadata --locked --format-version 1 > /dev/null
      cargo fmt --all -- --check
      (cd tools/norn-ts && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck)
      (cd packages/norn-sdk && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test --timeout 60000)
      (cd packages/norn-demo-ui && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test)
      (cd apps/studio && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test && bun run build)
      python3 verification/coverage-v1/negative_canaries.py --workflow-contract
      python3 scripts/ci/frontier.py self-test
      python3 scripts/proof_enrollment.py
      python3 scripts/test_proof_enrollment.py
      python3 scripts/ci/test_implementation_receipts.py
      python3 scripts/ci/test_contract_diff.py
      python3 scripts/ci/contract_diff.py --ci
      python3 scripts/test_record_new_files.py
      python3 scripts/test_demo_scenarios.py
      python3 scripts/standards_atlas.py --check
      python3 scripts/test_standards_atlas.py
      python3 scripts/metrics/test_metrics.py
      cargo xtask ci policy
      cargo xtask ci provenance
      ;;
    test)
      select_shard "${2:-all}"
      # An affected plan narrows each shard to the reached packages.
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
      cargo check --locked --target wasm32-unknown-unknown -p norn-canonical -p norn-ir -p norn-expr -p norn-checker -p norn-graph -p norn-datalog -p norn-authz -p norn-kernel -p norn-builtins -p norn-projections -p norn-decisions -p norn-api
      cargo check --locked -p norn-cell --no-default-features --target wasm32-unknown-unknown
      cargo clippy --locked -p norn-checker-wasm --target wasm32-unknown-unknown -- -D warnings
      cargo clippy --locked -p norn-cell-do --target wasm32-unknown-unknown -- -D warnings
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
      ensure_nextest
      python3 scripts/check_demo_scenarios.py
      cargo install --locked cargo-deny --version 0.19.0
      cargo deny --locked check
      cargo xtask ci policy
      ;;
    integration)
      cargo build --locked -p norn-cli
      cargo build --locked -p norn-cell-do --bin norn-dev-host
      cargo install --locked wasm-bindgen-cli --version 0.2.129
      crates/norn-checker-wasm/scripts/build-pkg.sh
      export CI=true
      export NORN_BIN="${CARGO_TARGET_DIR:-$PWD/target}/debug/norn"
      export NORN_DEV_HOST_BIN="${CARGO_TARGET_DIR:-$PWD/target}/debug/norn-dev-host"
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
      (cd packages/norn-demo-ui && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test)
      python3 -m venv "$upstream/openapi-venv"
      "$upstream/openapi-venv/bin/python" -m pip install --disable-pip-version-check --no-input openapi-spec-validator==0.9.0
      "$upstream/openapi-venv/bin/python" -m openapi_spec_validator sdks/openapi/norn-api.openapi.json
      git diff --exit-code -- sdks/openapi/norn-api.openapi.json packages/norn-sdk/src/generated/norn-api.ts
      ;;
    browser)
      # Studio browser rows (#1113): Playwright against `vite preview` of this build, with the
      # checker WASM package built into apps/studio/src/checker. Without a config there is
      # nothing to run; browser tests without a config are a broken setup, never a skip.
      if ! compgen -G 'apps/studio/playwright.config.*' > /dev/null; then
        if compgen -G 'apps/studio/e2e/*.e2e.ts' > /dev/null; then
          printf 'browser: apps/studio/e2e has browser tests but no playwright.config\n' >&2
          exit 1
        fi
        printf '::notice::browser: no apps/studio/playwright.config.*; no Studio browser tests to run\n'
        return 0
      fi
      # The checker package needs the wasm32 target. CI's pinned toolchain step installs it;
      # a hand run on a plain toolchain fails here, not deep inside the WASM build.
      local wasm_lib
      wasm_lib=$(rustc --print target-libdir --target wasm32-unknown-unknown 2> /dev/null) || wasm_lib=''
      if [[ -z $wasm_lib || ! -d $wasm_lib ]]; then
        printf 'browser: %s has no wasm32-unknown-unknown target; run under the pinned CI toolchain (dtolnay/rust-toolchain in ci-policy.yml)\n' "$(rustc --version 2> /dev/null || echo 'no rustc')" >&2
        exit 1
      fi
      cargo install --locked wasm-bindgen-cli --version 0.2.129
      crates/norn-checker-wasm/scripts/build-pkg.sh "$PWD/apps/studio/src/checker"
      (
        cd apps/studio
        bun install --frozen-lockfile --ignore-scripts --backend copyfile
        bun run build
        # "No tests found" must fail: a broken testDir or testMatch is never a pass.
        listed=$(bunx playwright test --list | sed -n 's/^Total: \([0-9][0-9]*\) tests\{0,1\} in .*/\1/p')
        [[ ${listed:-0} -gt 0 ]] || { printf 'browser: Playwright lists no tests\n' >&2; exit 1; }
        # A suite of only skipped rows must fail too: require at least one executed test.
        report="$upstream/playwright.json"
        PLAYWRIGHT_JSON_OUTPUT_NAME="$report" bunx playwright test --reporter=list,json
        ran=$(python3 -c 'import json, sys; stats = json.load(open(sys.argv[1]))["stats"]; print(stats.get("expected", 0) + stats.get("flaky", 0))' "$report")
        [[ $ran -gt 0 ]] || { printf 'browser: Playwright ran no tests (every row skipped)\n' >&2; exit 1; }
      )
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
    *) printf 'Usage: %s [all|plan [--affected BASE [HEAD]]|shards|bundle-plan PLAN|bundle-check PLAN|bundle PLAN ID|PHASE [SHARD]] with PHASE one of: %s test-shards\n' "$0" "${phases[*]}" >&2; exit 2 ;;
  esac
}
# Drops Cargo build outputs between bundle tasks (#1059). Only the workspace's own
# target dir is touched: CARGO_TARGET_DIR is canonicalized (symlinks and `..` resolved)
# and anything else (empty, /, $HOME, the workspace or an ancestor, a sibling, a
# symlink out of the workspace) is refused and left in place. Inside it only Cargo's
# profile and target-triple trees (each holds a `.fingerprint`) and `doc` go;
# verification evidence and tools (target/verification-*), symlinks and any other
# content stay.
prune_target() {
  local root target entry name
  root=$(realpath -m -- "$PWD") && target=$(realpath -m -- "${CARGO_TARGET_DIR:-$PWD/target}") || return 0
  if [[ $target != "$root/target" ]]; then
    printf '::warning::refusing to prune %s: only %s/target may be pruned\n' "$target" "$root"
    return 0
  fi
  for entry in "$target"/*; do
    name=${entry##*/}
    [[ -d $entry && ! -L $entry && $name != verification-* ]] || continue
    if [[ $name == doc || -d $entry/.fingerprint ]] || compgen -G "$entry/*/.fingerprint" > /dev/null; then
      rm -rf -- "$entry"
    fi
  done
}
bundle_plan() {
  python3 - "$1" "$2" "${3:-}" "$(list_shards)" "${phases[*]}" <<'PY'
import json, sys
mode, raw, selected, shards, phases = sys.argv[1:]
plan = json.loads(raw)
known = {"test/" + shard for shard in shards.split()} | {
    "phase/" + phase for phase in phases.split() + ["test-shards"]
    if phase not in {"cheap", "test", "formal"}}
# Warm nextest runtimes and phase seconds: Boat run 41541177097;
# heavy-3 and integration: repaired Boat run 41542781995.
weights = {
    "test/heavy-1": 30, "test/heavy-2": 122, "test/heavy-3": 89,
    "test/heavy-4": 43, "test/heavy-5": 27, "test/heavy-6": 29,
    "test/rest-1": 143, "test/rest-2": 59, "test/rest-3": 62,
    "phase/doctest": 6, "phase/features": 239, "phase/wasm": 97,
    "phase/clippy": 75, "phase/doc": 60, "phase/guards": 369,
    "phase/integration": 827, "phase/provenance": 17, "phase/test-shards": 5,
    # Estimate until the first Studio browser rows run in CI (#1113).
    "phase/browser": 600,
}
# Cold test tasks spent 163–193 seconds compiling, before nextest's runtime.
# Charge that once per bundle; later shards reuse the same test binaries.
compile_seconds = 180
def compiles_tests(task):
    return task.startswith("test/") or task == "phase/test-shards"
def cold_cost(task):
    return weights[task] + (compile_seconds if compiles_tests(task) else 0)
def bin_cost(tasks):
    return sum(weights[task] for task in tasks) + (compile_seconds if any(map(compiles_tests, tasks)) else 0)

if set(weights) != known:
    raise SystemExit("bundle task inventory differs from core.sh")
if not isinstance(plan, dict) or plan.get("mode") not in {"full", "affected"}:
    raise SystemExit("invalid bundle plan mode")
tasks = []
for field, prefix in (("shard", "test/"), ("phase", "phase/")):
    values = plan.get(field)
    if not isinstance(values, list) or any(not isinstance(value, str) for value in values):
        raise SystemExit("invalid planned task array")
    tasks.extend(prefix + value for value in values)
if len(tasks) != len(set(tasks)) or set(tasks) - known:
    raise SystemExit("duplicate or unknown planned task")
if plan["mode"] == "full" and set(tasks) != known:
    raise SystemExit("full bundle plan omitted a registered task")
target_seconds = 11 * 60
# Use the fewest jobs that meet the service target. If an indivisible task
# exceeds it, minimize the critical path before allocating additional VMs.
candidates = []
maximum = min(6, max(1, len(tasks) // 2)) if tasks else 0
for count in range(1, maximum + 1):
    bins, costs, compiled = [[] for _ in range(count)], [0] * count, [False] * count
    for task in sorted(tasks, key=lambda task: (-cold_cost(task), task)):
        def added_cost(index):
            return weights[task] + (compile_seconds if compiles_tests(task) and not compiled[index] else 0)
        index = min(range(count), key=lambda index: (costs[index] + added_cost(index), index))
        costs[index] += added_cost(index)
        bins[index].append(task)
        compiled[index] |= compiles_tests(task)
    if len(tasks) >= 2 * count:
        for index in range(count):
            if len(bins[index]) != 1:
                continue
            donor, task = min(
                ((donor, task) for donor in range(count) if len(bins[donor]) > 2 for task in bins[donor]),
                key=lambda move: (bin_cost(bins[index] + [move[1]]) - bin_cost(bins[index]),
                                  -bin_cost(bins[move[0]]), move[1], move[0]))
            bins[donor].remove(task)
            bins[index].append(task)
    costs = [bin_cost(tasks) for tasks in bins]
    candidates.append((max(max(costs), target_seconds), count, bins))
bins = min(candidates, default=(0, 0, []))[2]
bundles = [{"id": f"b{index}", "tasks": tasks} for index, tasks in enumerate(bins) if tasks]
if sorted(task for bundle in bundles for task in bundle["tasks"]) != sorted(tasks):
    raise SystemExit("bundle inventory dropped a planned task")
if mode == "pack":
    plan["bundle"] = bundles
    print(json.dumps(plan))
elif mode in {"check", "tasks"}:
    if plan.get("bundle") != bundles:
        raise SystemExit("bundle inventory differs from the deterministic plan")
    if mode == "tasks":
        matching = [bundle for bundle in bundles if bundle["id"] == selected]
        if len(matching) != 1:
            raise SystemExit("unknown bundle identity")
        print("\n".join(matching[0]["tasks"]))
else:
    raise SystemExit("unknown bundle operation")
PY
}

case "${1:-all}" in
  # PRs and merge groups share the dependency closure. Main/manual runs and
  # unplannable changes run every shard and phase; formal remains a Boat gate.
  plan)
    others=()
    for phase in "${phases[@]}"; do [[ $phase == cheap || $phase == test || $phase == formal ]] || others+=("$phase"); done
    case "${2:-}" in
      '') [[ $# -le 1 ]] || run_phase usage; changed='' ;;
      --affected) [[ $# -ge 3 && $# -le 4 && $3 =~ ^[0-9a-f]{40}$ && ${4:-HEAD} =~ ^([0-9a-f]{40}|HEAD)$ ]] || run_phase usage; changed=$(git diff --name-only --no-renames "$3" "${4:-HEAD}" --) || exit 2 ;;
      *) run_phase usage ;;
    esac
    # Changed paths go on fd 3: a large diff exceeds the per-variable size limit.
    plan=$(python3 - "${2:-full}" "$(list_shards | tr '\n' ' ')" "${others[*]}" "$rest_shards" "${heavy[@]}" 3<<<"$changed" <<'PY'
import json, re, sys, tomllib
from pathlib import Path
mode, shards, phases, rest, *heavy = sys.argv[1:]
rest = int(rest)
def done(plan):
    print(json.dumps(plan))
    sys.exit(0)
def full(reason):
    done({"mode": "full", "shard": shards.split(), "phase": phases.split() + ["test-shards"], "packages": "", "rest_shards": rest, "reason": reason})
if mode == "full":
    full("complete suite")
changed = [line for line in open(3).read().splitlines() if line]
root = Path(".")
# Derive gate paths from the same inventory used by trusted policy.
gate_source = (root / "crates/norn-skeleton-guard/src/ci/mod.rs").read_text()
gate_block = gate_source.split("const GATE_FILES:", 1)[1].split("];", 1)[0]
gates = set(re.findall(r'"([^"]+)"', gate_block))
for path in changed:
    if (path in gates or path.startswith((".github/workflows/", "crates/norn-skeleton-guard/"))
            or Path(path).name in {"Cargo.lock", "Cargo.toml", "rust-toolchain", "rust-toolchain.toml", "lean-toolchain"}):
        full(f"gate, dependency or toolchain changed: {path}")
try:
    workspace = tomllib.loads((root / "Cargo.toml").read_text())
    shared = workspace["workspace"].get("dependencies", {})
    members = {m.rstrip("/"): tomllib.loads((root / m / "Cargo.toml").read_text()) for m in workspace["workspace"]["members"]}
except (OSError, ValueError, KeyError):
    full("workspace dependency graph is unplannable")
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
    # Provenance records change with every file; core checks them.
    elif not path.startswith(".github/ci/provenance/") and not (
            path.endswith(".md") and (path.startswith("docs/") or "/" not in path)):
        outside.append(path)
reached, stack = set(touched) | {"norn-skeleton-guard"}, list(touched)
while stack:
    for user in users[stack.pop()]:
        if user not in reached:
            reached.add(user)
            stack.append(user)
if outside:
    full(f"{len(outside)} unplannable paths, e.g. {outside[0]}")
else:
    reason = f"changed: {', '.join(sorted(touched)) or 'Markdown only'}"
parts = rest if "norn-cell" in reached else 1
selected_heavy = [f"heavy-{i}" for i, expression in enumerate(heavy, 1)
                  if set(re.findall(r"binary_id\(([^:]+)::", expression)) & reached]
# Studio changes already plan in full; a checker change reaches the Studio's WASM package.
browser = ["browser"] if "norn-checker-wasm" in reached else []
done({"mode": "affected", "shard": selected_heavy + [f"rest-{i}" for i in range(1, parts + 1)], "phase": ["guards", "provenance", "clippy"] + browser,
      "packages": "" if reached >= known else " ".join(sorted(reached)), "rest_shards": parts, "reason": reason})
PY
)
    bundle_plan pack "$plan"
    ;;
  bundle-plan) [[ $# -eq 2 ]] || run_phase usage; bundle_plan pack "$2" ;;
  bundle-check) [[ $# -eq 2 ]] || run_phase usage; bundle_plan check "$2" ;;
  bundle)
    [[ $# -eq 3 ]] || run_phase usage
    task_lines=$(bundle_plan tasks "$2" "$3")
    [[ $task_lines == phase/test-shards ]] || prepare_inputs
    pruned=''
    while IFS=/ read -r kind item; do
      task="$kind/$item"
      # Bundle tasks never share build outputs (the planner packs them freely), so
      # a task may drop the previous task's target dir when disk runs low (#1059).
      if [[ -n $pruned ]] && (( $(df -Pk / | awk 'NR == 2 {print $4}') < 40 * 1024 * 1024 )); then
        prune_target
        printf '::notice::pruned target before %s; %s KiB free\n' "$task" "$(df -Pk / | awk 'NR == 2 {print $4}')"
      fi
      pruned=1
      printf '::group::%s\n' "$task"
      (
        task_start=$SECONDS
        trap 'task_status=$?; result=failure; [[ $task_status -ne 0 ]] || result=success; printf "::notice::task=%s result=%s seconds=%s\n::endgroup::\n" "$task" "$result" "$((SECONDS-task_start))"; exit "$task_status"' EXIT
        if [[ $kind == test ]]; then run_phase test "$item"; else run_phase "$item"; fi
      )
    done <<<"$task_lines"
    ;;
  all) [[ $# -le 1 ]] || run_phase usage; for phase in "${phases[@]}"; do run_phase "$phase"; done ;;
  shards) list_shards ;;
  test) [[ $# -le 2 ]] || run_phase usage; run_phase test "${2:-all}" ;;
  *) [[ $# -le 1 ]] || run_phase usage; run_phase "$1" ;;
esac
