#!/usr/bin/env bash
# Build and check the files a drop release attaches. The release-assets
# workflow runs each command; so does any local or Boat proof of it.
#
#   assets.sh tools [PREFIX]       install zig and cargo-zigbuild; print the bin directory
#   assets.sh version TAG          the package version for a release tag
#   assets.sh cli PLATFORM OUT     OUT/norn-PLATFORM.tar.gz (the norn binary, LICENSE, NOTICE)
#   assets.sh smoke PLATFORM OUT   run, or for macOS inspect, what `cli` built
#   assets.sh sdk TAG OUT          OUT/norn-sdk-VERSION.tgz (packages/norn-sdk)
#   assets.sh pyclient TAG OUT       OUT/norn_client-VERSION.tar.gz and the wheel
#   assets.sh sums OUT             OUT/SHA256SUMS over every file in OUT
#
# PLATFORM is linux-x64, linux-arm64, macos-arm64 or macos-x64. Every build is
# cross-compiled on Linux x86_64 with zig as the linker; the Linux binaries are
# static (musl). Sources come from the commit's tracked files.
set -euo pipefail

TOOLCHAIN=1.100.0
ZIG_VERSION=0.13.0
ZIG_SHA256=d45312e61ebcc48032b77bc4cf7fd6915c11fa16e4aad116b66c9468211230ea # zig-linux-x86_64-0.13.0.tar.xz
CARGO_ZIGBUILD=0.24.1

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

target_of() {
  case "$1" in
    linux-x64) echo x86_64-unknown-linux-musl ;;
    linux-arm64) echo aarch64-unknown-linux-musl ;;
    macos-x64) echo x86_64-apple-darwin ;;
    macos-arm64) echo aarch64-apple-darwin ;;
    *) echo "assets.sh: unknown platform '$1'" >&2; exit 2 ;;
  esac
}

# drop-28 is 0.0.28; any other tag is 0.0.0 with the tag as its suffix.
version_of() {
  local tag=$1
  if [[ $tag =~ ^drop-([0-9]+)$ ]]; then
    echo "0.0.${BASH_REMATCH[1]}"
  elif [[ $tag =~ ^drop-([A-Za-z0-9.-]+)$ ]]; then
    echo "0.0.0-${BASH_REMATCH[1]}"
  else
    echo "assets.sh: '$tag' is not a drop tag" >&2
    exit 2
  fi
}

# PEP 440 spelling of version_of: 0.0.0-test.1 becomes 0.0.0+test.1.
python_version_of() {
  local version
  version=$(version_of "$1")
  echo "${version/-/+}"
}

# A macOS build needs an SDK for the system libraries it links. The only framework norn's
# dependencies use is CoreFoundation, for the system time zone (iana-time-zone, through
# chrono). Rather than download Apple's SDK, write a text stub that names the symbols
# those crates import; the real framework supplies them when the binary runs on a Mac.
# A new import fails the link and is added here.
macos_sdk() {
  local frameworks=$1/System/Library/Frameworks/CoreFoundation.framework
  mkdir -p "$frameworks" "$1/usr/lib"
  cat > "$frameworks/CoreFoundation.tbd" << 'TBD'
--- !tapi-tbd
tbd-version:     4
targets:         [ x86_64-macos, arm64-macos ]
install-name:    '/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation'
current-version: 1
exports:
  - targets:         [ x86_64-macos, arm64-macos ]
    symbols:         [ _CFRelease, _CFStringGetBytes, _CFStringGetCStringPtr, _CFStringGetLength,
                       _CFTimeZoneCopySystem, _CFTimeZoneGetName, _CFTimeZoneResetSystem ]
...
TBD
}

commit_time() {
  git log -1 --format=%ct HEAD
}

# A reproducible archive of DIR's listed members.
archive() {
  local out=$1 dir=$2
  shift 2
  tar --sort=name --mtime="@$(commit_time)" --owner=0 --group=0 --numeric-owner \
    -C "$dir" -cf - "$@" | gzip -n > "$out"
}

cmd=${1:?usage: assets.sh COMMAND ...}
shift
case "$cmd" in
  tools)
    prefix=${1:-$HOME/.norn-release-tools}
    mkdir -p "$prefix/bin"
    [[ $(uname -sm) == "Linux x86_64" ]] || { echo "assets.sh: builds run on Linux x86_64" >&2; exit 1; }
    rustup toolchain install "$TOOLCHAIN" --profile minimal --no-self-update >&2
    rustup target add --toolchain "$TOOLCHAIN" \
      x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-apple-darwin aarch64-apple-darwin >&2
    if [[ ! -x $prefix/zig/zig ]] || [[ $("$prefix/zig/zig" version) != "$ZIG_VERSION" ]]; then
      archive_file=$(mktemp)
      curl -fsSL --retry 3 "https://ziglang.org/download/$ZIG_VERSION/zig-linux-x86_64-$ZIG_VERSION.tar.xz" -o "$archive_file"
      echo "$ZIG_SHA256  $archive_file" | sha256sum -c - >&2
      rm -rf "$prefix/zig" && mkdir -p "$prefix/zig"
      tar -xJf "$archive_file" -C "$prefix/zig" --strip-components 1
      rm -f "$archive_file"
    fi
    ln -sfn "$prefix/zig/zig" "$prefix/bin/zig"
    if [[ $("$prefix/bin/cargo-zigbuild" --version 2> /dev/null) != *"$CARGO_ZIGBUILD"* ]]; then
      RUSTUP_TOOLCHAIN=$TOOLCHAIN cargo install --locked --root "$prefix" cargo-zigbuild --version "$CARGO_ZIGBUILD" >&2
    fi
    echo "$prefix/bin"
    ;;

  version)
    version_of "${1:?usage: assets.sh version TAG}"
    ;;

  cli)
    platform=${1:?usage: assets.sh cli PLATFORM OUT}
    out=${2:?usage: assets.sh cli PLATFORM OUT}
    target=$(target_of "$platform")
    export RUSTUP_TOOLCHAIN=$TOOLCHAIN CARGO_INCREMENTAL=0
    if [[ $platform == macos-* ]]; then
      sdk=$(mktemp -d)
      macos_sdk "$sdk"
      export SDKROOT=$sdk
    fi
    cargo zigbuild --locked --release -p norn-cli --target "$target"
    stage=$(mktemp -d)
    cp "${CARGO_TARGET_DIR:-target}/$target/release/norn" LICENSE NOTICE "$stage/"
    mkdir -p "$out"
    archive "$out/norn-$platform.tar.gz" "$stage" norn LICENSE NOTICE
    rm -rf "$stage" "${sdk:-}"
    ;;

  smoke)
    platform=${1:?usage: assets.sh smoke PLATFORM OUT}
    out=${2:?usage: assets.sh smoke PLATFORM OUT}
    stage=$(mktemp -d)
    tar -xzf "$out/norn-$platform.tar.gz" -C "$stage"
    case "$platform" in
      linux-x64) run=("$stage/norn") ;;
      linux-arm64) run=(qemu-aarch64-static "$stage/norn") ;;
      macos-*)
        # No macOS machine is available to the release: check that the file is a
        # Mach-O for the right CPU, and that an arm64 one carries a code signature
        # (macOS refuses to run an unsigned arm64 binary).
        python3 -I - "$platform" "$stage/norn" << 'PY'
import struct, sys
platform, path = sys.argv[1:]
data = open(path, "rb").read()
magic, cpu, _sub, _type, ncmds, _size, _flags, _reserved = struct.unpack_from("<IiiIIIII", data)
expected = {"macos-arm64": 0x0100000C, "macos-x64": 0x01000007}[platform]
assert magic == 0xFEEDFACF, f"not a 64-bit Mach-O: {magic:#x}"
assert cpu == expected, f"cpu {cpu:#x}, expected {expected:#x}"
offset, commands, dylibs = 32, [], []
for _ in range(ncmds):
    cmd, size = struct.unpack_from("<II", data, offset)
    commands.append(cmd)
    if cmd in (0xC, 0x80000018):  # LC_LOAD_DYLIB, LC_LOAD_WEAK_DYLIB
        name_at = offset + struct.unpack_from("<I", data, offset + 8)[0]
        dylibs.append(data[name_at : data.index(b"\0", name_at)].decode())
    offset += size
signed = 0x1D in commands
assert signed or platform == "macos-x64", "arm64 binary has no LC_CODE_SIGNATURE"
framework = "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation"
assert framework in dylibs, f"CoreFoundation is not linked from its system path: {dylibs}"
print(f"{platform}: Mach-O cpu {cpu:#x}, code signature: {signed}, links {dylibs}")
PY
        rm -rf "$stage"
        exit 0
        ;;
    esac
    "${run[@]}" --version
    "${run[@]}" openapi > "$stage/openapi.json"
    python3 -I -c 'import json, sys; json.load(open(sys.argv[1]))' "$stage/openapi.json"
    echo "$platform: norn --version and norn openapi ran"
    rm -rf "$stage"
    ;;

  sdk)
    tag=${1:?usage: assets.sh sdk TAG OUT}
    out=${2:?usage: assets.sh sdk TAG OUT}
    version=$(version_of "$tag")
    mkdir -p "$out" && out=$(cd "$out" && pwd)
    stage=$(mktemp -d)
    git archive HEAD packages/norn-sdk LICENSE NOTICE | tar -x -C "$stage"
    package="$stage/packages/norn-sdk"
    cp "$stage/LICENSE" "$stage/NOTICE" "$package/"
    python3 -I - "$package/package.json" "$version" << 'PY'
import json, sys
path, version = sys.argv[1:]
manifest = json.load(open(path))
manifest["version"] = version
manifest["files"] = ["src", "LICENSE", "NOTICE"]
open(path, "w").write(json.dumps(manifest, indent=2) + "\n")
PY
    mkdir -p "$out"
    (cd "$package" && npm pack --ignore-scripts --pack-destination "$out" > /dev/null)
    rm -rf "$stage"
    ;;

  pyclient)
    tag=${1:?usage: assets.sh pyclient TAG OUT}
    out=${2:?usage: assets.sh pyclient TAG OUT}
    version=$(python_version_of "$tag")
    mkdir -p "$out" && out=$(cd "$out" && pwd)
    work=$(mktemp -d)
    python3 -m venv "$work/venv"
    "$work/venv/bin/python" -m pip install --disable-pip-version-check --no-input -q --require-hashes \
      -r sdks/python/requirements.txt -r sdks/python/package-requirements.txt
    python3 -I sdks/python/prepare.py sdks/openapi/norn-api.openapi.json "$work/client-input.json"
    mkdir -p "$work/project"
    PATH="$work/venv/bin:$PATH" "$work/venv/bin/openapi-python-client" generate \
      --path "$work/client-input.json" --output-path "$work/project/norn_client" --meta none > "$work/generate.log" 2>&1
    if grep -Eq "Warning|WARNING|Unable to process" "$work/generate.log"; then
      cat "$work/generate.log"
      echo "assets.sh: the generator reported warnings; the client would be missing pieces" >&2
      exit 1
    fi
    cp LICENSE NOTICE "$work/project/"
    cat > "$work/project/pyproject.toml" << TOML
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[project]
name = "norn-client"
version = "$version"
description = "Python client for a Norn cell, generated from its OpenAPI document"
requires-python = ">=3.11"
license = "Apache-2.0"
license-files = ["LICENSE", "NOTICE"]
dependencies = ["httpx2>=2.16.0,<3", "attrs>=22.2.0"]

[tool.hatch.build.targets.wheel]
packages = ["norn_client"]
TOML
    mkdir -p "$out"
    epoch=$(commit_time)
    (cd "$work/project" && SOURCE_DATE_EPOCH=$epoch "$work/venv/bin/python" -m build --no-isolation --sdist --wheel --outdir "$out")
    rm -rf "$work"
    ;;

  sums)
    out=${1:?usage: assets.sh sums OUT}
    (cd "$out" && rm -f SHA256SUMS && sha256sum -- * > SHA256SUMS && cat SHA256SUMS)
    ;;

  *)
    echo "assets.sh: unknown command '$cmd'" >&2
    exit 2
    ;;
esac
