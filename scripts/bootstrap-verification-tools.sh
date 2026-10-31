#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
download_root="$tool_root/downloads"
bin_root="$tool_root/bin"

mkdir -p "$download_root" "$bin_root"

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

ELAN_VERSION="4.2.3"
LEAN_VERSION="4.33.1"
QUINT_VERSION="0.32.0"
KANI_VERSION="0.67.0"
Z3_VERSION="5.1.0"
JRE_VERSION="21.0.12.1_1"

elan_archive="$download_root/elan-$ELAN_VERSION-x86_64-unknown-linux-gnu.tar.gz"
download_verified \
  "https://github.com/leanprover/elan/releases/download/v$ELAN_VERSION/elan-x86_64-unknown-linux-gnu.tar.gz" \
  "df0b2b3a439961ffcbb3985214365ffe40f49bc871df04dff268c7d8e21ca8b2" \
  "$elan_archive"

elan_unpack="$tool_root/elan-installer-$ELAN_VERSION"
if [[ ! -x "$elan_unpack/elan-init" ]]; then
  mkdir -p "$elan_unpack"
  tar -xzf "$elan_archive" -C "$elan_unpack"
fi

export ELAN_HOME="$tool_root/elan"
if [[ ! -x "$ELAN_HOME/bin/lean" ]]; then
  "$elan_unpack/elan-init" -y --no-modify-path --default-toolchain "leanprover/lean4:v$LEAN_VERSION"
elif ! "$ELAN_HOME/bin/elan" toolchain list | grep -Fxq "leanprover/lean4:v$LEAN_VERSION"; then
  "$ELAN_HOME/bin/elan" toolchain install "leanprover/lean4:v$LEAN_VERSION"
fi

"$ELAN_HOME/bin/elan" toolchain install leanprover/lean4:v4.34.1

for proxy in elan lean lake leanc leanmake; do
  ln -sfn "$ELAN_HOME/bin/$proxy" "$bin_root/$proxy"
done

quint_binary="$tool_root/quint-$QUINT_VERSION/quint"
if [[ ! -x "$quint_binary" ]]; then
  mkdir -p "$(dirname "$quint_binary")"
  download_verified \
    "https://github.com/quint-co/quint/releases/download/v$QUINT_VERSION/quint-linux-amd64" \
    "939b64095b706017f2f202c6f99c860c40be7c31bddc2b98557316e50f42cd7f" \
    "$quint_binary"
  chmod +x "$quint_binary"
fi
ln -sfn "$quint_binary" "$bin_root/quint"

kani_archive="$download_root/kani-$KANI_VERSION-x86_64-unknown-linux-gnu.tar.gz"
download_verified \
  "https://github.com/model-checking/kani/releases/download/kani-$KANI_VERSION/kani-$KANI_VERSION-x86_64-unknown-linux-gnu.tar.gz" \
  "3b5f7afd3b51603ee720db7bc1bc4fe46b5a4f5d36daad9939c4b4c658b51ac0" \
  "$kani_archive"

kani_home="$tool_root/kani-home"
kani_root="$kani_home/kani-$KANI_VERSION"
if [[ ! -x "$kani_root/bin/kani-driver" ]]; then
  mkdir -p "$kani_root"
  tar -xzf "$kani_archive" -C "$kani_root" --strip-components=1
fi

kani_wrapper_root="$tool_root/kani-wrapper"
if [[ ! -x "$kani_wrapper_root/bin/kani" ]]; then
  cargo install --locked --root "$kani_wrapper_root" kani-verifier --version "$KANI_VERSION"
fi

export RUSTUP_HOME="$tool_root/rustup"
kani_rust_toolchain="$(cat "$kani_root/rust-toolchain-version")"
if [[ ! -d "$RUSTUP_HOME/toolchains/$kani_rust_toolchain" ]]; then
  rustup toolchain install "$kani_rust_toolchain" --profile minimal --no-self-update
fi
ln -sfn "$RUSTUP_HOME/toolchains/$kani_rust_toolchain" "$kani_root/toolchain"
ln -sfn "$kani_wrapper_root/bin/kani" "$bin_root/kani"
ln -sfn "$kani_wrapper_root/bin/cargo-kani" "$bin_root/cargo-kani"

z3_archive="$download_root/z3-$Z3_VERSION-x64-glibc-2.39.zip"
download_verified \
  "https://github.com/Z3Prover/z3/releases/download/z3-$Z3_VERSION/z3-$Z3_VERSION-x64-glibc-2.39.zip" \
  "f47be8d27d3230e823bf1eeede2fe0abaca55bb78d0b59974370e6689a92284a" \
  "$z3_archive"

z3_root="$tool_root/z3-$Z3_VERSION"
z3_binary="$(find "$z3_root" -type f -path '*/bin/z3' -perm -u+x -print -quit 2>/dev/null || true)"
if [[ -z "$z3_binary" ]]; then
  mkdir -p "$z3_root"
  unzip -q "$z3_archive" -d "$z3_root"
fi
z3_binary="$(find "$z3_root" -type f -path '*/bin/z3' -perm -u+x -print -quit)"
if [[ -z "$z3_binary" ]]; then
  echo "Z3 archive did not contain an executable named z3" >&2
  exit 1
fi
ln -sfn "$z3_binary" "$bin_root/z3"

jre_archive="$download_root/OpenJDK21U-jre_x64_linux_hotspot_$JRE_VERSION.tar.gz"
download_verified \
  "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12.1%2B1/OpenJDK21U-jre_x64_linux_hotspot_$JRE_VERSION.tar.gz" \
  "2413149700df0f7d440500a84a8f764c535f21e5a5e87d38328b64eec2c5b500" \
  "$jre_archive"

jre_root="$tool_root/temurin-jre-$JRE_VERSION"
java_binary="$(find "$jre_root" -type f -path '*/bin/java' -perm -u+x -print -quit 2>/dev/null || true)"
if [[ -z "$java_binary" ]]; then
  mkdir -p "$jre_root"
  tar -xzf "$jre_archive" -C "$jre_root"
  java_binary="$(find "$jre_root" -type f -path '*/bin/java' -perm -u+x -print -quit)"
fi
if [[ -z "$java_binary" ]]; then
  echo "Temurin archive did not contain an executable named java" >&2
  exit 1
fi
ln -sfn "$java_binary" "$bin_root/java"

# The harness owns server lifetimes; acquire the distribution without starting
# Quint's unmanaged default-port server. Bind both archive and executed JAR.
apalache_archive="$download_root/apalache-0.56.1.tgz"
download_verified \
  "https://github.com/apalache-mc/apalache/releases/download/v0.56.1/apalache.tgz" \
  "91125e5a3646b9c9d3a7d921d3323f321fac5071909f72b3960c66ff2f998ee1" \
  "$apalache_archive"
apalache_root="$tool_root/quint-home/apalache-dist-0.56.1"
mkdir -p "$apalache_root"
tar -xzf "$apalache_archive" -C "$apalache_root"
printf '%s  %s\n' \
  "4753c0ebb2cbb266e2c6ac19ab5ca3827d726cc80fd1fc5d7c1eeb64736cd60b" \
  "$apalache_root/apalache/lib/apalache.jar" | sha256sum --check --status

cat <<EOF
Installed pinned verification tools under:
  $tool_root

Use them without modifying your shell profile:
  export PATH="$bin_root:\$PATH"
  export ELAN_HOME="$ELAN_HOME"
  export KANI_HOME="$kani_home"
  export RUSTUP_HOME="$RUSTUP_HOME"
EOF
