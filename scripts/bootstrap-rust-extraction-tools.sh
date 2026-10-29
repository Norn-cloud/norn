#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
download_root="$tool_root/downloads"
source_root="$tool_root/sources"
bin_root="$tool_root/bin"

mkdir -p "$download_root" "$source_root" "$bin_root"

download_verified() {
  local url="$1"
  local sha256="$2"
  local destination="$3"

  if [[ -f "$destination" ]] && printf '%s  %s\n' "$sha256" "$destination" | sha256sum --check --status; then
    return
  fi

  rm -f "$destination"
  curl --fail --location --retry 3 --output "$destination" "$url"
  printf '%s  %s\n' "$sha256" "$destination" | sha256sum --check --status
}

clone_pin() {
  local url="$1"
  local commit="$2"
  local destination="$3"

  if [[ ! -d "$destination/.git" ]]; then
    git clone --filter=blob:none "$url" "$destination"
  fi
  if [[ "$(git -C "$destination" rev-parse HEAD)" != "$commit" ]]; then
    git -C "$destination" fetch --depth 1 origin "$commit"
    git -C "$destination" checkout --detach "$commit"
  fi
  if [[ -n "$(git -C "$destination" status --porcelain=v1 --untracked-files=all)" ]]; then
    echo "pinned source tree is dirty: $destination" >&2
    exit 1
  fi
}

OPAM_VERSION="2.5.2"
OPAM_REPOSITORY_COMMIT="45220c31b03bbad5f368a3e01c8e3da44844c5db"
FSTAR_VERSION="2026.08.16"
HAX_COMMIT="37ecb8240fc237377f63614622650190a45617ff"
HAX_RUST_TOOLCHAIN="nightly-2025-11-08-x86_64-unknown-linux-gnu"
OPAM_SWITCH="norn-verification-5.4.1"

opam_binary="$tool_root/opam-$OPAM_VERSION/opam"
if [[ ! -x "$opam_binary" ]]; then
  mkdir -p "$(dirname "$opam_binary")"
  download_verified \
    "https://github.com/ocaml/opam/releases/download/$OPAM_VERSION/opam-$OPAM_VERSION-x86_64-linux" \
    "edfca2630c373b44b7ee1c2f81cd8dcf67468d0db57d6c02158de553ac63dbd4" \
    "$opam_binary"
  chmod +x "$opam_binary"
fi
ln -sfn "$opam_binary" "$bin_root/opam"

fstar_archive="$download_root/fstar-v$FSTAR_VERSION-Linux-x86_64.tar.gz"
download_verified \
  "https://github.com/FStarLang/FStar/releases/download/v$FSTAR_VERSION/fstar-v$FSTAR_VERSION-Linux-x86_64.tar.gz" \
  "e8718fad41c51ef212aa6bfd7418796ab5e32f24e490eb0d7bef9357486fa6ab" \
  "$fstar_archive"

fstar_root="$tool_root/fstar-$FSTAR_VERSION"
fstar_binary="$(find "$fstar_root" -type f -name fstar.exe -perm -u+x -print -quit 2>/dev/null || true)"
if [[ -z "$fstar_binary" ]]; then
  mkdir -p "$fstar_root"
  tar -xzf "$fstar_archive" -C "$fstar_root"
  fstar_binary="$(find "$fstar_root" -type f -name fstar.exe -perm -u+x -print -quit)"
fi
if [[ -z "$fstar_binary" ]]; then
  echo "F* archive did not contain fstar.exe" >&2
  exit 1
fi
ln -sfn "$fstar_binary" "$bin_root/fstar.exe"

clone_pin "https://github.com/cryspen/hax.git" "$HAX_COMMIT" "$source_root/hax"

export CARGO_HOME="$tool_root/cargo"
export RUSTUP_HOME="$tool_root/rustup"
export RUSTUP_TOOLCHAIN="$HAX_RUST_TOOLCHAIN"
export OPAMROOT="$tool_root/opam-root"
export PATH="$bin_root:$CARGO_HOME/bin:$PATH"

if [[ ! -d "$RUSTUP_HOME/toolchains/$HAX_RUST_TOOLCHAIN" ]]; then
  rustup toolchain install "$HAX_RUST_TOOLCHAIN" \
    --profile minimal \
    --component rustc-dev \
    --component llvm-tools-preview \
    --component rust-analysis \
    --component rust-src \
    --component rustfmt \
    --no-self-update
fi

opam_repository_url="git+https://github.com/ocaml/opam-repository.git#$OPAM_REPOSITORY_COMMIT"
if [[ ! -f "$OPAMROOT/config" ]]; then
  "$opam_binary" init --bare --disable-sandboxing --no-setup --yes default "$opam_repository_url"
else
  "$opam_binary" repository set-url default "$opam_repository_url" --all-switches --yes
  "$opam_binary" update default --yes
fi
if ! "$opam_binary" switch list --short | grep -Fxq "$OPAM_SWITCH"; then
  "$opam_binary" switch create "$OPAM_SWITCH" 5.4.1 --yes
fi
export OPAMSWITCH="$OPAM_SWITCH"
eval "$("$opam_binary" env --set-root --set-switch --shell=sh)"

if [[ ! -x "$CARGO_HOME/bin/cargo-hax" ]] || ! command -v hax-engine >/dev/null 2>&1; then
  (
    cd "$source_root/hax"
    ./setup.sh -j "${NORN_VERIFICATION_BUILD_JOBS:-4}" --no-cleanup
  )
fi
ln -sfn "$CARGO_HOME/bin/cargo-hax" "$bin_root/cargo-hax"
ln -sfn "$CARGO_HOME/bin/driver-hax-frontend-exporter" "$bin_root/driver-hax-frontend-exporter"
hax_engine="$(command -v hax-engine)"
ln -sfn "$hax_engine" "$bin_root/hax-engine"

for source in "$source_root/hax"; do
  if [[ -n "$(git -C "$source" status --porcelain=v1 --untracked-files=all)" ]]; then
    echo "pinned source tree became dirty while building: $source" >&2
    exit 1
  fi
done

cat <<EOF
Installed pinned extraction tools under:
  $tool_root

Use them without modifying your shell profile:
  export PATH="$bin_root:$CARGO_HOME/bin:\$PATH"
  export RUSTUP_HOME="$RUSTUP_HOME"
  export OPAMROOT="$OPAMROOT"
  export OPAMSWITCH="$OPAM_SWITCH"
EOF
