#!/usr/bin/env bun
/**
 * `norn setup`: download and verify prebuilt local runtime artifacts
 * (`norn-checker-wasm`, `norn-dev-host`, and `norn` CLI) from GitHub Releases
 * (or a `--base-url` mirror) into `~/.cache/norn/runtime/`.
 *
 * Fail-closed verification:
 * 1. Fetches `SHA256SUMS` and checks that every required archive has a single
 *    64-hex SHA-256 entry.
 * 2. Downloads all three archives into an isolated temporary directory with
 *    size and time bounds.
 * 3. Verifies EVERY archive's SHA-256 against `SHA256SUMS` BEFORE extracting
 *    anything. Any mismatch aborts without touching the cache directory.
 * 4. Validates archive entries (rejects absolute paths, `..`, and symlinks)
 *    and required files before staging and atomically swapping into
 *    `<cacheRoot>/runtime`.
 */
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import {
  chmodSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs"
import { tmpdir } from "node:os"
import { join, posix } from "node:path"
import { cachedCliPath, cachedDevHostPath, defaultCacheRoot } from "./runtime-cache.ts"

export const DEFAULT_REPO = "Norn-cloud/norn"
export const SETUP_DOWNLOAD_TIMEOUT_MS = 600_000
const MAX_SUMS_BYTES = 1024 * 1024
const MAX_ARCHIVE_BYTES = 256 * 1024 * 1024

export interface SetupOptions {
  readonly version?: string
  readonly baseUrl?: string
  readonly cacheDir?: string
  readonly repo?: string
  readonly platform?: string
  readonly env?: Readonly<Record<string, string | undefined>>
  readonly timeoutMs?: number
  readonly stdout?: (text: string) => void
  readonly stderr?: (text: string) => void
}

export interface SetupResult {
  readonly version: string
  readonly platform: string
  readonly baseUrl: string
  readonly runtimeDir: string
  readonly checkerWasmDir: string
  readonly devHostBin: string
  readonly cliBin: string
  readonly sha256: Readonly<Record<string, string>>
}

const VALID_PLATFORMS = new Set(["linux-x64", "linux-arm64", "macos-arm64", "macos-x64"])

/** Map Node/Bun `process.platform` + `process.arch` to the release platform key. */
export function detectReleasePlatform(
  platform: string = process.platform,
  arch: string = process.arch,
): string {
  let os: string
  if (platform === "linux") os = "linux"
  else if (platform === "darwin") os = "macos"
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

/** Reject remote plaintext HTTP; allow https or local loopback mirrors for offline/tests. */
export function isFetchableUrl(url: string): boolean {
  return url.startsWith("https://") || /^http:\/\/(127\.0\.0\.1|localhost|\[::1\])(:\d+)?(\/|$)/.test(url)
}

export function resolveReleaseBaseUrl(options: {
  readonly version?: string
  readonly baseUrl?: string
  readonly repo?: string
  readonly env?: Readonly<Record<string, string | undefined>>
}): { readonly version: string; readonly baseUrl: string } {
  const env = options.env ?? process.env
  const version = options.version || env.NORN_VERSION || "latest"
  const rawBase = options.baseUrl || env.NORN_BASE_URL
  const repo = options.repo || env.NORN_REPO || DEFAULT_REPO
  const baseUrl = rawBase
    ? rawBase.replace(/\/+$/, "")
    : version === "latest"
      ? `https://github.com/${repo}/releases/latest/download`
      : `https://github.com/${repo}/releases/download/${encodeURIComponent(version)}`
  if (!isFetchableUrl(baseUrl)) {
    throw new Error(`refusing non-https release URL ${baseUrl} (plain http is allowed only on loopback)`)
  }
  return { version, baseUrl }
}

/**
 * Strictly parse a GNU `sha256sum` file.
 *
 * Accepts `<64 lowercase hex>  <name>` or `<64 lowercase hex> *<name>`.
 * Fails closed if the same filename appears more than once (no single checksum)
 * or if a non-empty line is malformed.
 */
export function parseSha256Sums(content: string): ReadonlyMap<string, string> {
  const sums = new Map<string, string>()
  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line) continue
    const match = /^([0-9a-f]{64}) [ *](.+)$/.exec(line)
    if (!match) throw new Error(`malformed SHA256SUMS line: ${line}`)
    const digest = match[1]!
    const name = match[2]!
    if (sums.has(name)) throw new Error(`SHA256SUMS lists ${name} more than once`)
    sums.set(name, digest)
  }
  return sums
}

function sha256Hex(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex")
}

async function fetchBounded(url: string, limit: number, timeoutMs: number): Promise<Uint8Array> {
  const response = await fetch(url, { redirect: "follow", signal: AbortSignal.timeout(timeoutMs) })
  if (!response.ok || !response.body) throw new Error(`HTTP ${response.status} fetching ${url}`)
  const declared = Number(response.headers.get("content-length") ?? "NaN")
  if (Number.isFinite(declared) && declared > limit) {
    await response.body.cancel()
    throw new Error(`${url} exceeds size limit (${declared} > ${limit} bytes)`)
  }
  const chunks: Uint8Array[] = []
  let size = 0
  const reader = response.body.getReader()
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    size += value.byteLength
    if (size > limit) {
      await reader.cancel()
      throw new Error(`${url} exceeded size limit (${limit} bytes)`)
    }
    chunks.push(value)
  }
  const bytes = new Uint8Array(size)
  let offset = 0
  for (const chunk of chunks) {
    bytes.set(chunk, offset)
    offset += chunk.byteLength
  }
  return bytes
}

function assertSafeTarEntries(archivePath: string, assetName: string): void {
  const list = spawnSync("tar", ["-tzf", archivePath], { encoding: "utf8" })
  if (list.status !== 0) {
    throw new Error(`failed to inspect archive ${assetName}: ${list.stderr.trim() || "tar exited non-zero"}`)
  }
  for (const rawEntry of list.stdout.split(/\r?\n/)) {
    const entry = rawEntry.trim()
    if (!entry) continue
    const normalized = posix.normalize(entry)
    if (entry.startsWith("/") || normalized === ".." || normalized.startsWith("../") || entry.includes("\0")) {
      throw new Error(`refusing unsafe archive entry '${entry}' in ${assetName}`)
    }
  }
}

function assertNoSymlinks(dir: string, assetName: string): void {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    const stat = lstatSync(full)
    if (stat.isSymbolicLink()) {
      throw new Error(`refusing symlink '${entry.name}' in ${assetName}`)
    }
    if (stat.isDirectory()) assertNoSymlinks(full, assetName)
  }
}

function extractArchive(archivePath: string, destDir: string, assetName: string): void {
  assertSafeTarEntries(archivePath, assetName)
  mkdirSync(destDir, { recursive: true })
  const run = spawnSync("tar", ["-xzf", archivePath, "-C", destDir], { encoding: "utf8" })
  if (run.status !== 0) {
    throw new Error(`failed to extract ${assetName}: ${run.stderr.trim() || "tar exited non-zero"}`)
  }
  assertNoSymlinks(destDir, assetName)
}

function assertRegularFile(path: string, label: string): void {
  if (!existsSync(path)) throw new Error(`${label} is missing from the extracted archive`)
  if (!lstatSync(path).isFile()) throw new Error(`${label} is not a regular file`)
}

/**
 * Download, verify, and install the prebuilt local runtime into `<cacheRoot>/runtime`.
 *
 * Guarantees fail-closed behavior: if `SHA256SUMS` is missing an asset, any
 * download fails, or any archive's SHA-256 differs from `SHA256SUMS`, this
 * throws before extracting anything and without creating or modifying `cacheDir`.
 */
export async function setupLocalRuntime(options: SetupOptions = {}): Promise<SetupResult> {
  const env = options.env ?? process.env
  const platform = options.platform ?? detectReleasePlatform()
  if (!VALID_PLATFORMS.has(platform)) {
    throw new Error(`unsupported platform '${platform}': expected one of ${[...VALID_PLATFORMS].join(", ")}`)
  }
  const { version, baseUrl } = resolveReleaseBaseUrl({
    ...(options.version !== undefined ? { version: options.version } : {}),
    ...(options.baseUrl !== undefined ? { baseUrl: options.baseUrl } : {}),
    ...(options.repo !== undefined ? { repo: options.repo } : {}),
    env,
  })
  const cacheRoot = options.cacheDir ?? defaultCacheRoot(env)
  const timeoutMs = options.timeoutMs ?? SETUP_DOWNLOAD_TIMEOUT_MS

  const wasmAsset = "norn-checker-wasm.tar.gz"
  const devHostAsset = `norn-dev-host-${platform}.tar.gz`
  const cliAsset = `norn-${platform}.tar.gz`
  const assets = [wasmAsset, devHostAsset, cliAsset] as const

  // Phase 1: Download & verify strictly in a temporary directory. Cache is untouched until verified.
  const workDir = mkdtempSync(join(tmpdir(), "norn-setup-"))
  try {
    const sumsBytes = await fetchBounded(`${baseUrl}/SHA256SUMS`, MAX_SUMS_BYTES, timeoutMs)
    const sums = parseSha256Sums(new TextDecoder().decode(sumsBytes))

    const expectedDigests: Record<string, string> = {}
    for (const asset of assets) {
      const want = sums.get(asset)
      if (!want) {
        throw new Error(`SHA256SUMS has no checksum entry for ${asset}`)
      }
      expectedDigests[asset] = want
    }

    const downloaded = new Map<string, string>()
    for (const asset of assets) {
      const bytes = await fetchBounded(`${baseUrl}/${asset}`, MAX_ARCHIVE_BYTES, timeoutMs)
      const actual = sha256Hex(bytes)
      const expected = expectedDigests[asset]!
      if (actual !== expected) {
        throw new Error(
          `checksum mismatch for ${asset}: expected ${expected}, got ${actual}; cache was not modified`,
        )
      }
      const archivePath = join(workDir, asset)
      writeFileSync(archivePath, bytes)
      downloaded.set(asset, archivePath)
    }

    // Phase 2: Extract verified archives in temp dir and check required payloads.
    const wasmExtract = join(workDir, "extract-wasm")
    const devHostExtract = join(workDir, "extract-dev-host")
    const cliExtract = join(workDir, "extract-cli")

    extractArchive(downloaded.get(wasmAsset)!, wasmExtract, wasmAsset)
    assertRegularFile(join(wasmExtract, "norn_checker_wasm.js"), `${wasmAsset}: norn_checker_wasm.js`)
    assertRegularFile(join(wasmExtract, "norn_checker_wasm_bg.wasm"), `${wasmAsset}: norn_checker_wasm_bg.wasm`)

    extractArchive(downloaded.get(devHostAsset)!, devHostExtract, devHostAsset)
    assertRegularFile(join(devHostExtract, "norn-dev-host"), `${devHostAsset}: norn-dev-host`)

    extractArchive(downloaded.get(cliAsset)!, cliExtract, cliAsset)
    assertRegularFile(join(cliExtract, "norn"), `${cliAsset}: norn`)

    // Phase 3: Stage on the same filesystem as the cache and swap into place atomically.
    mkdirSync(cacheRoot, { recursive: true })
    const runtimeDir = join(cacheRoot, "runtime")
    const stagingDir = join(cacheRoot, `.runtime.staging.${process.pid}.${Date.now()}`)
    const backupDir = join(cacheRoot, `.runtime.backup.${process.pid}.${Date.now()}`)
    try {
      mkdirSync(join(stagingDir, "checker-wasm"), { recursive: true })
      mkdirSync(join(stagingDir, "bin"), { recursive: true })

      cpSync(wasmExtract, join(stagingDir, "checker-wasm"), { recursive: true })
      const stagedDevHost = join(stagingDir, "bin", "norn-dev-host")
      const stagedCli = join(stagingDir, "bin", "norn")
      cpSync(join(devHostExtract, "norn-dev-host"), stagedDevHost)
      chmodSync(stagedDevHost, 0o755)
      cpSync(join(cliExtract, "norn"), stagedCli)
      chmodSync(stagedCli, 0o755)

      writeFileSync(
        join(stagingDir, "manifest.json"),
        `${JSON.stringify(
          {
            format: "norn-local-runtime/1",
            version,
            platform,
            baseUrl,
            sha256: expectedDigests,
          },
          null,
          2,
        )}\n`,
      )

      let backedUp = false
      if (existsSync(runtimeDir)) {
        renameSync(runtimeDir, backupDir)
        backedUp = true
      }
      try {
        renameSync(stagingDir, runtimeDir)
      } catch (error) {
        if (backedUp && !existsSync(runtimeDir) && existsSync(backupDir)) {
          renameSync(backupDir, runtimeDir)
        }
        throw error
      }
      if (backedUp) rmSync(backupDir, { recursive: true, force: true })
    } catch (error) {
      rmSync(stagingDir, { recursive: true, force: true })
      throw error
    }

    return {
      version,
      platform,
      baseUrl,
      runtimeDir,
      checkerWasmDir: join(runtimeDir, "checker-wasm"),
      devHostBin: cachedDevHostPath(cacheRoot),
      cliBin: cachedCliPath(cacheRoot),
      sha256: expectedDigests,
    }
  } finally {
    rmSync(workDir, { recursive: true, force: true })
  }
}

const USAGE = "usage: norn setup [--version TAG] [--base-url URL] [--cache-dir DIR] [--repo OWNER/REPO]\n"

export function parseSetupArgs(args: readonly string[]): { readonly help: true } | {
  readonly help: false
  readonly version?: string
  readonly baseUrl?: string
  readonly cacheDir?: string
  readonly repo?: string
} {
  let version: string | undefined
  let baseUrl: string | undefined
  let cacheDir: string | undefined
  let repo: string | undefined
  for (let i = 0; i < args.length; i++) {
    const arg = args[i]
    if (arg === "--help" || arg === "-h") return { help: true }
    const next = args[i + 1]
    if (arg === "--version") {
      if (!next || next.startsWith("-")) throw new Error("--version requires a tag")
      version = next
      i++
    } else if (arg === "--base-url") {
      if (!next || next.startsWith("-")) throw new Error("--base-url requires a URL")
      baseUrl = next
      i++
    } else if (arg === "--cache-dir") {
      if (!next || next.startsWith("-")) throw new Error("--cache-dir requires a directory path")
      cacheDir = next
      i++
    } else if (arg === "--repo") {
      if (!next || next.startsWith("-")) throw new Error("--repo requires owner/name")
      repo = next
      i++
    } else {
      throw new Error(`unknown option: ${arg}`)
    }
  }
  return {
    help: false,
    ...(version !== undefined ? { version } : {}),
    ...(baseUrl !== undefined ? { baseUrl } : {}),
    ...(cacheDir !== undefined ? { cacheDir } : {}),
    ...(repo !== undefined ? { repo } : {}),
  }
}

export async function setupCli(
  args: readonly string[],
  io: {
    readonly env?: Readonly<Record<string, string | undefined>>
    readonly stdout?: (text: string) => void
    readonly stderr?: (text: string) => void
  } = {},
): Promise<number> {
  const stdout = io.stdout ?? ((text: string) => { process.stdout.write(text) })
  const stderr = io.stderr ?? ((text: string) => { process.stderr.write(text) })
  let parsed: ReturnType<typeof parseSetupArgs>
  try {
    parsed = parseSetupArgs(args)
  } catch (error) {
    stderr(`norn setup: ${error instanceof Error ? error.message : String(error)}\n${USAGE}`)
    return 2
  }
  if (parsed.help) {
    stdout(USAGE)
    return 0
  }
  try {
    const result = await setupLocalRuntime({
      ...(parsed.version !== undefined ? { version: parsed.version } : {}),
      ...(parsed.baseUrl !== undefined ? { baseUrl: parsed.baseUrl } : {}),
      ...(parsed.cacheDir !== undefined ? { cacheDir: parsed.cacheDir } : {}),
      ...(parsed.repo !== undefined ? { repo: parsed.repo } : {}),
      ...(io.env !== undefined ? { env: io.env } : {}),
    })
    stdout(
      `norn setup: installed ${result.platform} runtime (${result.version})\n`
      + `  checker-wasm: ${result.checkerWasmDir}\n`
      + `  norn-dev-host: ${result.devHostBin}\n`
      + `  norn: ${result.cliBin}\n`,
    )
    return 0
  } catch (error) {
    stderr(`norn setup: ${error instanceof Error ? error.message : String(error)}\n`)
    return 1
  }
}

if (import.meta.main) {
  process.exit(await setupCli(process.argv.slice(2)))
}
