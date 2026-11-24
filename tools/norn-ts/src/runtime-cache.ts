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
 * - `current`                     atomic pointer file naming the active `<genId>`
 * - `generations/<genId>/`        immutable generation directory containing:
 *   - `checker-wasm/`             `norn_checker_wasm.js` + `norn_checker_wasm_bg.wasm`
 *   - `bin/norn-dev-host`         native dev host binary
 *   - `bin/norn`                  native Rust CLI binary
 *   - `manifest.json`             verified version/platform/digest record
 *
 * Backward compatibility: if `<cacheRoot>/runtime/current` does not point to a
 * valid generation directory, discovery falls back to the direct layout under
 * `<cacheRoot>/runtime/{checker-wasm,bin,manifest.json}`.
 */
import { lstatSync, readFileSync, statSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

export const RUNTIME_MANIFEST_FORMAT = "norn-local-runtime/1"

export interface RuntimeManifest {
  readonly format: string
  readonly version: string
  readonly platform: string
  readonly baseUrl: string
  readonly sha256: Readonly<Record<string, string>>
}

const VALID_GENERATION_ID = /^[A-Za-z0-9._-]+$/

function isDirectory(path: string): boolean {
  try {
    return statSync(path).isDirectory()
  } catch {
    return false
  }
}

function isRegularFileNoFollow(path: string): boolean {
  try {
    return lstatSync(path).isFile()
  } catch {
    return false
  }
}

/**
 * Normalize a Node `process.platform`-`process.arch` string (e.g. `darwin-arm64`)
 * to the release platform key used in release assets and `manifest.json`
 * (e.g. `macos-arm64`).
 */
export function toReleasePlatform(nodePlatform: string): string {
  return nodePlatform.startsWith("darwin-")
    ? `macos-${nodePlatform.slice("darwin-".length)}`
    : nodePlatform
}

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

/** Read the active generation ID from `<cacheDir>/runtime/current`, if valid. */
export function readActiveGenerationId(cacheDir: string): string | undefined {
  const pointerPath = join(cacheDir, "runtime", "current")
  if (!isRegularFileNoFollow(pointerPath)) return undefined
  let raw: string
  try {
    raw = readFileSync(pointerPath, "utf8").trim()
  } catch {
    return undefined
  }
  if (!raw || raw === "." || raw === ".." || !VALID_GENERATION_ID.test(raw)) return undefined
  const genDir = join(cacheDir, "runtime", "generations", raw)
  return isDirectory(genDir) ? raw : undefined
}

/**
 * Resolve the active runtime directory under `<cacheDir>/runtime`:
 * - `<cacheDir>/runtime/generations/<genId>` when `<cacheDir>/runtime/current`
 *   names an existing generation directory;
 * - otherwise `<cacheDir>/runtime` (legacy direct layout or empty cache).
 */
export function activeRuntimeDir(cacheDir: string): string {
  const genId = readActiveGenerationId(cacheDir)
  if (genId) return join(cacheDir, "runtime", "generations", genId)
  return join(cacheDir, "runtime")
}

/** `<activeRuntimeDir>/checker-wasm`, or `undefined` when no cache root applies. */
export function cachedCheckerWasmDir(env: Readonly<Record<string, string | undefined>> = process.env): string | undefined {
  const root = nornCacheRoot(env)
  return root ? join(activeRuntimeDir(root), "checker-wasm") : undefined
}

/** `<activeRuntimeDir>/bin/norn-dev-host`. */
export function cachedDevHostPath(cacheDir: string): string {
  return join(activeRuntimeDir(cacheDir), "bin", "norn-dev-host")
}

/** `<activeRuntimeDir>/bin/norn`. */
export function cachedCliPath(cacheDir: string): string {
  return join(activeRuntimeDir(cacheDir), "bin", "norn")
}

/** `<activeRuntimeDir>/manifest.json`. */
export function cachedRuntimeManifestPath(cacheDir: string): string {
  return join(activeRuntimeDir(cacheDir), "manifest.json")
}
