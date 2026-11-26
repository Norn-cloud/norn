#!/bin/sh
# Install the `norn` command line from a Norn GitHub release.
#
#   curl -fsSL https://github.com/Norn-cloud/norn/releases/latest/download/install.sh | sh
#
# Environment:
#   NORN_VERSION      release tag to install (default: the latest release)
#   NORN_INSTALL_DIR  where `norn` goes (default: ~/.local/bin)
#   NORN_REPO         owner/name of the repository whose release to use
#   NORN_BASE_URL     a directory URL holding SHA256SUMS and the archive (tests)
#
# The archive is checked against the release's SHA256SUMS before anything is
# installed. That catches a damaged or truncated download; it is not a
# signature, so it does not protect against a tampered release.
set -eu

die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

fetch() {
  if command -v curl > /dev/null 2>&1; then
    curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget > /dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    die "curl or wget is required"
  fi
}

sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum > /dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    die "sha256sum or shasum is required"
  fi
}

main() {
  repo=${NORN_REPO:-Norn-cloud/norn}
  version=${NORN_VERSION:-latest}
  dir=${NORN_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}

  case "$repo" in
    *[!A-Za-z0-9._/-]* | /* | */ | */*/*)
      die "invalid NORN_REPO: $repo"
      ;;
    */*) ;;
    *) die "invalid NORN_REPO: $repo" ;;
  esac
  case "$version" in
    "" | *[!A-Za-z0-9._+-]* | *..*)
      die "invalid NORN_VERSION: $version"
      ;;
  esac

  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) die "unsupported OS $(uname -s): Linux and macOS only" ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch=x64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *) die "unsupported CPU $(uname -m): x86_64 and arm64 only" ;;
  esac
  # A shell running under Rosetta reports x86_64 on an Apple-silicon Mac.
  if [ "$os" = macos ] && [ "$arch" = x64 ] \
    && [ "$(sysctl -n sysctl.proc_translated 2> /dev/null || true)" = 1 ]; then
    arch=arm64
  fi
  asset=norn-$os-$arch.tar.gz

  if [ -n "${NORN_BASE_URL:-}" ]; then
    base=${NORN_BASE_URL%/}
  elif [ "$version" = latest ]; then
    base=https://github.com/$repo/releases/latest/download
  else
    base=https://github.com/$repo/releases/download/$version
  fi

  work=$(mktemp -d)
  staged=""
  cleanup() {
    rm -rf "$work"
    if [ -n "$staged" ]; then
      rm -f "$staged"
    fi
  }
  trap cleanup EXIT
  trap 'cleanup; exit 129' HUP
  trap 'cleanup; exit 130' INT
  trap 'cleanup; exit 143' TERM

  fetch "$base/SHA256SUMS" "$work/SHA256SUMS" \
    || die "cannot download $base/SHA256SUMS; this release may predate prebuilt binaries"
  fetch "$base/$asset" "$work/$asset" \
    || die "cannot download $base/$asset; this release has no binary for $os-$arch"

  want=$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1 }' "$work/SHA256SUMS")
  case "$want" in
    "" | *[!0-9a-f]*) die "SHA256SUMS has no single checksum for $asset" ;;
  esac
  [ "${#want}" -eq 64 ] || die "SHA256SUMS has no single checksum for $asset"
  have=$(sha256_of "$work/$asset")
  [ "$have" = "$want" ] || die "checksum mismatch for $asset: expected $want, got $have; nothing installed"

  mkdir "$work/extract"
  tar -xzf "$work/$asset" -C "$work/extract" norn || die "$asset does not contain norn"
  [ -f "$work/extract/norn" ] && [ ! -L "$work/extract/norn" ] \
    || die "$asset norn entry must be a regular file"
  mkdir -p "$dir"
  # Copy beside the target and rename, so a running norn is replaced atomically.
  staged=$(mktemp "$dir/.norn.XXXXXX")
  cp "$work/extract/norn" "$staged"
  chmod 755 "$staged"
  mv -f "$staged" "$dir/norn"
  staged=""

  printf 'installed %s\n' "$("$dir/norn" --version)"
  printf 'at %s/norn\n' "$dir"
  if [ "$os" = macos ]; then
    printf 'note: the macOS binaries are cross-built on Linux and have not been run natively yet (experimental)\n' >&2
  fi
  case ":$PATH:" in
    *":$dir:"*) ;;
    *) printf 'add it to your PATH:  export PATH="%s:$PATH"\n' "$dir" ;;
  esac
}

main "$@"
