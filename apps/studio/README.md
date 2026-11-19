# studio

Norn Studio: the React web app (`src/`, built with Vite) and the Studio Worker
(`worker/`). The Worker runs Better Auth under `/api/auth`, serves the
authenticated `/api/*` routes, forwards cell requests to the Norn cell host
bound as `NORN_HOST`, and serves the built assets. The cell host decides every
read and action; Studio holds no permission logic of its own.

## Commands

Run from this directory with Bun (`bun install --frozen-lockfile` first).

| Command | What it does |
| --- | --- |
| `bun run dev` | Vite dev server for the app. |
| `bun run build` | Production build into `dist/`. |
| `bun run typecheck` | `tsc` over the app and over `worker/`. |
| `bun test` | Unit tests (`test/`, `worker/test/`). |
| `bun run e2e` | Playwright browser tests in `e2e/`. Without `E2E_BASE` they run against `vite preview` of the current build with a stubbed session; set `E2E_BASE` (and `E2E_STATE`) to test a deployed stage. |
| `bun run checker` | Builds the checker and engine WebAssembly packages from `crates/norn-checker-wasm` into `src/checker/` and `src/engine/` (not committed). Without them the build still succeeds and the canvas reports the checker as absent. |
| `bun run local --project <dir>` | After `bun run build`: serves the build on loopback against a running `norn dev` cell in `<dir>` (`scripts/local-cell.ts`). |
| `bun run deploy` | Builds the checker and app, then deploys with Alchemy (`alchemy.run.ts`). |

## Depends on

- A Norn cell host for data: the Durable Object host (`apps/norn-cell-do`) or
  `norn dev` locally.
- The Rust toolchain and `wasm-bindgen` for `bun run checker`.
- Cloudflare (Workers, D1) through Alchemy for `deploy`; `celld-config.ts`
  writes the equivalent config for a self-hosted celld deploy.
- Chrome for the Playwright tests (`PLAYWRIGHT_CHANNEL` overrides the channel).
