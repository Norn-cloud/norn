#!/usr/bin/env bash
# Build Norn's release from the checked-out commit, on both channels, and
# verify it. Nothing is published.
#
#   scripts/release/build.sh OUT BASE_URL
#
# OUT/tarball   the GitHub pre-release (CLI and authoring tarballs, one dev
#               host per platform, manifest.json, SHA256SUMS); its build.json
#               fetches hosts from BASE_URL, the release's download address
# OUT/registry  the npm packages (@norn/cli, @norn/authoring,
#               @norn/dev-host-<platform>), the same sources and built files
#
# The tools come from scripts/release/tools.sh (on PATH). The worktree must be
# clean: assemble.py takes sources from git, and the compiled files must come
# from the same bytes. The release workflow runs this script; so does any
# local or Boat proof of it.
set -euo pipefail

TOOLCHAIN=1.98.0
out=${1:?usage: build.sh OUT BASE_URL}
base_url=${2:?usage: build.sh OUT BASE_URL}
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
export RUSTUP_TOOLCHAIN=$TOOLCHAIN CARGO_INCREMENTAL=0
[[ -z $(git status --porcelain --untracked-files=no) ]] || { echo "build.sh: the worktree has changes; build from a clean commit" >&2; git status --short >&2; exit 1; }
commit=$(git rev-parse HEAD)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

python3 scripts/release/test_assemble.py
python3 scripts/release/test_third_party.py
# The shipped closure is held to the workspace licence policy (deny.toml).
cargo deny --locked check licenses

crates/norn-checker-wasm/scripts/build-pkg.sh "$work/wasm"
for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  cargo zigbuild --locked --release -p norn-cell-do --bin norn-dev-host --target "$target"
  mkdir -p "$work/hosts/$target"
  cp "${CARGO_TARGET_DIR:-target}/$target/release/norn-dev-host" "$work/hosts/$target/"
done
python3 scripts/release/third_party.py --out "$work/THIRD-PARTY-LICENSES"

for channel in tarball registry; do
  python3 scripts/release/assemble.py build --commit "$commit" --channel "$channel" \
    $([[ $channel == tarball ]] && printf -- '--base-url %s' "$base_url") \
    --wasm "$work/wasm" --hosts "$work/hosts" --third-party "$work/THIRD-PARTY-LICENSES" \
    --toolchain "$TOOLCHAIN" --out "$out/$channel"
done
# The compile must not have touched a tracked file.
[[ -z $(git status --porcelain --untracked-files=no) ]] || { echo "build.sh: the build changed tracked files" >&2; git status --short >&2; exit 1; }
cat "$out/tarball/release/SHA256SUMS"
