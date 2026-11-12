#!/usr/bin/env bash
# Install the exact tools a Norn release is built with, into PREFIX (default
# $HOME/.norn-release-tools), and print the directory to put on PATH.
#
#   scripts/release/tools.sh [PREFIX]
#
# Needs rustup and cargo. Every version is pinned here; the zig tarball is
# checked against its published sha256 before it is unpacked. The release
# workflow and a local or Boat proof of it run this same script.
set -euo pipefail

TOOLCHAIN=1.98.0
ZIG_VERSION=0.13.0
ZIG_SHA256=d45312e61ebcc48032b77bc4cf7fd6915c11fa16e4aad116b66c9468211230ea   # zig-linux-x86_64-0.13.0.tar.xz
CARGO_ZIGBUILD=0.24.1
CARGO_DENY=0.19.0
# The wasm-bindgen CLI must equal the crate pin; build-pkg.sh refuses any other.
WASM_BINDGEN=$(sed -n 's/^wasm-bindgen = "=\([^"]*\)"$/\1/p' "$(dirname "$0")/../../crates/norn-checker-wasm/Cargo.toml")
TARGETS=(wasm32-unknown-unknown x86_64-unknown-linux-musl aarch64-unknown-linux-musl)

prefix=${1:-$HOME/.norn-release-tools}
mkdir -p "$prefix/bin"
[[ $(uname -sm) == "Linux x86_64" ]] || { echo "tools.sh: releases are built on Linux x86_64" >&2; exit 1; }
[[ -n $WASM_BINDGEN ]] || { echo "tools.sh: no exact wasm-bindgen pin in crates/norn-checker-wasm/Cargo.toml" >&2; exit 1; }

rustup toolchain install "$TOOLCHAIN" --profile minimal --no-self-update >&2
rustup target add --toolchain "$TOOLCHAIN" "${TARGETS[@]}" >&2

if [[ ! -x $prefix/zig/zig ]] || [[ $("$prefix/zig/zig" version) != "$ZIG_VERSION" ]]; then
  archive=$(mktemp)
  curl -fsSL --retry 3 "https://ziglang.org/download/$ZIG_VERSION/zig-linux-x86_64-$ZIG_VERSION.tar.xz" -o "$archive"
  echo "$ZIG_SHA256  $archive" | sha256sum -c - >&2
  rm -rf "$prefix/zig" && mkdir -p "$prefix/zig"
  tar -xJf "$archive" -C "$prefix/zig" --strip-components 1
  rm -f "$archive"
fi
ln -sfn "$prefix/zig/zig" "$prefix/bin/zig"

install() {
  local crate=$1 version=$2 binary=$3
  if [[ $("$prefix/bin/$binary" --version 2>/dev/null) != *"$version"* ]]; then
    RUSTUP_TOOLCHAIN=$TOOLCHAIN cargo install --locked --root "$prefix" "$crate" --version "$version" >&2
  fi
}
install cargo-zigbuild "$CARGO_ZIGBUILD" cargo-zigbuild
install cargo-deny "$CARGO_DENY" cargo-deny
install wasm-bindgen-cli "$WASM_BINDGEN" wasm-bindgen
echo "$prefix/bin"
