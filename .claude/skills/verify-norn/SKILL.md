---
name: verify-norn
description: Drive Norn's real surfaces (native `norn` CLI, hosted/dev cell HTTP API, Studio UI, and promoted verification gates) 1-by-1 using the stop-gap auto-extracted feature map (`scripts/feature_map.py` + `registry/features/`). Use when asked to verify Norn features 1-by-1, check stale features, or prove end-to-end behavior with honest evidence.
---

# Verify Norn (1-by-1 Feature Verification)

> **Stop-gap notice (#2005):** `scripts/feature_map.py` and `registry/features/` are a stop-gap ontology to be retired once TARGET Norn authoring (`concept({ ... })`, pinned packs) hosts the feature map natively in a Norn cell.

## 1-by-1 verification loop

When asked to verify Norn features 1-by-1:

1. **Check map integrity first:**
   ```bash
   python3 scripts/feature_map.py check
   ```
   If Law 1 (orphan/dangling surface), Law 2 (unprobed implemented feature or missing path/gate), or Law 3 (stale generated cards) fails, fix the registry or regenerate with `python3 scripts/feature_map.py write` before driving probes.
2. **Find what needs verification:**
   ```bash
   python3 scripts/feature_map.py list --stale
   ```
   Optionally narrow with `--domain <core-engine|api-and-hosts|authoring-and-cli|studio-ui|verification>` or `--unverified`.
3. **Inspect one feature before touching runtime:**
   ```bash
   python3 scripts/feature_map.py show <feature-id>
   ```
   Read the cited `specContracts` and linked `invariants` (promoted Lean/Quint/Kani gates or checker modules) FIRST so you know what is already formally checked versus what requires runtime/E2E probing.
4. **Drive the feature's probe** using the real surface harness below, capture action + resulting state + second-view read, and report any unmet precondition or gap honestly. Never weaken a probe to make it pass.
5. **Update `lastVerified` only after an actual run** on clean sources, recording the exact `commitSha` (`git rev-parse HEAD`) and `liveSourceDigest` (which hashes `sourcePaths`, cited invariant files, promoted-gate artifact/model files, and the feature contract: `probe`, `invariants`, `surfaces`) printed by `show <feature-id>`, then run `python3 scripts/feature_map.py write` and `python3 scripts/record_new_files.py`.

## Launch

**Hard rule:** Never run `cargo` or `rustc` on shared development hosts; run heavy Rust builds, full suites, and formal provers in CI or disposable Boat sandboxes.

- **Prebuilt native runtime (`norn`, `norn-dev-host`, WASM checker):**
  ```bash
  scripts/dev/fetch-local-runtime.sh
  ```
  (Caches verified binaries under `~/.cache/norn/runtime/`.)
- **Disposable local cell (`norn dev`):** start `norn dev` inside a fresh temporary project directory with its own `.norn/dev.json` (`format: "norn-dev/1"`) and unique loopback port. Never share a running user cell.
- **Studio UI over a local cell:**
  ```bash
  (cd apps/studio && bun run build && bun run local --project <tmp-project> --port <free-port>)
  ```
  For approval-fencing checks (requester cannot decide their own pending decision), launch a second Studio instance on another loopback port with `--as reviewer` (or `--as <named-principal>`).

## Doctor

Run this read-only check before driving a feature if anything looks off:

```bash
python3 scripts/feature_map.py check
test -f <tmp-project>/.norn/dev.json && python3 -c 'import json, sys; d=json.load(open(sys.argv[1])); print(d["format"], d["url"], d["scope"])' <tmp-project>/.norn/dev.json
curl -fsS -H "Authorization: Bearer $(cat <tmp-project>/<tokenFile>)" "<loopback-url>/v1/definitions?orgId=<orgId>&environmentId=<environmentId>" | head -c 200
```

Ready means: feature map check exits `0`, `.norn/dev.json` reports `norn-dev/1` on `127.0.0.1` / `[::1]`, and `GET /v1/definitions` (or Studio's `/api/me`) answers with the expected `orgId` and `environmentId`.

## Drive

- **CLI (`cli:norn ...`, `cli:norn-ts ...`):** execute literal Rust `norn` or TypeScript authoring (`bun run tools/norn-ts/src/cli.ts`) subcommands against disposable temp dirs; record command, exit code, stdout, and stderr.
- **HTTP API (`http:<METHOD> <PATH>`):** send `curl` requests to the loopback `norn dev` URL using the bearer token from `.norn/dev.json` (`tokenFile` for developer, `reviewer.tokenFile` for reviewer). Pass explicit scope in the query string (`?orgId=<orgId>&environmentId=<environmentId>`), include `idempotencyKey` in the JSON request body on mutating action/decision calls (e.g. `{"input":{...},"idempotencyKey":"..."}`), and verify both positive commits and fail-closed refusals.
- **Studio UI (`studio:<route>`):** prefer existing Playwright specs in `apps/studio/e2e/*.e2e.ts` or drive routes from `apps/studio/src/lib/pages.ts` using ARIA roles and accessible names (never brittle coordinates). Confirm mutations via a second read-only view or reload.
- **Formal & integrity gates (`gate:<id>`):** run Python integrity gates locally (`python3 scripts/proof_enrollment.py`, `python3 scripts/ci/check_test_wiring.py`, `python3 scripts/record_new_files.py --check`); inspect Lean/Quint/Kani gate definitions in `verification/coverage-v1/promoted-gates.json` and run them only in CI or disposable Boat sandboxes.

## Evidence

- Label evidence honestly using Norn's vocabulary: `checked-proof`, `exhaustive-finite`, `bounded`, `runtime-enforced`, `e2e-driven`, or `unverified`. Never claim a bare "verified".
- Capture the **action**, the **resulting state**, and a **second-view read** (e.g., `GET /v1/objects/{type}/{id}` after `POST /v1/actions/{actionId}`, or reloading a Studio page after saving).
- Store scratch proof artifacts under a disposable run directory outside tracked files (e.g. `/tmp/norn-verify-<run-id>/`) and never commit logs, tokens, or `.env` files.
- Report skipped or blocked paths with the exact command attempted and the unmet precondition; never claim a feature passed through a different entry point than the one tested.

## Cleanup

- Terminate only the PIDs you started (`kill "$DEV_PID" "$STUDIO_PID"`); never kill by global process name.
- Remove disposable temp project/data directories created for the run while retaining your proof summary/artifacts under `/tmp/norn-verify-<run-id>/`.

## Feature map

Generated per-domain feature cards live in [`./features/README.md`](./features/README.md) (regenerated by `python3 scripts/feature_map.py write`; do not hand-edit).
