# Typed authoring

This frontend reads typed TypeScript syntax without executing authored modules.
Its Effect profile and compiler lower definitions and bounded flows to Norn IR;
the Rust checker remains the admission authority through the WASM ABI.

Install the exact dependencies with `bun install --frozen-lockfile`.
`bun src/cli.ts profile <file.ts>` recognizes the supported syntax.
`check <pack-dir|file.ts> --org ID --env ID` compiles and admits a set;
`compile <pack-dir|file.ts> --org ID --env ID --out FILE` writes its IR array.
Set `NORN_CHECKER_WASM_PKG` to the package built by
`crates/norn-checker-wasm/scripts/build-pkg.sh` for admission.

The [quickstart](../../QUICK_START.md) uses an independently authored definition.
`bun run typecheck` and `bun test` exercise the profile, compiler, builders and
native/WASM checker contracts. The public frontend has no remote deployment or
product-server commands.
