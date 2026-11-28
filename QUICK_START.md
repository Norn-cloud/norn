# Install

Nothing is on npm, PyPI or crates.io yet. Prebuilt archives for the Rust `norn`,
the checker WASM, the local dev host, `@norn/sdk`, and the Python client attach
to each `drop-*` release, or you can build from a checkout of this repository.

## Which `norn`?

Two different programs are called `norn`:

| | Rust `norn` | TypeScript `norn` |
|---|---|---|
| Source | `crates/norn-cli` | `tools/norn-ts` (`src/cli.ts`) |
| Run it as | `target/release/norn`, or `cargo run -p norn-cli --` | `bun tools/norn-ts/src/cli.ts`, or `bunx norn` inside a project made by `norn init` |
| Commands | `check`, `eval`, `explain`, `mcp`, `remote`, `cell`, `source`, `capabilities`, `openapi`, `source-digest` | `init`, `setup`, `dev`, `fmt`, `check`, `compile`, `profile`, `codegen`, `question`, `dashboard`, `plan`, `apply`, `explain` |
| Use it to | admit compiled IR, evaluate decisions, serve MCP, plan and apply on a hosted cell | write typed definitions, compile them to IR, generate SDK types, run a local dev cell |

Both have `check` and `explain`, and they take different inputs: the TypeScript
`check` reads `.ts` sources, the Rust `check` reads the compiled JSON IR.
Renaming one of them is tracked in [#1451](https://github.com/Norn-cloud/norn/issues/1451).

## Fastest setup from a checkout (`norn setup`)

`norn setup` downloads and verifies the prebuilt checker WASM, dev host, and Rust
`norn` from a GitHub release into `~/.cache/norn/runtime/`, so you do not need
a local Rust toolchain just to author and run definitions:

```sh
git clone https://github.com/Norn-cloud/norn && cd norn
(cd tools/norn-ts && bun install --frozen-lockfile && bun src/cli.ts setup)
```

You can also run `scripts/dev/fetch-local-runtime.sh` from the repository root.
When `NORN_CHECKER_WASM_PKG` and `NORN_DEV_HOST_BIN` are unset, the TypeScript
CLI uses that cache automatically.

## From source

You need Rust 1.100.0 with the `wasm32-unknown-unknown` target, `wasm-bindgen-cli`
0.2.129, Bun 1.4.5, Node with `npx` (the checker build runs binaryen's
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

To install only the Rust `norn` from a tag:
`cargo install --locked --git https://github.com/Norn-cloud/norn --tag TAG norn-cli`.
For a step-by-step walkthrough, follow [the authoring tutorial](docs/authoring/tutorial.md).

## From a release (available from drop-29)

`drop-28` and earlier releases have no files attached. From `drop-29` on, each
`drop-*` release attaches what [`scripts/release/assets.sh`](scripts/release/assets.sh)
builds: the Rust `norn` (`norn-PLATFORM.tar.gz`) and local dev host
(`norn-dev-host-PLATFORM.tar.gz`) for Linux and macOS on x64 and arm64, the
checker WASM (`norn-checker-wasm.tar.gz`), the TypeScript SDK
(`norn-sdk-VERSION.tgz`), the Python client (wheel and sdist), `install.sh`, and
`SHA256SUMS`. `VERSION` is `0.0.N` for `drop-N`.

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
and have not been run natively yet; treat them as experimental. The TypeScript `norn`
runs from `tools/norn-ts` in a checkout; run `bun tools/norn-ts/src/cli.ts setup`
to download and verify the prebuilt checker WASM, dev host, and Rust `norn` automatically.

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

# Run the product packs locally

`norn dev` serves a project's own SQLite cell on loopback. The three-systems
template gives it the product packs (canon, ERPNext, Shopify, Odoo, sources,
Arabic locale) exactly as a hosted tenant provisions them, plus their synthetic
exports. Prerequisites, from this checkout: Rust 1.100.0, Bun 1.4.5 and
`bun install --frozen-lockfile` in `tools/norn-ts`; Studio also needs the
checker WASM (`wasm-bindgen-cli` 0.2.129, as above; `build-pkg.sh` runs
binaryen 132.0.0's `wasm-opt` through `npx`, so it needs the network once, or
`NORN_WASM_OPT=0` to skip it).

```sh
# Either cache prebuilt binaries once with `bun tools/norn-ts/src/cli.ts setup`,
# or build locally and set NORN_DEV_HOST_BIN="$PWD/target/release/norn-dev-host".
bun tools/norn-ts/src/cli.ts init /tmp/packs --template three-systems
cd /tmp/packs && bunx norn dev --load data/erpnext.json --load data/shopify.json --load data/odoo.json
```

Each `--load` file is a recorded export: its `snapshot` rows, then its
`incremental` changes, go through the cell's admitted mappings, so parties,
items, holdings, order lines, deliveries, postings and settlements all have
records with lineage to their source row. A file the cell refuses, or a row a
mapping quarantines, stops `norn dev` with exit 3 and the reason. While it
runs, `POST /v1/observations:load` takes further slices of an export (at most
100 rows and 8 page commits each) through the same path.

Studio over the running cell, with live updates:

```sh
cd apps/studio && bun install --frozen-lockfile && bun run checker && bun run build
bun run local --project /tmp/packs                       # http://127.0.0.1:5180
bun run local --project /tmp/packs --as reviewer --port 5181
```

`norn dev` serves two local principals with every grant: the manifest's actor
(`.norn/dev.token`) and its reviewer (`.norn/dev.reviewer.token`). An approval
admits against the hosted roles (`viewer`, `member`, `admin`, `owner`); a call
it gates waits until someone other than its requester decides it, so ask in one
Studio and decide in the reviewer's. Holding every role is not holding every
action: an action whose `access` names a policy is decided by that policy for
both principals, as on a hosted cell. A worked example with an approval, a law
and an ERPNext export is [docs/build/purchase-requests.md](docs/build/purchase-requests.md).
