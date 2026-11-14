#!/usr/bin/env bash
# scripts/ci/scoped.sh BASE: the scoped gate for a demo/robin unit (HEAD against BASE), run from
# the unit's checkout. Each step's log and its status and seconds go to $NORN_SCOPED_OUT
# (default target/scoped/), and the script exits non-zero if any step failed. It never replaces
# main's merge gate: PRs to main run the full plan (core.sh). Touched crates come from the diff,
# their direct dependents from cargo metadata; tests run through core.sh (its fixture inputs).
set -uo pipefail
base=${1:?usage: scripts/ci/scoped.sh BASE}
out=${NORN_SCOPED_OUT:-$PWD/target/scoped}
mkdir -p "$out" && : > "$out/steps"
failed=0
# opt-level stays CI's (0). Measured 2026-11-14 (#1348): opt-level 1 made norn-api tests 3.4x
# faster but the I/O-bound norn-cell durable-store tests slower, so it is not the default.
step() { local n=$1 t=$SECONDS rc; shift; "$@" > "$out/$n.log" 2>&1; rc=$?; echo "$n rc=$rc $((SECONDS-t))s" | tee -a "$out/steps"; [[ $rc -eq 0 ]] || failed=1; }
t0=$SECONDS
changed=$(git -c core.quotePath=false diff --name-only --no-renames "$base" HEAD) || { echo "scoped: cannot diff against $base" >&2; exit 2; }
# Workspace-wide inputs reach every crate: such a unit needs the full gate, never a scoped one.
if wide=$(grep -E '^(Cargo\.lock|Cargo\.toml|rust-toolchain(\.toml)?|\.cargo/.*)$' <<<"$changed"); then
  echo "scoped: workspace-wide change ($(tr '\n' ' ' <<<"$wide")); run the full gate: scripts/ci/core.sh all" >&2
  exit 3
fi
read -r touched dependents outside < <(python3 -I - "$changed" <<'PY'
import json, subprocess, sys
changed = sys.argv[1].split()
meta = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"]))
root = meta["workspace_root"]
pkgs = {p["name"]: p["manifest_path"][len(root) + 1:].rsplit("/", 1)[0] for p in meta["packages"]}
touched = {n for n, d in pkgs.items() for f in changed if f.startswith(d + "/")}
deps = {n for p in meta["packages"] for n in [p["name"]]
        if any(d["name"] in touched and d.get("path") for d in p["dependencies"])} - touched
# The planner's rule (core.sh plan --affected): a path outside every member crate is
# unplannable, except provenance records and top-level or docs/ Markdown. Studio has its own
# steps below. Crate tests read fixtures/, registry/ and the like, so such a unit needs the full gate.
dirs = sorted(pkgs.values())
outside = [f for f in changed if not any(f.startswith(d + "/") for d in dirs)
           and not f.startswith(("apps/studio/", ".github/ci/provenance/"))
           and not (f.endswith(".md") and (f.startswith("docs/") or "/" not in f))]
print(",".join(sorted(touched)) or "-", ",".join(sorted(deps)) or "-", outside[0] if outside else "-", len(outside))
PY
)
[[ -n ${touched:-} && -n ${dependents:-} && -n ${outside:-} ]] || { echo "could not derive the touched crates" >&2; exit 2; }
if [[ ${outside%% *} != - ]]; then
  echo "scoped: ${outside##* } changed path(s) outside the crates and apps/studio, e.g. ${outside%% *}; run the full gate: scripts/ci/core.sh all" >&2
  exit 3
fi
echo "touched=$touched dependents=$dependents" | tee -a "$out/steps"
step fmt cargo fmt --all -- --check
if [[ $touched != - ]]; then
  p=(); for c in ${touched//,/ }; do p+=(-p "$c"); done
  step clippy cargo clippy --locked "${p[@]}" --all-targets -- -D warnings
  [[ ,$touched, != *,norn-cell,* ]] || step wasm cargo check --locked -p norn-cell --no-default-features --target wasm32-unknown-unknown
  # Through core.sh: its fixture inputs (NORN_UPSTREAM_FIXTURE_DIR), the pinned `ci` nextest
  # profile and package narrowing, exactly as CI's affected plans run tests.
  packages=${touched//,/ }; [[ $dependents == - ]] || packages+=" ${dependents//,/ }"
  # Test temp dirs on tmpfs (ruling 2026-11-14, #1348): the norn-cell durable-store tests reopen
  # their store cleanly and assert logical state; none kills a process, fakes a crash or times
  # fsync, so tmpfs (where fsync is a no-op) checks the same things, 23x faster on Boat disks.
  # Real-disk durability stays covered: CI, main's merge gate and robin's nightly keep real disk.
  # NORN_SCOPED_TMPDIR overrides; an unusable tmpfs falls back to the default TMPDIR.
  scoped_tmp=${NORN_SCOPED_TMPDIR:-/dev/shm}
  [[ -d $scoped_tmp && -w $scoped_tmp ]] || scoped_tmp=${TMPDIR:-/tmp}
  echo "tests TMPDIR=$scoped_tmp" | tee -a "$out/steps"
  step tests env NORN_CI_PACKAGES="$packages" TMPDIR="$scoped_tmp" bash scripts/ci/core.sh test all
  step regen bash -c 'cargo run --locked -q -p norn-cli -- openapi > sdks/openapi/norn-api.openapi.json && (cd packages/norn-sdk && bun run generate) && git diff --exit-code -- sdks/openapi packages/norn-sdk/src/generated'
fi
step provenance bash scripts/ci/core.sh provenance
if grep -q '^apps/studio/' <<<"$changed"; then
  step studio bash -c 'cd apps/studio && bun install --frozen-lockfile --backend copyfile && bun run typecheck && bun test && bun run build'
  # Studio e2e. Only unit tests changed: none. Only e2e specs changed: those specs, through the
  # same build as core.sh's browser phase. Anything else (the specs drive the whole UI, and most
  # changes land in shared code): the full suite via `core.sh browser`, with its guards.
  studio=$(grep '^apps/studio/' <<<"$changed")
  specs=$(grep -E '^apps/studio/e2e/[^/]+\.e2e\.ts$' <<<"$studio" || true)
  rest=$(grep -vE '^apps/studio/(test/|e2e/[^/]+\.e2e\.ts$)' <<<"$studio" || true)
  if [[ -n $rest ]]; then
    step e2e bash scripts/ci/core.sh browser
  elif [[ -n $specs ]]; then
    step e2e bash -c 'set -o pipefail; crates/norn-checker-wasm/scripts/build-pkg.sh "$PWD/apps/studio/src/checker" && cd apps/studio && bun run build && bunx playwright test "$@" --reporter=list | tee /dev/stderr | grep -qE "^ +[0-9]+ passed"' _ $(sed 's#^apps/studio/##' <<<"$specs")
  fi
fi
echo "total $((SECONDS-t0))s failed=$failed" | tee -a "$out/steps"
exit "$failed"
