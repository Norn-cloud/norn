#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
tool_root="$(realpath -m "$tool_root")"
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
AENEAS_COMMIT="74a460a2f80ecea481bbdf1a08f881633c3bb097"
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
printf '%s  %s\n' \
  "edfca2630c373b44b7ee1c2f81cd8dcf67468d0db57d6c02158de553ac63dbd4" \
  "$opam_binary" | sha256sum --check --status
ln -sfn "$opam_binary" "$bin_root/opam"

clone_pin "https://github.com/AeneasVerif/aeneas.git" "$AENEAS_COMMIT" "$source_root/aeneas"

export CARGO_HOME="$tool_root/cargo"
export RUSTUP_HOME="$tool_root/rustup"
export OPAMROOT="$tool_root/opam-root"
export PATH="$bin_root:$CARGO_HOME/bin:$PATH"
# OCaml native compilation is memory intensive even with only four CPUs.
export OPAMJOBS="${OPAMJOBS:-1}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export OPAMSWITCH="$OPAM_SWITCH"
receipt="$tool_root/extraction-install.sha256"

installation_digest() {
  # Bind the recipe (all pins), package inventory, installed code and libraries.
  # Build/download caches are deliberately excluded; they are never executed.
  sha256sum < "${BASH_SOURCE[0]}"
  "$opam_binary" list --installed --short --columns=name,version
  find -L "$OPAMROOT/$OPAM_SWITCH/bin" "$OPAMROOT/$OPAM_SWITCH/lib" "$OPAMROOT/$OPAM_SWITCH/share" \
    "$bin_root" "$tool_root/rustup/toolchains" \
    "$source_root/aeneas/bin" "$source_root/aeneas/charon/bin" \
    -type f -exec stat -Lc 'mode %a %n' {} + -exec sha256sum {} + | LC_ALL=C sort
}

if [[ -f "$receipt" ]]; then
  eval "$("$opam_binary" env --set-root --set-switch --shell=sh)"
  test "$(git -C "$source_root/aeneas/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
  test -z "$(git -C "$source_root/aeneas/charon" status --porcelain=v1 --untracked-files=all)"
  installed_digest="$(installation_digest | sha256sum)"
  if [[ "$installed_digest" != "$(cat "$receipt")" ]]; then
    echo "Pinned extraction installation changed; rebuild the snapshot without $receipt" >&2
    exit 1
  fi
  echo "Exact pinned extraction installation verified: $installed_digest"
  exit 0
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

"$opam_binary" install --yes \
  logs.0.10.0 \
  ppx_deriving.6.1.3 \
  unionFind.20260226 \
  visitors.20260520 \
  yojson.3.0.0 \
  zarith.1.14 \
  menhir.20260209 \
  menhirLib.20260209 \
  re.1.14.0 \
  easy_logging.0.8.2 \
  core_unix.v0.17.1 \
  ocamlgraph.2.2.0 \
  progress.0.5.0 \
  domainslib.0.5.2 \
  odoc.3.2.1 \
  ppx_deriving_yojson.3.10.0

if [[ ! -x "$source_root/aeneas/charon/bin/charon" ]]; then
  env -u RUSTUP_TOOLCHAIN -u CARGO_TARGET_DIR make -C "$source_root/aeneas" setup-charon
fi
test "$(git -C "$source_root/aeneas/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
if [[ -n "$(git -C "$source_root/aeneas/charon" status --porcelain=v1 --untracked-files=all)" ]]; then
  echo "pinned source tree is dirty: $source_root/aeneas/charon" >&2
  exit 1
fi
env -u RUSTUP_TOOLCHAIN -u CARGO_TARGET_DIR make -C "$source_root/aeneas/charon" build-dev
env -u RUSTUP_TOOLCHAIN -u CARGO_TARGET_DIR make -C "$source_root/aeneas" build-dev
ln -sfn "$source_root/aeneas/bin/aeneas" "$bin_root/aeneas"
ln -sfn "$source_root/aeneas/charon/bin/charon" "$bin_root/charon"

for source in "$source_root/aeneas" "$source_root/aeneas/charon"; do
  if [[ -n "$(git -C "$source" status --porcelain=v1 --untracked-files=all)" ]]; then
    echo "pinned source tree became dirty while building: $source" >&2
    exit 1
  fi
done

test "$("$OPAMROOT/$OPAM_SWITCH/bin/ocamlc" -version)" = 5.4.1
test "$("$source_root/aeneas/bin/aeneas" -version)" = 'aeneas 74a460a2'
test "$("$source_root/aeneas/charon/bin/charon" version)" = 0.1.240
installation_digest | sha256sum > "$receipt.tmp"
mv "$receipt.tmp" "$receipt"

cat <<EOF
Installed pinned extraction tools under:
  $tool_root

Use them without modifying your shell profile:
  export PATH="$bin_root:$CARGO_HOME/bin:\$PATH"
  export RUSTUP_HOME="$RUSTUP_HOME"
  export OPAMROOT="$OPAMROOT"
  export OPAMSWITCH="$OPAM_SWITCH"
EOF
