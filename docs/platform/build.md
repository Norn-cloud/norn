# Building the workspace

How the Rust workspace builds, and what the build settings in the repository do.

## Toolchain

There is no `rust-toolchain.toml`. CI installs Rust 1.100.0 (`.github/workflows/ci-policy.yml`), with `rustfmt`, `clippy` and the `wasm32-unknown-unknown` target. Use the same version locally to match CI. The workspace uses edition 2024 and resolver 2.

## `.cargo/config.toml`

It sets no linker and no `rustflags`. It has one alias:

```sh
cargo xtask ci <gate>
```

`xtask` runs the typed CI gate runner from the skeleton guard, which is its own Cargo workspace: `run --locked --quiet --manifest-path crates/norn-skeleton-guard/Cargo.toml --bin xtask --`. The manifest path is relative, so run `cargo xtask` from the repository root.

## Integration test binaries

Crates with multiple foldable integration targets compile those tests as modules of
`tests/suites.rs`. Run `python3 scripts/codemods/fold_integration_tests.py` to enroll a
new integration test file in those crates.

On a cold 4-vCPU Boat VM, mean `cargo test --no-run --workspace --locked` time fell
from 316 s to 177 s. Test executables fell from 304 to 77 and the target directory
shrank from 37 GB to 8.3 GB. With Wild, the same comparison was 317 s to 174 s.
The linker difference was within measurement noise, so the workspace keeps its
default linker.

## Profiles (`Cargo.toml`)

| Profile | Settings | Why |
|---|---|---|
| `dev` | `debug = "line-tables-only"`; dependencies `debug = false` | Backtraces keep `file:line`, while target dirs and link times stay small. The bundled DuckDB C++ is built without `-g` and with `NDEBUG`. Overflow checks and debug assertions stay on. `test` inherits `dev`. |
| `test`, package `norn-canonical` | `opt-level = 3` | |
| `release` | `overflow-checks = true` | Silent wraparound is never acceptable in kernel or runtime code. |
| `wasm-release` | inherits `release`; `opt-level = "s"`, `lto = true`, `codegen-units = 1` | Size-optimized checker WebAssembly package. Overflow checks stay on. |

For full debug info in one session:

```sh
cargo build --config 'profile.dev.debug=true' --config 'profile.dev.package."*".debug=true'
```

`wasm-release` is used by `crates/norn-checker-wasm/scripts/build-pkg.sh`, which runs `cargo rustc -p norn-checker-wasm --target wasm32-unknown-unknown --profile wasm-release --crate-type cdylib` and then `wasm-bindgen`.

## Optional: a faster linker

This is a local speed-up only. CI does not use it, and nothing in the repository turns it on. Linking with mold or wild can cut link time on large test binaries.

Keep it scoped to one command. Do not add a linker to `.cargo/config.toml` in the repository, and do not export `RUSTFLAGS` in your shell profile: changing `RUSTFLAGS` changes Cargo's fingerprints, so every build that does not set the same flags rebuilds from scratch.

Use mold only if it is installed:

```sh
if command -v mold >/dev/null 2>&1; then
  RUSTFLAGS="-C link-arg=-fuse-ld=mold" cargo test --locked -p norn-kernel
else
  cargo test --locked -p norn-kernel
fi
```

Or wild, through clang, for the Linux x86-64 target only:

```sh
if command -v wild >/dev/null 2>&1 && command -v clang >/dev/null 2>&1; then
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang \
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=--ld-path=$(command -v wild)" \
    cargo test --locked -p norn-kernel
else
  cargo test --locked -p norn-kernel
fi
```

The `CARGO_TARGET_<triple>_*` form touches only native builds, so `wasm32-unknown-unknown` builds are not affected.

The same wild setting as a local-only config, as the comment in `.cargo/config.toml` shows (put it in your own `~/.cargo/config.toml`, never in the repository):

```toml
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=--ld-path=/absolute/path/to/wild"]
```

Substitute the path of an installed mold binary to compare the two linkers.

## CI

CI builds through `scripts/ci/core.sh`, phase by phase. Rust compiles in CI go through sccache; see [CI compiler cache](ci-compiler-cache.md).
