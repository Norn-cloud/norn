#!/usr/bin/env bash
# Scan the checkout with the checksum-pinned gitleaks release.
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
if ! { [[ -f $archive ]] && printf '%s  %s\n' "$digest" "$archive" | sha256sum -c --quiet - > /dev/null 2>&1; }; then
  curl -fsSL --retry 3 -o "$archive.part" "https://github.com/gitleaks/gitleaks/releases/download/v$version/gitleaks_${version}_linux_x64.tar.gz"
  mv -f "$archive.part" "$archive"
  printf '%s  %s\n' "$digest" "$archive" | sha256sum -c --quiet -
fi
tar -xzf "$archive" -C "$work" gitleaks
# Select the embedded stock rules. Gitleaks also reads a source-root ignore file automatically.
printf '[extend]\nuseDefault = true\n' > "$work/stock.toml"
if [[ -e $repo/.gitleaksignore || -L $repo/.gitleaksignore ]]; then
  printf 'secret scan: checkout-root .gitleaksignore is forbidden\n' >&2
  exit 1
fi
"$work/gitleaks" dir "$repo" --config "$work/stock.toml" --gitleaks-ignore-path "$work" \
  --no-banner --redact --ignore-gitleaks-allow
