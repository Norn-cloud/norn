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
