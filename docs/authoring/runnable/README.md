# Compile and check a definition

This path uses the public toolchain at main `2859d63d435a89ce82dc3902d24924c0882e95a8`.
It ends with admission and a receipt. For execution, embed `norn-cell`; the public
CLI currently supplies `check`, `eval`, `openapi`, and `remote`.

Install Rust 1.98.0, Bun 1.3.14, the `wasm32-unknown-unknown` Rust target, and
`wasm-bindgen-cli` 0.2.129. From the repository root, install authoring dependencies
with `cd tools/norn-ts && bun install --frozen-lockfile --backend copyfile`, then
return to the repository root. Build the native binary with
`cargo build --locked -p norn-cli` and the WASM package with
`bash crates/norn-checker-wasm/scripts/build-pkg.sh`.

Set `NORN_BIN` to the absolute path of `target/debug/norn`,
`NORN_CHECKER_WASM_PKG` to the absolute path of `target/norn-checker-wasm/pkg`,
and `NORN_DOC_TMP` to an empty temporary directory you own. If using
`CARGO_TARGET_DIR`, use that directory instead of `target` for both build outputs.
Run this block from the repository root:

```sh runnable
bun tools/norn-ts/src/cli.ts check tools/norn-ts/fixtures/quickstart --org example --env local > "$NORN_DOC_TMP/wasm-report.json"
bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/quickstart --org example --env local --out "$NORN_DOC_TMP/task.json"
printf '{}\n' > "$NORN_DOC_TMP/context.json"
"$NORN_BIN" check --artifacts "$NORN_DOC_TMP/task.json" --context "$NORN_DOC_TMP/context.json" > "$NORN_DOC_TMP/native-report.json"
```

The [Task source](../../../tools/norn-ts/fixtures/quickstart/task.ts) imports
`Schema` from `effect` and `Norn` from `@norn/authoring`. It declares an identity
and text properties. Compilation reads its syntax without running application
code. Both reports must admit the same artifact and return the same canonical
receipt digest. Changing a definition requires checking it again.

The integration gate executes the block above through
[`runnable-docs.test.ts`](../../../tools/norn-ts/test/runnable-docs.test.ts), which
also compares the native and WASM receipts. The [status matrix](status.md) lists
narrow release tests and remaining gaps. The intended concept-based authoring
model is an [architecture design](../../architecture/decisions/2026-11-01.md);
its examples are not the current SDK syntax.
