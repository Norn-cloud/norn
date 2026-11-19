# Your first Norn project (alpha)

This takes you from an empty directory to a running Norn project on your own machine: define a product type and an action, run the dev host, call the action, see what Norn refuses and why, and repair it.

Norn is in alpha. Read "What works where" first.

## What works where

- **Bun 1.3.x** is required (`bun --version`). The TypeScript CLI runs on Bun; Node cannot run it yet.
- **Linux x64:** the path below is the one that is exercised.
- **Linux arm64 and macOS:** building from source is expected to work, but nobody has run this tutorial there yet. Tell us if it works.
- **Windows:** use WSL (Linux x64) and follow the Linux path.

## Which `norn`

Two programs are called `norn` (renaming one is [#1451](https://github.com/Norn-cloud/norn/issues/1451)):

- the **TypeScript `norn`** in `tools/norn-ts`: `init`, `dev`, `fmt`, `check` and `compile` (typed sources), `profile`, `question`, `dashboard`, `plan`, `apply`, `explain`. Every `norn` command in this tutorial is this one. Inside a project made by `norn init` it runs as `bunx norn`.
- the **Rust `norn`** in `crates/norn-cli`: `check` (compiled JSON IR), `eval`, `explain`, `mcp`, `remote`, `cell`, `source`, `capabilities`, `openapi`, `source-digest`. The drop releases attach this one.

## 1. Build from source

No release carries the TypeScript CLI, the checker WASM or the dev host yet. From `drop-29` on, a `drop-*` release attaches the Rust `norn` for Linux and macOS, the TypeScript SDK, the Python client, `install.sh` and `SHA256SUMS` (what [`scripts/release/assets.sh`](../../scripts/release/assets.sh) builds); none of these is needed for this tutorial.

You need Rust 1.98.0 with the `wasm32-unknown-unknown` target, `wasm-bindgen-cli` 0.2.129, Bun 1.3.14, and Node with `npx` (the checker build runs binaryen's `wasm-opt` through it). Build in a checkout of the repository:

```sh
git clone https://github.com/Norn-cloud/norn && cd norn
NORN=$PWD
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.129
crates/norn-checker-wasm/scripts/build-pkg.sh
cargo build --locked --release -p norn-cell-do --bin norn-dev-host
(cd tools/norn-ts && bun install --frozen-lockfile)
export NORN_CHECKER_WASM_PKG="$NORN/target/norn-checker-wasm/pkg"
export NORN_DEV_HOST_BIN="$NORN/target/release/norn-dev-host"
```

`NORN_CHECKER_WASM_PKG` is the checker that `norn check` and `norn dev` load; `NORN_DEV_HOST_BIN` is the native host that `norn dev` runs. Keep both set in every terminal you use below. The [quickstart](../../QUICK_START.md) has the same build and the release path.

## 2. Create a project

Outside the checkout:

```sh
bun "$NORN/tools/norn-ts/src/cli.ts" init shop
cd shop
```

`norn init` writes these files:
- `norn.json`: the project, its scope (`org=local`, `env=dev`) and where its cell lives;
- `src/product.ts`: a `shop.Product` type and a `shop.product.add` action;
- `AGENTS.md`: the rules for a coding agent working in the project.

It then runs `bun install`. Run from a checkout, the project depends on that checkout's `tools/norn-ts` (a `file:` dependency), so `bunx norn` in the project is the CLI you just built.

## 3. Check

```sh
bunx norn check src --org local --env dev
```

The output is JSON. `"admitted": true` means the definitions are consistent. The receipt is a hash of exactly what was checked.

## 4. Run it

```sh
bunx norn dev
```

`norn dev` serves the project's cell on `127.0.0.1` and prints its URL. Every save in `src/` is compiled, checked and installed as the next revision, or refused, in which case the last good revision keeps serving. The session token is in `.norn/dev.token`. From a second terminal:

```sh
URL=$(bun -e 'console.log(JSON.parse(require("fs").readFileSync(".norn/dev.json","utf8")).url)')
TOKEN=$(cat .norn/dev.token)
curl -s -X POST -H "authorization: Bearer $TOKEN" \
  --data '{"input":{"sku":"SKU-1","name":"Ring","price":"19.99"},"idempotencyKey":"add-SKU-1"}' \
  "$URL/v1/actions/shop.product.add?orgId=local&environmentId=dev"
curl -s -H "authorization: Bearer $TOKEN" "$URL/v1/objects/shop.Product/SKU-1?orgId=local&environmentId=dev"
```

The first call commits and returns a receipt; the second reads the product back. Decimals go into an action as text (`"19.99"`) so they stay exact.

## 5. Explain

```sh
bunx norn explain action shop.product.add
bunx norn explain concept shop.Product
```

`norn explain` shows what a declaration reads and writes, the laws that apply to it, and why a proof is unknown. Add `--json` for the same report as data.

## 6. Break it, then repair it

In `src/product.ts`, change the identity to a property that does not exist:

```ts
identity: [{ name: "primary", properties: ["skuu"] }],
```

Save. `norn dev` refuses the change and keeps serving the previous revision. `bunx norn explain action shop.product.add` prints the refusal as a code frame: the file, line and column (`src/product.ts:7`), the code `cli.compile.source_refused` with the compiler's own finding (`'"skuu"'` is not one of the type's properties), a caret under `"skuu"`, and a `fix:` line. Add `--json` for the same finding as a `norn/diagnostic@2` record.

Put `"sku"` back and save; the next revision installs.

## Not in the alpha yet

- A `norn call` command (until then, use `curl` as above).
- Prebuilt TypeScript CLI, checker or dev host files: build them from source as in step 1.

`norn plan` and `norn apply` send a pack to a running hosted cell (`--url`), and the Rust `norn mcp` serves the available operations to an agent over MCP stdio.
