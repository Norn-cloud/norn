/**
 * Shared local runtime cache paths used by `norn setup`, `norn check`,
 * `norn explain`, and `norn dev`.
 *
 * Cache root precedence:
 * 1. `NORN_CACHE_DIR`
 * 2. `$XDG_CACHE_HOME/norn`
 * 3. `$HOME/.cache/norn` (or `os.homedir()/.cache/norn` when reading `process.env`)
 *
 * Prebuilt runtime layout under `<cacheRoot>/runtime`:
 * - `checker-wasm/`         `norn_checker_wasm.js` + `norn_checker_wasm_bg.wasm`
 * - `bin/norn-dev-host`     native dev host binary
 * - `bin/norn`              native Rust CLI binary
 * - `manifest.json`         verified version/platform/digest record
 */
import { homedir } from "node:os"
import { join } from "node:path"

/**
 * Resolve the Norn cache root from `env`.
 *
 * When a caller passes a synthetic env map (such as `{}` in unit tests) with
 * none of `NORN_CACHE_DIR`, `XDG_CACHE_HOME`, or `HOME` set, returns
 * `undefined` rather than leaking the host user's home directory into tests.
 */
export function nornCacheRoot(env: Readonly<Record<string, string | undefined>> = process.env): string | undefined {
  if (env.NORN_CACHE_DIR) return env.NORN_CACHE_DIR
  if (env.XDG_CACHE_HOME) return join(env.XDG_CACHE_HOME, "norn")
  if (env.HOME) return join(env.HOME, ".cache", "norn")
  return env === process.env ? join(homedir(), ".cache", "norn") : undefined
}

/** Resolve a concrete Norn cache root, falling back to `os.homedir()`. */
export function defaultCacheRoot(env: Readonly<Record<string, string | undefined>> = process.env): string {
  return nornCacheRoot(env) ?? join(homedir(), ".cache", "norn")
}

/** `<cacheRoot>/runtime`, or `undefined` when `nornCacheRoot(env)` is unset. */
export function runtimeCacheDir(env: Readonly<Record<string, string | undefined>> = process.env): string | undefined {
  const root = nornCacheRoot(env)
  return root ? join(root, "runtime") : undefined
}

/** `<cacheRoot>/runtime/checker-wasm`, or `undefined` when no cache root applies. */
export function cachedCheckerWasmDir(env: Readonly<Record<string, string | undefined>> = process.env): string | undefined {
  const runtime = runtimeCacheDir(env)
  return runtime ? join(runtime, "checker-wasm") : undefined
}

/** `<cacheDir>/runtime/bin/norn-dev-host`. */
export function cachedDevHostPath(cacheDir: string): string {
  return join(cacheDir, "runtime", "bin", "norn-dev-host")
}

/** `<cacheDir>/runtime/bin/norn`. */
export function cachedCliPath(cacheDir: string): string {
  return join(cacheDir, "runtime", "bin", "norn")
}
