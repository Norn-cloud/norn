# Install

Nothing is on npm, PyPI or crates.io. Today you build Norn from a checkout of this
repository. Prebuilt release files start with the next drop release.

## Which `norn`?

Two different programs are called `norn`:

| | Rust `norn` | TypeScript `norn` |
|---|---|---|
| Source | `crates/norn-cli` | `tools/norn-ts` (`src/cli.ts`) |
| Run it as | `target/release/norn`, or `cargo run -p norn-cli --` | `bun tools/norn-ts/src/cli.ts`, or `bunx norn` inside a project made by `norn init` |
| Commands | `check`, `eval`, `explain`, `mcp`, `remote`, `cell`, `source`, `capabilities`, `openapi`, `source-digest` | `init`, `dev`, `fmt`, `check`, `compile`, `profile`, `question`, `dashboard`, `plan`, `apply`, `explain` |
| Use it to | admit compiled IR, evaluate decisions, serve MCP, plan and apply on a hosted cell | write typed definitions, compile them to IR, run a local dev cell |

Both have `check` and `explain`, and they take different inputs: the TypeScript
`check` reads `.ts` sources, the Rust `check` reads the compiled JSON IR.
The release files below carry only the Rust `norn`. Renaming one of them is tracked
in [#1451](https://github.com/Norn-cloud/norn/issues/1451).

## From source (works today)

You need Rust 1.98.0 with the `wasm32-unknown-unknown` target, `wasm-bindgen-cli`
0.2.129, Bun 1.3.14, Node with `npx` (the checker build runs binaryen's
`wasm-opt` through `npx`; set `NORN_WASM_OPT=0` to skip that step), and `sha256sum`
(the checker build prints checksums with it; on macOS it comes with GNU coreutils,
`brew install coreutils`). The builds write under `CARGO_TARGET_DIR` when it is set,
so the paths below follow it.

```sh
git clone https://github.com/Norn-cloud/norn && cd norn
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.129

# the checker as WASM, which the TypeScript tools load
crates/norn-checker-wasm/scripts/build-pkg.sh
# an absolute path, even when CARGO_TARGET_DIR is relative
TARGET="$(cd "${CARGO_TARGET_DIR:-target}" && pwd)"
export NORN_CHECKER_WASM_PKG="$TARGET/norn-checker-wasm/pkg"

# the Rust `norn` and the local dev host that the TypeScript `norn dev` runs
cargo build --locked --release -p norn-cli
cargo build --locked --release -p norn-cell-do --bin norn-dev-host
export NORN_DEV_HOST_BIN="$TARGET/release/norn-dev-host"

# the TypeScript `norn`, and Studio if you want the web app
(cd tools/norn-ts && bun install --frozen-lockfile)
(cd apps/studio && bun install --frozen-lockfile)
```

The hosted-cell smoke in [`crates/norn-cell`](crates/norn-cell/README.md) also needs
`esbuild` on `PATH` and celld v0.6.3 (see that README for its pin and checksum). To install only the Rust `norn` from a tag:
`cargo install --locked --git https://github.com/Norn-cloud/norn --tag TAG norn-cli`.

A one-command `norn dev` setup for a project with packs is in progress; this page
will link its guide once it lands. Until then, follow
[the authoring tutorial](docs/authoring/tutorial.md).

## From a release (available from drop-29)

`drop-28` and earlier releases have no files attached. From `drop-29` on, each
`drop-*` release attaches what [`scripts/release/assets.sh`](scripts/release/assets.sh)
builds: the Rust `norn` for Linux and macOS on x64 and arm64 (`norn-PLATFORM.tar.gz`),
the TypeScript SDK (`norn-sdk-VERSION.tgz`), the Python client (wheel and sdist),
`install.sh`, and `SHA256SUMS`. `VERSION` is `0.0.N` for `drop-N`.

```sh
TAG=drop-29
# the Rust `norn` (Linux or macOS, x64 or arm64) into ~/.local/bin
curl -fsSL https://github.com/Norn-cloud/norn/releases/download/$TAG/install.sh | NORN_VERSION=$TAG sh
# the Python client (generated from the OpenAPI document)
uv add https://github.com/Norn-cloud/norn/releases/download/$TAG/norn_client-0.0.29-py3-none-any.whl
# the TypeScript SDK (types; TypeScript source)
npm i https://github.com/Norn-cloud/norn/releases/download/$TAG/norn-sdk-0.0.29.tgz
```

`install.sh` checks the archive it downloads against `SHA256SUMS`; that catches a
damaged download, it is not a signature. The macOS binaries are cross-built on Linux
and have not been run natively yet; treat them as experimental. The TypeScript `norn`,
the checker WASM and the dev host are not release files: build them from source as above.

# Check a typed definition

After the source build above, from the repository root:

```sh
cd tools/norn-ts
bun src/cli.ts check fixtures/quickstart --org example --env local
bun src/cli.ts compile fixtures/quickstart --org example --env local --out /tmp/norn-task.json
cd ../..
printf '{}\n' > /tmp/norn-context.json
"${CARGO_TARGET_DIR:-target}/release/norn" check --artifacts /tmp/norn-task.json --context /tmp/norn-context.json
```

The independently authored [Task](tools/norn-ts/fixtures/quickstart/task.ts) defines
an identity and two text properties. Compilation reads typed syntax without running
user code. Both check commands use the same Rust admission logic and reject invalid
artifacts with diagnostics. The report includes a canonical receipt and its SHA-256.

For execution, embed `norn-cell::SqliteCell` through the `norn-kernel::CellHost`
interface. Its [cell host tests](crates/norn-cell/tests/cell_host.rs) exercise
transactional commit, idempotent delivery and recovery against the real SQLite host.
Run the retained core gates with `bash scripts/ci/core.sh`.
