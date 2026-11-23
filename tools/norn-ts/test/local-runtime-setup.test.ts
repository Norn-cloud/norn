/**
 * Fast local DX runtime setup & auto-discovery tests:
 * - `norn setup --base-url ...` downloads, verifies every SHA-256 against
 *   `SHA256SUMS`, extracts to the cache directory, and `checkerPackageDir` /
 *   `loadChecker` / `hostBinary` auto-discover the cached runtime.
 * - Tampered archive / mismatched SHA-256 / missing checksum entry fails closed
 *   without creating or modifying the cache.
 * - Wrapper `scripts/dev/fetch-local-runtime.sh` works end-to-end against a
 *   local HTTP fixture server (zero network required).
 */
import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { CHECKER_PKG_ENV, checkerPackageDir, loadChecker } from "../src/checker-abi.ts"
import { hostBinary, hostLookup } from "../src/dev.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { cachedCheckerWasmDir, cachedDevHostPath } from "../src/runtime-cache.ts"
import { detectReleasePlatform, parseSha256Sums, setupLocalRuntime } from "../src/setup.ts"

const scratch = () => mkdtempSync(join(tmpdir(), "norn-local-runtime-"))
const sha256 = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex")

async function runAsync(
  cmd: string[],
  env?: Record<string, string | undefined>,
): Promise<{ readonly status: number; readonly stdout: string; readonly stderr: string }> {
  const proc = Bun.spawn(cmd, {
    ...(env ? { env } : {}),
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, stderr, status] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  return { status, stdout, stderr }
}

function makeTarGz(files: Record<string, string | Uint8Array>): Uint8Array {
  const dir = scratch()
  try {
    const names: string[] = []
    for (const [name, content] of Object.entries(files)) {
      const target = join(dir, name)
      writeFileSync(target, content)
      names.push(name)
    }
    const out = join(dir, "archive.tar.gz")
    const run = spawnSync("tar", ["-czf", out, "-C", dir, ...names], { encoding: "utf8" })
    if (run.status !== 0) throw new Error(`tar failed: ${run.stderr}`)
    return readFileSync(out)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

interface FixtureRelease {
  readonly baseUrl: string
  readonly close: () => void
  readonly digests: Record<string, string>
}

function startReleaseServer(
  platform: string,
  overrides: {
    readonly corruptAsset?: string
    readonly omitSumFor?: string
    readonly extraFiles?: Record<string, Uint8Array>
  } = {},
): FixtureRelease {
  const wasmArchive = makeTarGz({
    "norn_checker_wasm.js":
      'export function initSync() {}\n'
      + 'export function checkArtifactSet() { return JSON.stringify({ format: "norn-check/1", admitted: true, diagnostics: { diagnostics: [] }, receipt: null, receiptSha256: null }) }\n'
      + 'export function version() { return JSON.stringify({ abi: "norn-checker-abi/1", fixture: "cached-runtime" }) }\n',
    "norn_checker_wasm_bg.wasm": new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]),
    "package.json": '{"name":"@norn/checker-wasm","version":"0.1.0"}\n',
    LICENSE: "Apache-2.0\n",
    NOTICE: "Norn\n",
  })
  const devHostArchive = makeTarGz({
    "norn-dev-host": "#!/bin/sh\necho cached-dev-host\n",
    LICENSE: "Apache-2.0\n",
    NOTICE: "Norn\n",
  })
  const cliArchive = makeTarGz({
    norn: "#!/bin/sh\necho cached-norn-cli\n",
    LICENSE: "Apache-2.0\n",
    NOTICE: "Norn\n",
  })

  const wasmName = "norn-checker-wasm.tar.gz"
  const devHostName = `norn-dev-host-${platform}.tar.gz`
  const cliName = `norn-${platform}.tar.gz`

  const files: Record<string, Uint8Array> = {
    [wasmName]: wasmArchive,
    [devHostName]: devHostArchive,
    [cliName]: cliArchive,
    ...(overrides.extraFiles ?? {}),
  }

  const digests: Record<string, string> = {}
  const sumLines: string[] = []
  for (const [name, body] of Object.entries(files)) {
    const digest = sha256(body)
    digests[name] = digest
    if (name !== overrides.omitSumFor) {
      sumLines.push(`${digest}  ${name}`)
    }
  }

  if (overrides.corruptAsset && files[overrides.corruptAsset]) {
    const original = files[overrides.corruptAsset]!
    const tampered = new Uint8Array(original)
    tampered[tampered.length - 1] = (tampered[tampered.length - 1] ?? 0) ^ 0xff
    files[overrides.corruptAsset] = tampered
  }

  const sumsBytes = new TextEncoder().encode(`${sumLines.join("\n")}\n`)
  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    fetch(request) {
      const name = new URL(request.url).pathname.replace(/^\/+/, "")
      if (name === "SHA256SUMS") return new Response(sumsBytes)
      const body = files[name]
      if (body) return new Response(body)
      return new Response("not found", { status: 404 })
    },
  })

  return {
    baseUrl: `http://127.0.0.1:${server.port}`,
    close: () => server.stop(true),
    digests,
  }
}

test("norn setup downloads, verifies SHA-256, extracts to cache, and checker-abi + dev auto-discover it", async () => {
  const root = scratch()
  const platform = detectReleasePlatform()
  const release = startReleaseServer(platform)
  try {
    const xdgCache = join(root, "xdg-cache")
    const env: Record<string, string | undefined> = { ...process.env, XDG_CACHE_HOME: xdgCache }
    delete env.NORN_CACHE_DIR
    delete env.NORN_CHECKER_WASM_PKG
    delete env.NORN_DEV_HOST_BIN

    const run = await runAsync(
      [process.execPath, join(TOOL_ROOT, "src/cli.ts"), "setup", "--version", "drop-test", "--base-url", release.baseUrl],
      env,
    )
    expect({ status: run.status, stderr: run.stderr }).toEqual({ status: 0, stderr: "" })
    expect(run.stdout).toContain(`installed ${platform} runtime (drop-test)`)

    // Verify layout under $XDG_CACHE_HOME/norn/runtime/
    const expectedWasmDir = cachedCheckerWasmDir({ XDG_CACHE_HOME: xdgCache })!
    const expectedDevHost = cachedDevHostPath(join(xdgCache, "norn"))
    expect(existsSync(join(expectedWasmDir, "norn_checker_wasm.js"))).toBe(true)
    expect(existsSync(join(expectedWasmDir, "norn_checker_wasm_bg.wasm"))).toBe(true)
    expect(existsSync(expectedDevHost)).toBe(true)
    expect(statSync(expectedDevHost).mode & 0o111).toBeGreaterThan(0)
    expect(existsSync(join(xdgCache, "norn", "runtime", "bin", "norn"))).toBe(true)

    // checker-abi.ts auto-discovers cached runtime when NORN_CHECKER_WASM_PKG is unset,
    // and respects NORN_CHECKER_WASM_PKG override above the cache.
    expect(checkerPackageDir({ XDG_CACHE_HOME: xdgCache }, join(root, "no-bundled"))).toBe(expectedWasmDir)
    expect(checkerPackageDir({ XDG_CACHE_HOME: xdgCache, [CHECKER_PKG_ENV]: "/custom/override" }, join(root, "no-bundled"))).toBe("/custom/override")
    const checker = await loadChecker(checkerPackageDir({ XDG_CACHE_HOME: xdgCache }, join(root, "no-bundled")))
    expect(checker).toBeDefined()
    expect(checker!.version()).toEqual({ abi: "norn-checker-abi/1", fixture: "cached-runtime" })

    // dev.ts auto-discovers ~/.cache/norn/runtime/bin/norn-dev-host in a checkout when NORN_DEV_HOST_BIN is unset.
    const discovered = await hostBinary({
      ...hostLookup({ XDG_CACHE_HOME: xdgCache }),
      build: undefined,
      packageDir: () => undefined,
    })
    expect(discovered).toBe(expectedDevHost)
  } finally {
    release.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test("tampered archive or missing checksum fails closed without creating or modifying the cache", async () => {
  const root = scratch()
  const platform = detectReleasePlatform()
  const tampered = startReleaseServer(platform, { corruptAsset: `norn-dev-host-${platform}.tar.gz` })
  const missingEntry = startReleaseServer(platform, { omitSumFor: "norn-checker-wasm.tar.gz" })
  try {
    // 1. Fresh cache: tampered archive must fail and leave the cache directory nonexistent.
    const freshCache = join(root, "fresh-cache")
    await expect(
      setupLocalRuntime({ baseUrl: tampered.baseUrl, cacheDir: freshCache, platform }),
    ).rejects.toThrow("checksum mismatch")
    expect(existsSync(freshCache)).toBe(false)

    // 2. Existing valid cache: a later tampered setup must NOT overwrite or delete the existing cache.
    const existingCache = join(root, "existing-cache")
    const sentinel = join(existingCache, "runtime", "sentinel.txt")
    mkdirSync(join(existingCache, "runtime"), { recursive: true })
    writeFileSync(sentinel, "untouched")
    await expect(
      setupLocalRuntime({ baseUrl: tampered.baseUrl, cacheDir: existingCache, platform }),
    ).rejects.toThrow("checksum mismatch")
    expect(readFileSync(sentinel, "utf8")).toBe("untouched")

    // 3. Missing checksum entry in SHA256SUMS fails closed without touching cache.
    const missingCache = join(root, "missing-cache")
    await expect(
      setupLocalRuntime({ baseUrl: missingEntry.baseUrl, cacheDir: missingCache, platform }),
    ).rejects.toThrow("no checksum entry for norn-checker-wasm.tar.gz")
    expect(existsSync(missingCache)).toBe(false)

    // 4. Plaintext non-loopback HTTP URL is refused.
    await expect(
      setupLocalRuntime({ baseUrl: "http://example.invalid/releases", cacheDir: missingCache, platform }),
    ).rejects.toThrow("refusing non-https release URL")
  } finally {
    tampered.close()
    missingEntry.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test("scripts/dev/fetch-local-runtime.sh wraps norn setup against a local mirror", async () => {
  const root = scratch()
  const platform = detectReleasePlatform()
  const release = startReleaseServer(platform)
  try {
    const cacheDir = join(root, "wrapper-cache")
    const script = join(TOOL_ROOT, "../../scripts/dev/fetch-local-runtime.sh")
    const run = await runAsync(["bash", script, "--base-url", release.baseUrl, "--cache-dir", cacheDir])
    expect({ status: run.status, stderr: run.stderr }).toEqual({ status: 0, stderr: "" })
    expect(existsSync(join(cacheDir, "runtime", "checker-wasm", "norn_checker_wasm.js"))).toBe(true)
    expect(existsSync(join(cacheDir, "runtime", "bin", "norn-dev-host"))).toBe(true)
    expect(existsSync(join(cacheDir, "runtime", "bin", "norn"))).toBe(true)
  } finally {
    release.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test("parseSha256Sums rejects duplicate entries and malformed lines", () => {
  const valid = "0".repeat(64)
  expect(parseSha256Sums(`${valid}  a.tar.gz\n${valid} *b.tar.gz\n`).get("b.tar.gz")).toBe(valid)
  expect(() => parseSha256Sums(`${valid}  dup.tar.gz\n${valid}  dup.tar.gz\n`)).toThrow("more than once")
  expect(() => parseSha256Sums("not-a-hash  bad.tar.gz\n")).toThrow("malformed SHA256SUMS line")
})
