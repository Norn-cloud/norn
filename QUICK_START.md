# Install

Each `drop-*` release attaches prebuilt files; nothing is on npm, PyPI or crates.io yet.
Replace `TAG` with a release tag such as `drop-29`.

```sh
# the `norn` command line (Linux or macOS, x64 or arm64) into ~/.local/bin
curl -fsSL https://github.com/Norn-cloud/norn/releases/latest/download/install.sh | sh
# or from source, pinned to a tag
cargo install --locked --git https://github.com/Norn-cloud/norn --tag TAG norn-cli
# the Python client (generated from the OpenAPI document, so it is a release file, not a git path)
uv add https://github.com/Norn-cloud/norn/releases/download/TAG/norn_client-VERSION-py3-none-any.whl
# the TypeScript SDK (types; TypeScript source)
npm i https://github.com/Norn-cloud/norn/releases/download/TAG/norn-sdk-VERSION.tgz
```

The macOS binaries are cross-built on Linux and have not been run natively yet; treat them as experimental.
`VERSION` is `0.0.N` for `drop-N`. Every release lists its files and their SHA-256 sums in `SHA256SUMS`;
`install.sh` checks the one it downloads. Set `NORN_VERSION=TAG` to install a specific release.
`norn check` below also needs the TypeScript authoring tools, which still run from this checkout.

# Check a typed definition

Install Rust 1.98.0 with the `wasm32-unknown-unknown` target, Bun 1.3.14 and
`wasm-bindgen-cli` 0.2.129. From this checkout:

```sh
cargo install --locked wasm-bindgen-cli --version 0.2.129
crates/norn-checker-wasm/scripts/build-pkg.sh
export NORN_CHECKER_WASM_PKG="$PWD/target/norn-checker-wasm/pkg"
cd tools/norn-ts
bun install --frozen-lockfile
bun src/cli.ts check fixtures/quickstart --org example --env local
bun src/cli.ts compile fixtures/quickstart --org example --env local --out /tmp/norn-task.json
cd ../..
printf '{}\n' > /tmp/norn-context.json
cargo run --locked -p norn-cli -- check --artifacts /tmp/norn-task.json --context /tmp/norn-context.json
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
exports. Prerequisites, from this checkout: Rust 1.98.0, Bun 1.3.14 and
`bun install --frozen-lockfile` in `tools/norn-ts`; Studio also needs the
checker WASM (`wasm-bindgen-cli` 0.2.129, as above; `build-pkg.sh` runs
binaryen 132.0.0's `wasm-opt` through `npx`, so it needs the network once, or
`NORN_WASM_OPT=0` to skip it). The hosted cell's own smoke
(`apps/norn-cell-do/smoke.py`) also needs `esbuild` and celld.

```sh
cargo build --release -p norn-cell-do --bin norn-dev-host
export NORN_DEV_HOST_BIN="$PWD/target/release/norn-dev-host"
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
Studio and decide in the reviewer's.
