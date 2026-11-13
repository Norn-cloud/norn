# Your first Norn project (alpha)

This takes you from an empty directory to a running Norn project on your own machine: define a product type and an action, run the dev host, call the action, see what Norn refuses and why, and repair it.

Norn is in alpha. Read "What works where" first.

## What works where

- **Bun 1.3.x** is required (`bun --version`). The CLI runs on Bun; Node cannot run it yet.
- **Linux x64:** fully supported. The release carries a prebuilt dev host.
- **Linux arm64:** the release carries a prebuilt dev host that no one has run yet. Tell us if it works.
- **macOS:** `norn check`, `norn explain` and `norn fmt` are expected to work, but nobody has run them on macOS yet. `norn dev` needs a dev host you build from source; the CLI prints the exact commands when it cannot find one.
- **Windows:** use WSL (Linux x64) and follow the Linux path.

## 1. Download and verify

Each alpha is a GitHub pre-release of `Norn-cloud/norn`, tagged `v0.1.0-alpha.N`. It carries:
- the CLI tarball, `norn-cli-<version>.tgz`;
- the authoring types, `norn-authoring-<version>.tgz`;
- one prebuilt dev host per Linux platform;
- `manifest.json` and `SHA256SUMS`.

Check what you downloaded before you install it:

```sh
VERSION=0.1.0-alpha.N
BASE=https://github.com/Norn-cloud/norn/releases/download/v$VERSION
curl -fsSLO "$BASE/SHA256SUMS"
curl -fsSLO "$BASE/norn-cli-$VERSION.tgz"
sha256sum --ignore-missing -c SHA256SUMS        # must print: norn-cli-<version>.tgz: OK
gh attestation verify "norn-cli-$VERSION.tgz" --repo Norn-cloud/norn   # on releases that carry attestations
```

What these prove:
- **`sha256sum`:** the file is the one the release lists.
- **`gh attestation verify`:** the file was built by Norn's release workflow from the tagged commit. It needs the GitHub CLI, and only releases built by that workflow carry an attestation; a release without one says so in its notes.
- **Where the trust comes from:** every source file in the packages is byte-identical to the repository at that commit. Once Norn's release workflow exists, the built files (checker WASM, dev hosts, licence texts, manifests) will come only from a run of it, and its attestation will say which run. Until then, a release's notes say how it was built.
- **What it covers:** these checks cover the files you download yourself. When `norn dev` first runs, it fetches the dev host for your platform and runs it only if its sha256 is the one this release records.

Your project's `bun.lock` pins the exact tarballs you installed. Commit it. Until it exists, nothing pins what `norn init` installed: the first install trusts the release URL, so run `norn init` only from a release you verified.

## 2. Install the CLI and create a project

```sh
bun add --global "./norn-cli-$VERSION.tgz"
norn --version                  # norn 0.1.0-alpha.N (<commit>)
norn init shop
cd shop
```

`norn init` writes these files:
- `norn.json`: the project, its scope (`org=local`, `env=dev`) and where its cell lives;
- `src/product.ts`: a `shop.Product` type and a `shop.product.add` action;
- `AGENTS.md`: the rules for a coding agent working in the project.

It then installs the project's dependencies.

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

## macOS: building the dev host

`norn dev` prints the commit to build from. With Rust installed (the version it names):

```sh
git clone https://github.com/Norn-cloud/norn && cd norn
git checkout <the commit norn dev printed>
cargo build --release -p norn-cell-do --bin norn-dev-host
export NORN_DEV_HOST_BIN="$PWD/target/release/norn-dev-host"
```

Build from that exact commit: a dev host from another commit may not match your CLI.

## Not in the alpha yet

- Deploying a project anywhere but your own machine.
- `norn plan` and `norn apply`.
- An MCP endpoint for agents.
- A `norn call` command (until then, use `curl` as above).
