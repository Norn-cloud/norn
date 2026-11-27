/**
 * Shared local runtime cache paths and platform detection used by `norn setup`,
 * `norn check`, `norn explain`, and `norn dev`.
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
import { spawnSync } from "node:child_process"
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

export interface ActiveRuntimeRoot {
  readonly dir: string
  readonly isGeneration: boolean
  readonly generationId?: string
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
 * Map Node/Bun `process.platform` + `process.arch` to the release platform key
 * (`linux-x64`, `linux-arm64`, `macos-arm64`, `macos-x64`).
 *
 * Shared by both `setup.ts` and `dev.ts` so Rosetta on Apple Silicon agrees on
 * both install and discovery sides.
 */
export function detectPlatform(
  platform: string = process.platform,
  arch: string = process.arch,
): string {
  let os: string
  if (platform === "linux") os = "linux"
  else if (platform === "darwin" || platform === "macos") os = "macos"
  else throw new Error(`unsupported OS ${platform}: Linux and macOS only`)

  let cpu: string
  if (arch === "x64" || arch === "x86_64" || arch === "amd64") cpu = "x64"
  else if (arch === "arm64" || arch === "aarch64") cpu = "arm64"
  else throw new Error(`unsupported CPU ${arch}: x64 and arm64 only`)

  // Match scripts/release/install.sh: a Rosetta shell on Apple Silicon reports x64.
  if (os === "macos" && cpu === "x64" && platform === process.platform && arch === process.arch) {
    const check = spawnSync("sysctl", ["-n", "sysctl.proc_translated"], { encoding: "utf8" })
    if (check.status === 0 && check.stdout.trim() === "1") cpu = "arm64"
  }
  return `${os}-${cpu}`
}

/**
 * Convert a `<platform>-<arch>` key (such as Node's `darwin-arm64` or
 * `linux-x64`) through the shared `detectPlatform` logic.
 */
export function releasePlatformFor(nodePlatform: string): string {
  const dash = nodePlatform.indexOf("-")
  if (dash <= 0 || dash === nodePlatform.length - 1) {
    throw new Error(`unsupported platform '${nodePlatform}'`)
  }
  return detectPlatform(nodePlatform.slice(0, dash), nodePlatform.slice(dash + 1))
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

export function activeRuntimeDir(cacheDir: string): ActiveRuntimeRoot {
  const genId = readActiveGenerationId(cacheDir)
  if (genId) {
    return {
      dir: join(cacheDir, "runtime", "generations", genId),
      isGeneration: true,
      generationId: genId,
    }
  }
  return {
    dir: join(cacheDir, "runtime"),
    isGeneration: false,
  }
}

export function cachedCheckerWasmDir(env: Readonly<Record<string, string | undefined>> = process.env): string | undefined {
  const root = nornCacheRoot(env)
  return root ? join(activeRuntimeDir(root).dir, "checker-wasm") : undefined
}
