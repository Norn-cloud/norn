#!/usr/bin/env bash
# No literal secret in the committed tree (credential policy). Usage: secret-scan.sh [repository]
#
# gitleaks, pinned by version AND digest (always the verified download, never a `gitleaks` on PATH),
# scans every blob of HEAD's tree as committed: raw `git cat-file` bytes, so `export-ignore`,
# filters and working-tree encodings in .gitattributes hide nothing. It runs with its built-in
# rules and no allowlist of any kind: a tracked .gitleaks.toml or .gitleaksignore fails, inline
# allow comments are ignored, it runs in a clean environment (no GITLEAKS_CONFIG* or other
# override) outside the checkout, so no untracked config or ignore file is read. A false positive
# is fixed in the fixture. `Norn-cloud/norn-ops` holds the same scan in `ci/secret-scan.sh`.
set -euo pipefail
repo=$(cd "${1:-.}" && git rev-parse --show-toplevel)
version=8.28.0 digest=a65b5253807a68ac0cafa4414031fd740aeb55f54fb7e55f386acb52e6a840eb
[[ $(uname -sm) == 'Linux x86_64' ]] || { printf 'secret scan: no pinned gitleaks for %s\n' "$(uname -sm)" >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cache=${XDG_CACHE_HOME:-$HOME/.cache}/norn-ci
archive="$cache/gitleaks_${version}_linux_x64.tar.gz"
mkdir -p "$cache"
# A cached download is re-verified on every run; a mismatch is fetched again, then must verify.
if ! printf '%s  %s\n' "$digest" "$archive" | sha256sum -c --quiet - 2> /dev/null; then
  curl -fsSL --retry 3 -o "$archive.part" "https://github.com/gitleaks/gitleaks/releases/download/v$version/gitleaks_${version}_linux_x64.tar.gz"
  mv -f "$archive.part" "$archive"
  printf '%s  %s\n' "$digest" "$archive" | sha256sum -c --quiet -
fi
mkdir "$work/bin" "$work/tree"
tar -xzf "$archive" -C "$work/bin" gitleaks
configs=$(git -C "$repo" ls-tree -r --name-only HEAD | grep -E '(^|/)\.gitleaks(\.toml|ignore)$' || true)
[[ -z $configs ]] || { printf 'secret scan: no gitleaks allowlist may be tracked: %s\n' "$configs" >&2; exit 1; }
# Raw blobs of HEAD's tree (submodule commits have no blob; a symlink's blob is its target). Python
# runs from the empty work directory, so nothing beside it can shadow a module.
cd "$work"
git -C "$repo" ls-tree -r -z HEAD | python3 -c '
import os, subprocess, sys
repo, tree = sys.argv[1], sys.argv[2]
entries = [line.split(b"\t", 1) for line in sys.stdin.buffer.read().split(b"\0") if line]
blobs = [(meta.split()[2], path) for meta, path in entries if meta.split()[1] == b"blob"]
cat = subprocess.Popen(["git", "-C", repo, "cat-file", "--batch"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
for sha, path in blobs:
    cat.stdin.write(sha + b"\n"); cat.stdin.flush()
    size = int(cat.stdout.readline().split()[2])
    data = cat.stdout.read(size); cat.stdout.read(1)
    target = os.path.join(tree.encode(), path)
    os.makedirs(os.path.dirname(target), exist_ok=True)
    with open(target, "wb") as out:
        out.write(data)
cat.stdin.close()
sys.exit(cat.wait())
' "$repo" "$work/tree"
env -i PATH=/usr/bin:/bin HOME="$work" "$work/bin/gitleaks" dir "$work/tree" --no-banner --redact \
  --ignore-gitleaks-allow --gitleaks-ignore-path "$work" --exit-code 1
