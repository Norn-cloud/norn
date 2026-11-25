#!/usr/bin/env bash
# Python isolation (#513): no script directory or cwd on sys.path, and no bytecode read or
# written next to any source. Older Pythons ignore these variables, so the assertion refuses
# to run where they have no effect. scripts/ci/python_isolation.py pins these lines.
export PYTHONSAFEPATH=1
export PYTHONDONTWRITEBYTECODE=1
export PYTHONPYCACHEPREFIX="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/norn-pycache.XXXXXX")"
python3 -c 'import sys; sys.exit(0 if sys.flags.safe_path and sys.dont_write_bytecode and sys.pycache_prefix else 1)' || { echo "Python isolation not honoured" >&2; exit 1; }
set -euo pipefail
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
# `all` runs every phase, in this order, in one checkout. CI packs its
# selected tasks in sequential bundles; `test-shards` proves
# that the shards together select exactly the full test set, each test once.
phases=(cheap test doctest features duckdb wasm clippy doc rivet guards integration-authoring integration-wasm integration-clients browser formal rivet-conformance provenance)
# Each heavy shard isolates the slowest tests (nextest timings, #3541). The
# rest shards hash-partition every other test.
heavy=(
  'binary_id(norn-expr::suites) & test(=check::generated_accepted_expressions_are_total_and_typed)'
  'binary_id(norn-cell) & test(=suites::kernel_quint::quantity_v2_quint_traces)'
  'binary_id(norn-cell) & test(=suites::quantity_reservations::partial_resolution_interleaved_lots_conserve_independent_scalar_fold)'
  'binary_id(norn-cell) & test(=suites::definitions_install::first_overflow_column_charges_the_same_empty_object_bytes_in_check_and_install)'
  'binary_id(norn-cell) & (test(=suites::definitions_install::overflow_backfill_refuses_more_than_the_row_limit_in_check_and_install) | test(=suites::definitions_install::removing_an_empty_overflow_property_refuses_an_incomplete_row_scan))'
  '(binary_id(norn-cell) & test(=suites::quantity_reservations::generated_reservation_sequences_match_independent_scalar_fold)) | (binary_id(norn-cell::suites) & test(=cell_host::cell_host_laws_hold_across_reopen_for_the_restricted_engine))'
)
rest_shards=3
# Selection comes only from here: every run ignores nextest's configurable
# default filter and uses the pinned `ci` profile (no retries).
nextest=(cargo nextest --profile ci)
# The skeleton guard is its own Cargo workspace (#1270): its manifest and lockfile are in the
# gated crate directory. Every guard command names it and shares the root target directory;
# tool installs (`cargo install`) build in their own directory under $upstream instead.
guard=(--manifest-path crates/norn-skeleton-guard/Cargo.toml)
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
# Fresh builds on reused targets: sources extracted with their commit times
# (git archive) can be older than artifacts built from another tree, and cargo
# decides by mtime, so a reused target could keep stale binaries. When the
# tree differs from the one this target last built, touch every file changed
# between the two (every tracked file when the old tree is unknown) and every
# uncommitted change, then stamp the tree. Regeneration refuses a binary older
# than the stamp (require_fresh).
tree_stamp="$CARGO_TARGET_DIR/.norn-built-tree"
refresh_sources() {
  mkdir -p "$CARGO_TARGET_DIR"
  local current previous
  current=$(git rev-parse 'HEAD^{tree}')
  previous=$(cat "$tree_stamp" 2> /dev/null || true)
  if [[ -n $previous ]] && git cat-file -e "$previous^{tree}" 2> /dev/null; then
    [[ $previous == "$current" ]] || git diff --name-only -z "$previous" "$current" | xargs -0 -r touch -c --
  else
    git ls-files -z | xargs -0 -r touch -c --
  fi
  { git diff --name-only -z HEAD; git ls-files -z --others --exclude-standard; } | xargs -0 -r touch -c --
  # The stamp changes only with the tree, so an up-to-date binary stays newer.
  [[ $previous == "$current" ]] || printf '%s\n' "$current" > "$tree_stamp"
}
refresh_sources
# Keep the hosted planner independent of a Cargo build.
case "${1:-}" in
  bundle|bundle-plan|bundle-check) exec cargo xtask ci "$@" ;;
esac
# Fails unless binary $1 was built after this tree was stamped.
require_fresh() {
  [[ -x $1 && $1 -nt $tree_stamp ]] || { echo "core.sh: $1 is older than this tree; rebuild it" >&2; exit 1; }
}
# Pinned cargo tools: a binary that already reports the exact pinned version (CI preinstalls
# checksummed release binaries) is used as is; otherwise the same version is built from source.
ensure_tool() {
  local crate=$1 version=$2 binary=$3
  [[ $("$binary" --version 2> /dev/null) == "${4:-$binary} $version" ]] \
    || CARGO_TARGET_DIR="$upstream/cargo-install" cargo install --locked "$crate" --version "$version"
}
# The pinned nextest, installed like the other tools when absent.
ensure_nextest() {
  cargo nextest --version 2> /dev/null | grep -qF 'cargo-nextest 0.9.150 ' \
    || CARGO_TARGET_DIR="$upstream/cargo-install" cargo install --locked cargo-nextest --version 0.9.150
}
# Cached WASM packages are keyed by the committed tree ($tree_stamp). Any uncommitted edit
# or untracked file under Rust/WASM build inputs invalidates reuse so local runs never serve
# stale WASM, while non-Rust generated outputs (e.g. fixtures/ir, apps/studio) do not.
wasm_sources_clean() {
  local dirty
  dirty=$(git status --porcelain -- crates/ vendor/ third_party/ Cargo.toml Cargo.lock .cargo/ rust-toolchain.toml) || return 1
  [[ -z $dirty ]]
}
# Build or reuse the canonical checker/engine WASM package for this tree inside
# $CARGO_TARGET_DIR/norn-checker-wasm/{pkg,engine}. Inside a bundle, WasmPkg tasks
# run consecutively without pruning target/, so the first builds once and the rest reuse it.
ensure_checker_wasm_pkg() {
  local out_dir=${1:-}
  local canonical="$CARGO_TARGET_DIR/norn-checker-wasm/pkg"
  local clean_stamp="$CARGO_TARGET_DIR/norn-checker-wasm/.pkg-clean"
  local wasm="$canonical/norn_checker_wasm_bg.wasm"
  if ! wasm_sources_clean || [[ ! -s "$clean_stamp" || ! -f "$wasm" || ! -f "$canonical/package.json" || ! -f "$canonical/SIZE.txt" || ! "$canonical/SIZE.txt" -nt "$tree_stamp" ]] \
    || ! sha256sum --check --status "$clean_stamp" < "$wasm"; then
    ensure_tool wasm-bindgen-cli 0.2.129 wasm-bindgen
    rm -f "$clean_stamp" "$canonical/SIZE.txt"
    crates/norn-checker-wasm/scripts/build-pkg.sh "$canonical"
    if wasm_sources_clean; then sha256sum < "$wasm" > "$clean_stamp"; fi
  fi
  if [[ -n $out_dir && $out_dir != "$canonical" ]]; then
    python3 -c 'import pathlib, shutil, sys; src, dst = map(pathlib.Path, sys.argv[1:3]); shutil.rmtree(dst, ignore_errors=True); dst.parent.mkdir(parents=True, exist_ok=True); shutil.copytree(src, dst)' "$canonical" "$out_dir"
  fi
}
ensure_checker_wasm_engine() {
  local out_dir=${1:-}
  local canonical="$CARGO_TARGET_DIR/norn-checker-wasm/engine"
  local clean_stamp="$CARGO_TARGET_DIR/norn-checker-wasm/.engine-clean"
  local wasm="$canonical/norn_engine_wasm_bg.wasm"
  if ! wasm_sources_clean || [[ ! -s "$clean_stamp" || ! -f "$wasm" || ! -f "$canonical/package.json" || ! -f "$canonical/SIZE.txt" || ! "$canonical/SIZE.txt" -nt "$tree_stamp" ]] \
    || ! sha256sum --check --status "$clean_stamp" < "$wasm"; then
    ensure_tool wasm-bindgen-cli 0.2.129 wasm-bindgen
    rm -f "$clean_stamp" "$canonical/SIZE.txt"
    NORN_WASM_ENGINE=1 crates/norn-checker-wasm/scripts/build-pkg.sh "$canonical"
    if wasm_sources_clean; then sha256sum < "$wasm" > "$clean_stamp"; fi
  fi
  if [[ -n $out_dir && $out_dir != "$canonical" ]]; then
    python3 -c 'import pathlib, shutil, sys; src, dst = map(pathlib.Path, sys.argv[1:3]); shutil.rmtree(dst, ignore_errors=True); dst.parent.mkdir(parents=True, exist_ok=True); shutil.copytree(src, dst)' "$canonical" "$out_dir"
  fi
}
# Fails unless each named test of binary $2 ran and passed in nextest's JUnit report $1:
# absent (renamed, deleted, ignored, cfg'd out), skipped, failed or an unreadable report fails.
require_passed() {
  python3 - "$@" << 'PY'
import sys
import xml.etree.ElementTree as ET
report, binary, *names = sys.argv[1:]
cases = {case.get("name"): case for suite in ET.parse(report).getroot().iter("testsuite")
         if suite.get("name") == binary for case in suite.iter("testcase")}
bad = [name for name in names if name not in cases or any(
    child.tag in ("failure", "error", "skipped", "flakyFailure", "rerunFailure") for child in cases[name])]
if not names or bad:
    sys.exit(f"{report}: {binary} tests did not run and pass: {bad or 'none named'}")
PY
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
# CI telemetry: when NORN_CI_SPANS names a file, every phase and bundle task appends one JSON
# line (task, commit, PR, VM, start and end in ns, exit code, sccache hit and miss deltas) that
# a collector ships off the machine. Recording never changes a phase's result or exit status.
span_task='' span_start=0 span_counts=''
# Only this invocation records: nested core.sh runs (the canaries call it) inherit no span file.
spans_file=${NORN_CI_SPANS:-}
unset NORN_CI_SPANS
sccache_counts() {
  [[ ${RUSTC_WRAPPER:-} == *sccache* ]] || command -v sccache > /dev/null 2>&1 || return 0
  sccache --show-stats --stats-format=json 2> /dev/null | python3 -c 'import json, sys
stats = json.load(sys.stdin)["stats"]
print(*(sum(stats[key]["counts"].values()) for key in ("cache_hits", "cache_misses")))' 2> /dev/null || true
}
span_begin() {
  [[ -n $spans_file ]] || return 0
  span_task=$1 span_start=$(date +%s%N) span_counts=$(sccache_counts)
  [[ $span_start =~ ^[0-9]+$ ]] || span_task=''
}
span_end() {
  [[ -n $spans_file && -n $span_task ]] || return 0
  local end hits='' misses='' after phase=${span_task#*/} shard=''
  [[ $span_task != test/* ]] || { shard=$phase; phase=test; }
  end=$(date +%s%N)
  [[ $end =~ ^[0-9]+$ ]] || { span_task=''; return 0; }
  after=$(sccache_counts)
  if [[ $span_counts =~ ^[0-9]+\ [0-9]+$ && $after =~ ^[0-9]+\ [0-9]+$ ]]; then
    read -r hits misses <<<"$after"
    hits=$((hits - ${span_counts%% *})) misses=$((misses - ${span_counts##* }))
  fi
  clean() { tr -cd 'A-Za-z0-9._/:-' <<<"$1"; }
  printf '{"span":"task %s","ci_source":"core.sh","ci_task":"%s","ci_phase":"%s","ci_shard":"%s","ci_commit":"%s","ci_pr":"%s","ci_vm":"%s","ci_runner":"%s","start_ns":%s,"end_ns":%s,"span_ms":%s,"ci_exit_code":%s,"ok":%s,"cache_hits":%s,"cache_misses":%s}\n' \
    "$span_task" "$span_task" "$phase" "$shard" "$(git rev-parse HEAD 2> /dev/null || true)" \
    "$(clean "${NORN_CI_PR:-}")" "$(clean "${NORN_CI_VM:-$(hostname)}")" "$(clean "${NORN_CI_RUNNER:-${RUNNER_ENVIRONMENT:-local}}")" \
    "$span_start" "$end" "$(((end - span_start) / 1000000))" "$1" "$([[ $1 -eq 0 ]] && echo true || echo false)" \
    "${hits:-null}" "${misses:-null}" >> "$spans_file" 2> /dev/null || true
  span_task=''
}
# Runs phase $1 (shard $2) as one span; a failing phase is recorded by the EXIT trap. span_end
# takes the exit status.
timed_phase() {
  span_begin "$([[ $1 == test ]] && echo "test/${2:-all}" || echo "phase/$1")"
  run_phase "$@"
  span_end 0
}
trap 'status=$?; span_end "$status"; rm -rf -- "$upstream"' EXIT
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
# Workflow lint (#1348 CI-R2): pinned actionlint and zizmor release binaries, checked against
# their sha256. Their configs live in the gated
# guard directory and are passed explicitly, so no config elsewhere in the tree can loosen them.
# actionlint's shellcheck/pyflakes hooks are off: they would depend on whatever the runner has.
lint_workflows() {
  local dir="$upstream/workflow-lint"
  mkdir -p "$dir"
  fetch() {
    [[ $(uname -sm) == 'Linux x86_64' ]] || { printf 'workflow lint: no pinned %s for %s\n' "$1" "$(uname -sm)" >&2; exit 1; }
    curl -fsSL --retry 3 -o "$dir/$1.tar.gz" "$2"
    printf '%s  %s\n' "$3" "$dir/$1.tar.gz" | sha256sum -c --quiet -
    tar -xzf "$dir/$1.tar.gz" -C "$dir" "$1"
  }
  fetch actionlint https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz \
    8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
  fetch zizmor https://github.com/zizmorcore/zizmor/releases/download/v1.32.0/zizmor-x86_64-unknown-linux-gnu.tar.gz \
    bb110cc765eca9cd7efb2e284a956be5974c60027b0970f16397d86208a9957b
  "$dir/actionlint" -config-file crates/norn-skeleton-guard/actionlint.yaml -no-color -shellcheck= -pyflakes=
  "$dir/zizmor" --quiet --quiet --offline --collect=all --strict-collection --config crates/norn-skeleton-guard/zizmor.yml --no-progress --format plain .github/workflows
  # The baseline only shrinks: every ignore must still cover a finding of the unfiltered report.
  "$dir/zizmor" --quiet --quiet --offline --collect=all --strict-collection --no-config --no-ignores \
    --pedantic --no-exit-codes --format json .github/workflows > "$dir/findings.json"
  cargo xtask ci workflow-lint --findings "$dir/findings.json"
  # Require every finding of these audits, including pedantic findings and inline ignores.
  jq -e '
    map(select(.ident == "overprovisioned-secrets" or .ident == "unredacted-secrets"
      or .ident == "excessive-permissions" or .ident == "undocumented-permissions"
      or .ident == "secrets-inherit")) |
    if length == 0 then true else .[], false end
  ' "$dir/findings.json"
}
# Split by file in separate Bun processes so module mocks and environment
# changes within one shard cannot affect the other.
run_authoring_tests() (
  cd tools/norn-ts
  bun install --frozen-lockfile --backend copyfile
  bun run typecheck
  bun test --timeout 60000 --shard=1/2 --reporter=junit --reporter-outfile="$upstream/authoring-1.xml" &
  first=$!
  bun test --timeout 60000 --shard=2/2 --reporter=junit --reporter-outfile="$upstream/authoring-2.xml" &
  second=$!
  status=0
  wait "$first" || status=1
  wait "$second" || status=1
  (( status == 0 )) || exit "$status"
  # Update the inventory whenever an authoring test file is added or removed.
  python3 - ../../scripts/ci/authoring-test-files.txt "$upstream/authoring-1.xml" "$upstream/authoring-2.xml" <<'PYTHON'
import collections
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

# A line `path skip-allowed` names a file whose every test may be skipped (a gated test).
inventory = [line.split() for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()]
if any(len(fields) not in (1, 2) or fields[1:] not in ([], ["skip-allowed"]) for fields in inventory):
    sys.exit("authoring inventory: each line is `path` or `path skip-allowed`")
expected = collections.Counter(fields[0] for fields in inventory)
skip_allowed = {fields[0] for fields in inventory if fields[1:]}
actual, executed = collections.Counter(), set()
for report in sys.argv[2:]:
    suites = ET.parse(report).getroot().findall("./testsuite")
    # Bun nests each describe block as an inner testsuite: count cases at any depth.
    if not any(case.find("skipped") is None for suite in suites for case in suite.iter("testcase")):
        sys.exit(f"{report}: no authoring tests executed")
    actual.update(suite.attrib["file"] for suite in suites)
    executed.update(suite.attrib["file"] for suite in suites
                    if any(case.find("skipped") is None for case in suite.iter("testcase")))
if not expected or actual != expected:
    sys.exit(f"Authoring shard coverage differs: missing={list(expected - actual)}, extra or repeated={list(actual - expected)}")
# A file counts only when one of its tests ran: a suite of skipped cases is not coverage.
idle = sorted(set(expected) - executed - skip_allowed)
if idle:
    sys.exit(f"Authoring files with no executed test: {idle}")
print(f"Authoring shards covered exactly the {len(expected)} inventoried test files, {len(executed)} with executed tests")
PYTHON
)

run_phase() {
  if [[ $1 != cheap && $1 != test-shards && $1 != browser && $1 != duckdb && $1 != rivet && $1 != rivet-conformance && " ${phases[*]} " == *" $1 "* ]]; then prepare_inputs; fi
  case "$1" in
    cheap)
      bash scripts/ci/secret-scan.sh .
      use_jsonschema
      cargo metadata --locked --format-version 1 > /dev/null
      cargo metadata --locked --format-version 1 "${guard[@]}" > /dev/null
      cargo fmt --all -- --check
      cargo fmt --all "${guard[@]}" -- --check
      (cd tools/norn-ts && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck)
      (cd packages/norn-sdk && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test --timeout 60000)
      (cd packages/norn-demo-ui && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test)
      (cd packages/norn-react && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test)
      (cd apps/studio && bun install --frozen-lockfile --ignore-scripts --backend copyfile && bun run typecheck && bun test && bun run build)
      # TIMER-ALARM-1: the Durable Object's wake rules; OLAP-01: what connections receive (no dependencies).
      (cd apps/norn-cell-do && bun test ./wake.test.mjs ./worker.test.mjs ./connect_vars.test.mjs)
      python3 verification/coverage-v1/negative_canaries.py --workflow-contract
      # With Cargo autodiscovery disabled, a new test file must be enrolled in suites.rs.
      python3 scripts/codemods/fold_integration_tests.py --check
      python3 scripts/ci/frontier.py self-test
      python3 scripts/ci/python_isolation.py
      python3 scripts/ci/test_python_isolation.py
      python3 scripts/proof_enrollment.py
      python3 scripts/test_proof_enrollment.py
      python3 scripts/ci/test_implementation_receipts.py
      python3 scripts/ci/test_contract_diff.py
      python3 scripts/ci/contract_diff.py --ci
      python3 scripts/test_record_new_files.py
      python3 scripts/test_demo_scenarios.py
      python3 scripts/standards_atlas.py --check
      python3 scripts/test_standards_atlas.py
      python3 scripts/importers/test_owl_sources.py
      python3 scripts/metrics/test_metrics.py
      python3 scripts/ci/test_infisical_oidc.py
      lint_workflows
      cargo xtask ci policy
      cargo xtask ci provenance
      # The Rust concurrency and bundle checks run on every plan, whatever packages the
      # plan selects, and all named tests must have run and passed.
      ensure_nextest
      # nextest keeps its reports under the workspace root's target/nextest, whatever
      # CARGO_TARGET_DIR says, and the guard's workspace root is its own directory (#1270).
      parity_report="$(cargo metadata --locked --format-version 1 --no-deps "${guard[@]}" | python3 -c 'import json, sys; print(json.load(sys.stdin)["workspace_root"])')/target/nextest/ci/junit.xml"
      rm -f "$parity_report"
      "${nextest[@]}" run --locked --ignore-default-filter "${guard[@]}" --lib -E 'test(=ci::tests::the_parity_fixture_keeps_every_gate_pinned_case) | test(=ci::tests::pull_request_concurrency_matches_the_shared_parity_fixture) | test(=ci::bundle::tests::matches_python_on_real_plans) | test(=ci::bundle::tests::refuses_wrong_inventories_and_ids) | test(=ci::bundle::tests::groups_shared_builds_and_prunes_every_other_boundary) | test(=ci::bundle::tests::prunes_only_workspace_cargo_outputs)'
      require_passed "$parity_report" norn-skeleton-guard ci::tests::the_parity_fixture_keeps_every_gate_pinned_case ci::tests::pull_request_concurrency_matches_the_shared_parity_fixture ci::bundle::tests::matches_python_on_real_plans ci::bundle::tests::refuses_wrong_inventories_and_ids ci::bundle::tests::groups_shared_builds_and_prunes_every_other_boundary ci::bundle::tests::prunes_only_workspace_cargo_outputs
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
      cargo test --locked "${guard[@]}" --doc
      ;;
    features)
      cargo test --locked -p norn-canonical -p norn-checker -p norn-ui-checker
      cargo test --locked -p norn-canonical -p norn-checker -p norn-ui-checker --features norn-canonical/preserve-order-probe
      cargo test --locked -p norn-canonical -p norn-ir -p norn-checker -p norn-ui-checker --features serde_json/arbitrary_precision
      ;;
    duckdb)
      # The workspace never enables the optional DuckDB backend, so a change that breaks it
      # compiles everywhere else (#513, #1152). Build it and run its integration tests (the
      # tenant engine's correspondence suite and the transform backend) plus MANIFEST-LAWS-1
      # (#1514, dataset_laws) in one nextest invocation so both binaries build and schedule
      # across all vCPUs together; full plans run this phase, and affected plans run it
      # whenever norn-cell is reached. The in-process `duckdb_question` target was retired
      # with the SQL question path (#1395).
      ensure_nextest
      "${nextest[@]}" run --locked --ignore-default-filter --no-fail-fast \
        -p norn-cell --features duckdb --lib --test suites \
        -E '(binary_id(norn-cell) & (test(/^suites::analytics_engine::/) | test(/^suites::duckdb_transform::/) | test(/^analytics::engine::tests::/))) | (binary_id(norn-cell::suites) & test(/^dataset_laws::/))'
      ;;
    wasm)
      cargo check --locked --target wasm32-unknown-unknown -p norn-canonical -p norn-ir -p norn-expr -p norn-checker -p norn-ui-checker -p norn-graph -p norn-datalog -p norn-authz -p norn-kernel -p norn-builtins -p norn-projections -p norn-decisions -p norn-api
      cargo check --locked -p norn-cell --no-default-features --target wasm32-unknown-unknown
      cargo clippy --locked -p norn-checker-wasm --target wasm32-unknown-unknown -- -D warnings
      cargo clippy --locked -p norn-cell-do --target wasm32-unknown-unknown -- -D warnings
      ;;
    clippy)
      cargo clippy --locked --workspace --all-targets -- -D warnings
      cargo clippy --locked "${guard[@]}" --all-targets -- -D warnings
      ;;
    doc)
      RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
      RUSTDOCFLAGS="-D warnings" cargo doc --locked "${guard[@]}" --no-deps
      ;;
    rivet)
      # crates/norn-cell-rivet is its own Cargo workspace (D131): Rivet's SQLite and the native
      # engine cannot share the root lockfile. Its .cargo/config.toml (the SQLite flags Rivet's VFS
      # requires) applies only from its own directory, and it builds in its own target subtree.
      # A RivetKit change ships only with a storage version bump, judged against the base (#1513).
      python3 scripts/ci/test_rivetkit_channel.py
      python3 scripts/ci/rivetkit_channel.py --base "${NORN_CI_BASE:-origin/main}" --require-lock
      (
        cd crates/norn-cell-rivet
        export CARGO_TARGET_DIR="$CARGO_TARGET_DIR/rivet"
        cargo fmt --check
        cargo clippy --locked --all-targets -- -D warnings
        cargo clippy --locked --all-targets --features conformance -- -D warnings
        RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
        RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --features conformance
        cargo test --locked
        cargo test --locked --features conformance
      )
      ;;
    guards)
      crates/norn-skeleton-guard/scripts/zen-dependency-tree.sh
      crates/norn-skeleton-guard/scripts/baseline-shrink-only.sh "${NORN_CI_BASE:-HEAD}"
      crates/norn-checker/scripts/non-code-literals-shrink-only.sh "${NORN_CI_BASE:-HEAD}"
      rustup toolchain install nightly-2026-08-18 --profile minimal --no-self-update
      ensure_tool cargo-public-api 0.52.0 cargo-public-api
      crates/norn-skeleton-guard/scripts/public-api-boundary.sh nightly-2026-08-18
      # Proof enrollment and its tests run in `cheap`, which every CI run and `all` include.
      ensure_nextest
      python3 scripts/check_demo_scenarios.py
      ensure_tool cargo-deny 0.19.0 cargo-deny
      cargo deny --locked check
      cargo deny --locked "${guard[@]}" check --config deny.toml
      cargo xtask ci policy
      # The guard's own suite: it left the root workspace (#1270), so no test shard selects it.
      # The guards phase runs in every plan, full or affected.
      "${nextest[@]}" run --locked --ignore-default-filter --no-fail-fast "${guard[@]}"
      ;;
    integration-authoring)
      cargo build --locked -p norn-cli --bin norn -p norn-cell-do --bin norn-dev-host
      ensure_checker_wasm_pkg
      export CI=true
      export NORN_BIN="${CARGO_TARGET_DIR:-$PWD/target}/debug/norn"
      export NORN_DEV_HOST_BIN="${CARGO_TARGET_DIR:-$PWD/target}/debug/norn-dev-host"
      export NORN_CHECKER_WASM_PKG="${CARGO_TARGET_DIR:-$PWD/target}/norn-checker-wasm/pkg"
      bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/quickstart --org example --env local --out "$upstream/task.json"
      printf "{}\n" > "$upstream/context.json"
      "$NORN_BIN" check --artifacts "$upstream/task.json" --context "$upstream/context.json" > "$upstream/check.json"
      # runnable-docs runs each release test the status matrix names; build
      # them in one Cargo invocation so its bounded runs only execute (#1171 integrated gate).
      local runnable_docs_raw
      local -a runnable_docs_args
      runnable_docs_raw=$(
        python3 - docs/authoring/runnable/status.md <<'PY'
import pathlib, re, sys
path = pathlib.Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
commands = [cmd for cmd in re.findall(r"`([^`]+)`", text) if re.match(r"^cargo\s+test(?:\s|$)", cmd)]
if not commands:
    sys.exit(f"integration-authoring: no runnable-docs release tests found in {path}")
pattern = re.compile(r"^cargo test --locked -p ([a-z0-9-]+) (?:(--lib)|--test ([a-z0-9_]+)) [a-z_:]+ -- --exact$")
pkgs, targets = set(), set()
for cmd in commands:
    match = pattern.fullmatch(cmd)
    if not match:
        sys.exit(f"integration-authoring: unsupported runnable-docs cargo test command in {path}: {cmd}")
    pkgs.add(match.group(1))
    targets.add(("--lib",) if match.group(2) else ("--test", match.group(3)))
for pkg in sorted(pkgs):
    print("-p")
    print(pkg)
for target in sorted(targets):
    for arg in target:
        print(arg)
PY
      )
      mapfile -t runnable_docs_args <<< "$runnable_docs_raw"
      cargo test --locked --no-run "${runnable_docs_args[@]}"
      run_authoring_tests
      ;;
    integration-wasm)
      export CI=true
      ensure_tool wasm-bindgen-cli 0.2.129 wasm-bindgen
      ensure_checker_wasm_pkg
      export NORN_CHECKER_WASM_PKG="$CARGO_TARGET_DIR/norn-checker-wasm/pkg"
      cargo test --locked -p norn-checker-wasm --test parity -- --test-threads=1
      node crates/norn-checker-wasm/tests/parity/parity.mjs "$NORN_CHECKER_WASM_PKG" "$NORN_CHECKER_WASM_PARITY_OUT"
      # @norn/react against the checker (#1762): its capability walk is the checker's, the matrix
      # fixture is admitted, and the machine runtime evaluates in the checker's engine.
      (cd packages/norn-react && bun install --frozen-lockfile --ignore-scripts --backend copyfile && NORN_CHECKER_WASM_REQUIRED=1 bun test)
      # TENANT-ISOLATION-1 (S8): the cell Worker's boundary over its real Wasm gate, built as it ships.
      cargo build --locked -p norn-cell-do --lib --target wasm32-unknown-unknown --profile wasm-release
      wasm-bindgen "${CARGO_TARGET_DIR:-$PWD/target}/wasm32-unknown-unknown/wasm-release/norn_cell_do.wasm" \
        --target web --out-name norn_cell_do --out-dir "$upstream/norn-cell-do-pkg"
      (cd apps/norn-cell-do && NORN_CELL_DO_PKG="$upstream/norn-cell-do-pkg" bun test ./worker.wasm.test.mjs)
      # The ZEN-only engine package pages load (#1325): pages and the checker share one
      # evaluator so they cannot disagree, so it answers every evaluation of the shared
      # corpus exactly as the checker package does.
      ensure_checker_wasm_engine
      node crates/norn-checker-wasm/tests/parity/engine-parity.mjs "$NORN_CHECKER_WASM_PKG" "${CARGO_TARGET_DIR:-$PWD/target}/norn-checker-wasm/engine"
      ;;
    integration-clients)
      export CI=true
      cargo build --locked -p norn-cli --bin norn -p norn-cell-do --bin norn-dev-host
      export NORN_DEV_HOST_BIN="$CARGO_TARGET_DIR/debug/norn-dev-host"
      # The SDK and demo UI typecheck and tests run in `cheap`; the diff below proves
      # the regenerated SDK is the committed one those runs checked.
      (cd packages/norn-sdk && bun install --frozen-lockfile --backend copyfile)
      require_fresh "$CARGO_TARGET_DIR/debug/norn"
      "$CARGO_TARGET_DIR/debug/norn" openapi > sdks/openapi/norn-api.openapi.json
      (cd packages/norn-sdk && bun run generate)
      python3 -m venv "$upstream/openapi-venv"
      "$upstream/openapi-venv/bin/python" -m pip install --disable-pip-version-check --no-input openapi-spec-validator==0.9.0
      "$upstream/openapi-venv/bin/python" -m openapi_spec_validator sdks/openapi/norn-api.openapi.json
      git diff --exit-code -- sdks/openapi/norn-api.openapi.json packages/norn-sdk/src/generated/norn-api.ts
      # The Python client is generated here from the checked document, never checked in;
      # the demo runs it against a dev host (#521).
      bash sdks/python/run-demo.sh "$upstream/python-client"
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
      ensure_checker_wasm_pkg "$PWD/apps/studio/src/checker"
      ensure_checker_wasm_engine "$PWD/apps/studio/src/engine"
      (
        cd apps/studio
        bun install --frozen-lockfile --ignore-scripts --backend copyfile
        # The checker-admission case needs the WASM package built above, so it runs here,
        # not in the cheap phase, and is required: a missing package fails it, never skips.
        decision=$(NORN_CHECKER_WASM_REQUIRED=1 bun test test/decision.test.ts 2>&1) || { printf '%s\n' "$decision"; exit 1; }
        printf '%s\n' "$decision"
        # The step proves nothing unless that named case ran and passed: skipped, renamed or deleted fails.
        grep -qxE '\(pass\) the checker admits what a drawn decision lowers to \[[0-9.]+m?s\]' <<< "$decision" || { printf 'browser: the checker-admission case did not run and pass\n' >&2; exit 1; }
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
    rivet-conformance)
      # The host conformance suite on the native Rivet host: Rivet Engine 3.1.1 in Docker (pinned
      # digest) with two runners of a release `conformance` build, against the native reference.
      # About 20 minutes on a default VM, past a bundle's 11-minute target, and it needs Docker:
      # like formal, it is a Boat gate that no CI plan selects.
      (cd crates/norn-cell-rivet && CARGO_TARGET_DIR="$CARGO_TARGET_DIR/rivet" cargo build --locked --release --features conformance)
      NORN_CELL_DO_CONFORMANCE_OUT="$upstream/rivet-reference.json" cargo test --locked -p norn-cell-do --test suites conformance_reference::
      NORN_CELL_RIVET="$CARGO_TARGET_DIR/rivet/release/norn-cell-rivet" python3 apps/norn-cell-do/conformance/conformance.py \
        --reference "$upstream/rivet-reference.json" --host rivet-native --out "$upstream/rivet-conformance.json"
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
# The hosted plan job has no prebuilt xtask. Keep its packing model in sync
# with the Rust runner until the planner can use a cached or prebuilt binary.
hosted_bundle_plan() {
  python3 - "$1" "$(list_shards)" "${phases[*]}" <<'PY'
import json, sys
raw, shards, phases = sys.argv[1:]
plan = json.loads(raw)
known = {"test/" + shard for shard in shards.split()} | {
    "phase/" + phase for phase in phases.split() + ["test-shards"]
    if phase not in {"cheap", "test", "formal", "rivet-conformance"}}
# Task seconds on Boat, refitted from green run 45206607883 (2026-11-23, df92ecf)
# and Boat benchmarks for batched runnable-docs pre-build, shared integration binaries,
# and combined nextest duckdb execution.
# A test task's seconds are net of the test build it carries (compile_seconds), and
# a WASM-pkg task's seconds are net of the shared norn-checker-wasm build (wasm_compile_seconds).
weights = {
    "test/heavy-1": 33, "test/heavy-2": 146, "test/heavy-3": 90,
    "test/heavy-4": 37, "test/heavy-5": 43, "test/heavy-6": 56,
    "test/rest-1": 221, "test/rest-2": 391, "test/rest-3": 312,
    "phase/doctest": 17, "phase/features": 347, "phase/duckdb": 510, "phase/wasm": 173,
    "phase/clippy": 51, "phase/doc": 48, "phase/guards": 117,
    "phase/rivet": 259,
    "phase/integration-authoring": 543, "phase/integration-wasm": 293,
    "phase/integration-clients": 15, "phase/provenance": 4, "phase/test-shards": 3,
    "phase/browser": 239,
}
# Shared compile costs amortized once per bundle containing any task in the group:
# - Test: workspace test binaries (test/* and phase/test-shards).
# - WasmPkg: norn-checker-wasm wasm-release LTO + wasm-opt -Oz package (charged only for
#   actual WASM consumers). integration-clients shares the WasmPkg coalescing/pruning group
#   so it co-locates with integration-authoring and reuses target/debug/{norn,norn-dev-host},
#   without charging wasm_compile_seconds when packed alone.
compile_seconds = 216
wasm_compile_seconds = 300
wasm_compile_tasks = {"phase/integration-authoring", "phase/integration-wasm", "phase/browser"}
wasm_group_tasks = wasm_compile_tasks | {"phase/integration-clients"}
def compiles_tests(task):
    return task.startswith("test/") or task == "phase/test-shards"
def in_wasm_group(task):
    return task in wasm_group_tasks
def compiles_wasm_pkg(task):
    return task in wasm_compile_tasks
def cold_cost(task):
    return (weights[task]
            + (compile_seconds if compiles_tests(task) else 0)
            + (wasm_compile_seconds if compiles_wasm_pkg(task) else 0))
def bin_cost(tasks):
    return (sum(weights[task] for task in tasks)
            + (compile_seconds if any(map(compiles_tests, tasks)) else 0)
            + (wasm_compile_seconds if any(map(compiles_wasm_pkg, tasks)) else 0))

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
# #1652 moved `core` to hosted `ubuntu-24.04`, so only `test` bundles take a Boat start.
# Six bundles balance ~20m wall clock with +33% daily Boat start capacity (140 full runs
# per 840 daily starts).
max_bundles = 6
# Use the fewest jobs that meet the service target. If an indivisible task
# exceeds it, minimize the critical path before allocating additional VMs.
# For each bundle count, try both splitting and coalescing each shared build group
# (Test, WasmPkg) so the packer keeps shared builds together whenever that lowers
# makespan or total CPU time.
def pack_once(ordered, count, coalesce_test, coalesce_wasm):
    test_group = [task for task in ordered if compiles_tests(task)]
    wasm_group = [task for task in ordered if in_wasm_group(task)]
    other = [task for task in ordered if not compiles_tests(task) and not in_wasm_group(task)]
    items = []
    if coalesce_test and test_group:
        items.append(test_group)
    else:
        items.extend([task] for task in test_group)
    if coalesce_wasm and wasm_group:
        items.append(wasm_group)
    else:
        items.extend([task] for task in wasm_group)
    items.extend([task] for task in other)
    items.sort(key=lambda group: (-bin_cost(group), group[0]))
    bins = [[] for _ in range(count)]
    for group in items:
        index = min(range(count), key=lambda index: (bin_cost(bins[index] + group), index))
        bins[index].extend(group)
    if len(ordered) >= 2 * count:
        for index in range(count):
            if len(bins[index]) != 1:
                continue
            donors = [(donor, task) for donor in range(count) if len(bins[donor]) > 2 for task in bins[donor]]
            if not donors:
                return None
            donor, task = min(
                donors,
                key=lambda move: (bin_cost(bins[index] + [move[1]]) - bin_cost(bins[index]),
                                  -bin_cost(bins[move[0]]), move[1], move[0]))
            bins[donor].remove(task)
            bins[index].append(task)
    if any(not bucket for bucket in bins):
        return None
    return bins

ordered = sorted(tasks, key=lambda task: (-cold_cost(task), task))
best = None
maximum = min(max_bundles, max(1, len(tasks) // 2)) if tasks else 0
for count in range(1, maximum + 1):
    for coalesce_test in (False, True):
        for coalesce_wasm in (False, True):
            bins = pack_once(ordered, count, coalesce_test, coalesce_wasm)
            if bins is None:
                continue
            costs = [bin_cost(bucket) for bucket in bins]
            key = (max(max(costs), target_seconds), count, sum(costs))
            if best is None or key < best[0]:
                best = (key, bins)
bins = best[1] if best is not None else []
bundles = [{"id": f"b{index}", "tasks": tasks} for index, tasks in enumerate(bins) if tasks]
if sorted(task for bundle in bundles for task in bundle["tasks"]) != sorted(tasks):
    raise SystemExit("bundle inventory dropped a planned task")
plan["bundle"] = bundles
print(json.dumps(plan))
PY
}

case "${1:-all}" in
  # Pull requests get the dependency closure of their diff. Merge groups, main and manual runs
  # (no base) and unplannable changes run every shard and phase; formal and rivet-conformance
  # remain Boat gates.
  plan)
    others=()
    for phase in "${phases[@]}"; do [[ $phase == cheap || $phase == test || $phase == formal || $phase == rivet-conformance ]] || others+=("$phase"); done
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
    if (path in gates or path.startswith((".github/workflows/", ".cargo/", "crates/norn-skeleton-guard/"))
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
# The guard is its own workspace (#1270); the guards phase, in every plan, runs its suite.
reached, stack = set(touched), list(touched)
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
                  if set(re.findall(r"binary_id\(([^:)]+)(?:::[^)]*)?\)", expression)) & reached]
# Studio changes already plan in full; a checker change reaches the Studio's WASM package.
browser = ["browser"] if "norn-checker-wasm" in reached else []
# The DuckDB backend lives in norn-cell: a change that reaches it builds and tests it (#1206).
duckdb = ["duckdb"] if "norn-cell" in reached else []
# The Rivet host's own workspace builds these root crates by path; its own files plan in full.
rivet = ["rivet"] if reached & {"norn-api", "norn-cell", "norn-cell-do"} else []
# A plan that reaches no workspace package (Markdown only) runs no test shard: empty packages
# would not narrow a shard, and the guard's own suite runs in the guards phase (#1270).
rest_names = [f"rest-{i}" for i in range(1, parts + 1)] if reached else []
done({"mode": "affected", "shard": selected_heavy + rest_names, "phase": ["guards", "provenance", "clippy"] + browser + duckdb + rivet,
      "packages": "" if reached >= known else " ".join(sorted(reached)), "rest_shards": parts, "reason": reason})
PY
)
    hosted_bundle_plan "$plan"
    ;;
  all) [[ $# -le 1 ]] || run_phase usage; for phase in "${phases[@]}"; do timed_phase "$phase"; done ;;
  shards) list_shards ;;
  test) [[ $# -le 2 ]] || run_phase usage; timed_phase test "${2:-all}" ;;
  *) [[ $# -le 1 ]] || run_phase usage; timed_phase "$1" ;;
esac
