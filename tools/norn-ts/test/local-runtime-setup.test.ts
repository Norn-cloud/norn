import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  utimesSync,
  writeFileSync,
} from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { gzipSync } from "node:zlib"
import type { BuildInfo } from "../src/build-info.ts"
import { CHECKER_PKG_ENV, checkerPackageDir, loadChecker } from "../src/checker-abi.ts"
import { hostBinary, hostLookup } from "../src/dev.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { ProjectError } from "../src/project.ts"
import {
  RUNTIME_MANIFEST_FORMAT,
  activeRuntimeDir,
  cachedCheckerWasmDir,
  detectPlatform,
  readActiveGenerationId,
} from "../src/runtime-cache.ts"
import {
  extractArchive,
  fetchBounded,
  inspectTarGzArchive,
  isFetchableUrl,
  parseSha256Sums,
  resolveReleaseBaseUrl,
  sanitizeUrlForDiagnostic,
  setupLocalRuntime,
  withPublishLock,
} from "../src/setup.ts"

const enc = new TextEncoder()
const sha256 = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex")

async function withScratch<T>(fn: (root: string) => Promise<T> | T): Promise<T> {
  const root = mkdtempSync(join(tmpdir(), "norn-local-runtime-"))
  try {
    return await fn(root)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}

async function runAsync(
  cmd: string[],
  env?: Record<string, string | undefined>,
): Promise<{ readonly status: number; readonly stdout: string; readonly stderr: string }> {
  const proc = Bun.spawn(cmd, { ...(env ? { env } : {}), stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr, status] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  return { status, stdout, stderr }
}

function makeTarGz(files: Record<string, string | Uint8Array>): Uint8Array {
  const dir = mkdtempSync(join(tmpdir(), "norn-tar-"))
  try {
    const names: string[] = []
    for (const [name, content] of Object.entries(files)) {
      writeFileSync(join(dir, name), content)
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

function finalizeTarChecksum(header: Uint8Array): void {
  header.fill(0x20, 148, 156)
  let sum = 0
  for (let i = 0; i < 512; i++) sum += header[i]!
  header.set(enc.encode(`${sum.toString(8).padStart(6, "0")}\0 `), 148)
}

function makeRawTarGz(
  entries: ReadonlyArray<{
    readonly name: string
    readonly typeflag: string
    readonly content?: Uint8Array
    readonly declaredSize?: number
    readonly linkname?: string
  }>,
): Uint8Array {
  const blocks: Uint8Array[] = []
  const writeAscii = (target: Uint8Array, offset: number, text: string, maxLen: number) => {
    target.set(enc.encode(text).subarray(0, maxLen), offset)
  }

  for (const entry of entries) {
    const header = new Uint8Array(512)
    const content = entry.content ?? new Uint8Array(0)
    const size = entry.declaredSize ?? content.length
    writeAscii(header, 0, entry.name, 100)
    writeAscii(header, 100, "0000644\0", 8)
    writeAscii(header, 108, "0000000\0", 8)
    writeAscii(header, 116, "0000000\0", 8)
    writeAscii(header, 124, `${size.toString(8).padStart(11, "0")}\0`, 12)
    writeAscii(header, 136, "00000000000\0", 12)
    header[156] = entry.typeflag.charCodeAt(0)
    if (entry.linkname) writeAscii(header, 157, entry.linkname, 100)
    writeAscii(header, 257, "ustar\0", 6)
    writeAscii(header, 263, "00", 2)
    finalizeTarChecksum(header)
    blocks.push(header)

    if (content.length > 0) {
      const data = new Uint8Array(Math.ceil(content.length / 512) * 512)
      data.set(content, 0)
      blocks.push(data)
    }
  }
  blocks.push(new Uint8Array(1024))

  const tar = new Uint8Array(blocks.reduce((acc, b) => acc + b.length, 0))
  let offset = 0
  for (const block of blocks) {
    tar.set(block, offset)
    offset += block.length
  }
  return gzipSync(tar)
}

function encodePax(records: ReadonlyArray<readonly [string, string]>): Uint8Array {
  const parts = records.map(([k, v]) => {
    const body = ` ${k}=${v}\n`
    const bodyLen = enc.encode(body).length
    let len = bodyLen + 1
    while (String(len).length + bodyLen !== len) len = String(len).length + bodyLen
    return enc.encode(`${len}${body}`)
  })
  const out = new Uint8Array(parts.reduce((acc, p) => acc + p.length, 0))
  let off = 0
  for (const p of parts) {
    out.set(p, off)
    off += p.length
  }
  return out
}

interface FixtureRelease {
  readonly baseUrl: string
  readonly close: () => void
  readonly digests: Record<string, string>
}

function startReleaseServer(
  platform: string,
  overrides: {
    readonly fixtureTag?: string
    readonly corruptAsset?: string
    readonly omitSumFor?: string
    readonly extraFiles?: Record<string, Uint8Array>
    readonly replaceAsset?: Record<string, Uint8Array>
  } = {},
): FixtureRelease {
  const fixtureTag = overrides.fixtureTag ?? "cached-runtime"
  const files: Record<string, Uint8Array> = {
    "norn-checker-wasm.tar.gz": makeTarGz({
      "norn_checker_wasm.js":
        'export function initSync() {}\n'
        + 'export function checkArtifactSet() { return JSON.stringify({ format: "norn-check/1", admitted: true, diagnostics: { diagnostics: [] }, receipt: null, receiptSha256: null }) }\n'
        + `export function version() { return JSON.stringify({ abi: "norn-checker-abi/1", fixture: ${JSON.stringify(fixtureTag)} }) }\n`,
      "norn_checker_wasm_bg.wasm": new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]),
      "package.json": '{"name":"@norn/checker-wasm","version":"0.1.0"}\n',
      LICENSE: "Apache-2.0\n",
      NOTICE: "Norn\n",
    }),
    [`norn-dev-host-${platform}.tar.gz`]: makeTarGz({
      "norn-dev-host": `#!/bin/sh\necho ${fixtureTag}-dev-host\n`,
      LICENSE: "Apache-2.0\n",
      NOTICE: "Norn\n",
    }),
    [`norn-${platform}.tar.gz`]: makeTarGz({
      norn: `#!/bin/sh\necho ${fixtureTag}-norn-cli\n`,
      LICENSE: "Apache-2.0\n",
      NOTICE: "Norn\n",
    }),
    ...(overrides.extraFiles ?? {}),
    ...(overrides.replaceAsset ?? {}),
  }

  const digests: Record<string, string> = {}
  const sumLines: string[] = []
  for (const [name, body] of Object.entries(files)) {
    const digest = sha256(body)
    digests[name] = digest
    if (name !== overrides.omitSumFor) sumLines.push(`${digest}  ${name}`)
  }

  if (overrides.corruptAsset && files[overrides.corruptAsset]) {
    const tampered = new Uint8Array(files[overrides.corruptAsset]!)
    tampered[tampered.length - 1] = (tampered[tampered.length - 1] ?? 0) ^ 0xff
    files[overrides.corruptAsset] = tampered
  }

  const sumsBytes = enc.encode(`${sumLines.join("\n")}\n`)
  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    fetch(request) {
      const name = new URL(request.url).pathname.replace(/^\/+/, "")
      if (name === "SHA256SUMS") return new Response(sumsBytes)
      return files[name] ? new Response(files[name]) : new Response("not found", { status: 404 })
    },
  })

  return { baseUrl: `http://127.0.0.1:${server.port}`, close: () => server.stop(true), digests }
}

async function expectRedactedError(run: () => Promise<unknown> | unknown, match: RegExp, secrets = ["alice", "supersecret", "pa ss", "word"]): Promise<void> {
  let msg = ""
  try {
    await run()
  } catch (error) {
    msg = error instanceof Error ? error.message : String(error)
  }
  expect(msg).toMatch(match)
  for (const secret of secrets) expect(msg).not.toContain(secret)
}

async function expectHostRefusal(run: Promise<unknown>, messagePart: string): Promise<void> {
  try {
    await run
  } catch (error) {
    if (error instanceof ProjectError) {
      expect(error).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
      expect(error.message).toContain(messagePart)
      return
    }
    throw error
  }
  throw new Error("expected ProjectError refusal")
}

test("norn setup downloads, verifies SHA-256, publishes atomic generation, and checker-abi + dev auto-discover it", () =>
  withScratch(async (root) => {
    const platform = detectPlatform()
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

      const cacheRoot = join(xdgCache, "norn")
      const genId = readActiveGenerationId(cacheRoot)
      expect(genId).toBeDefined()
      const expectedWasmDir = cachedCheckerWasmDir({ XDG_CACHE_HOME: xdgCache })!
      const expectedDevHost = join(activeRuntimeDir(cacheRoot).dir, "bin", "norn-dev-host")
      expect(expectedWasmDir).toBe(join(cacheRoot, "runtime", "generations", genId!, "checker-wasm"))
      for (const path of [
        join(expectedWasmDir, "norn_checker_wasm.js"),
        join(expectedWasmDir, "norn_checker_wasm_bg.wasm"),
        expectedDevHost,
        join(activeRuntimeDir(cacheRoot).dir, "bin", "norn"),
      ]) {
        expect(existsSync(path)).toBe(true)
      }
      expect(statSync(expectedDevHost).mode & 0o111).toBeGreaterThan(0)

      expect(checkerPackageDir({ XDG_CACHE_HOME: xdgCache }, join(root, "no-bundled"))).toBe(expectedWasmDir)
      expect(checkerPackageDir({ XDG_CACHE_HOME: xdgCache, [CHECKER_PKG_ENV]: "/custom/override" }, join(root, "no-bundled"))).toBe("/custom/override")
      const checker = await loadChecker(checkerPackageDir({ XDG_CACHE_HOME: xdgCache }, join(root, "no-bundled")))
      expect(checker?.version()).toEqual({ abi: "norn-checker-abi/1", fixture: "cached-runtime" })

      expect(await hostBinary({ ...hostLookup({ XDG_CACHE_HOME: xdgCache }), build: undefined, packageDir: () => undefined })).toBe(expectedDevHost)
    } finally {
      release.close()
    }
  }))

test("redirects reject userinfo, reject HTTPS->HTTP downgrades (even loopback), and re-validate every hop", async () => {
  for (const url of [
    "https://github.com/Norn-cloud/norn/releases/latest/download/SHA256SUMS",
    "http://127.0.0.1:8080/SHA256SUMS",
    "http://localhost:8080/SHA256SUMS",
    "http://[::1]:8080/SHA256SUMS",
  ]) {
    expect(isFetchableUrl(url), url).toBe(true)
  }
  for (const url of [
    "http://example.invalid/SHA256SUMS",
    "http://127.0.0.1@example.invalid/SHA256SUMS",
    "https://user:pass@github.com/Norn-cloud/norn/releases/latest/download/SHA256SUMS",
    "https://token@github.com/Norn-cloud/norn/releases/latest/download/SHA256SUMS",
    "http://user:pass@127.0.0.1:8080/SHA256SUMS",
    "file:///etc/passwd",
  ]) {
    expect(isFetchableUrl(url), url).toBe(false)
  }

  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    fetch(request) {
      const url = new URL(request.url)
      const path = url.pathname
      if (path === "/downgrade") return new Response(null, { status: 302, headers: { location: "http://example.invalid/plaintext.tar.gz" } })
      if (path === "/userinfo-redirect") return new Response(null, { status: 302, headers: { location: `http://alice:supersecret@127.0.0.1:${url.port}/payload` } })
      if (path === "/relative-hop") return new Response(null, { status: 302, headers: { location: "/absolute-hop" } })
      if (path === "/absolute-hop") return new Response(null, { status: 307, headers: { location: `${url.origin}/payload` } })
      if (path === "/payload") return new Response("verified-payload")
      if (path.startsWith("/loop/")) return new Response(null, { status: 302, headers: { location: `/loop/${Number(path.slice("/loop/".length)) + 1}` } })
      return new Response("not found", { status: 404 })
    },
  })

  try {
    const base = `http://127.0.0.1:${server.port}`
    expect(sanitizeUrlForDiagnostic("https://alice:supersecret@example.com/releases/SHA256SUMS")).toBe("https://***@example.com/releases/SHA256SUMS")
    expect(sanitizeUrlForDiagnostic("https://alice:pa ss@word@example.com:notaport/path")).toBe("[malformed URL redacted]")

    for (const badInitial of [
      `http://alice:supersecret@127.0.0.1:${server.port}/payload`,
      "https://alice:supersecret@example.com/releases/a.tar.gz",
      "https://alice:pa ss@word@example.com/path",
      "https://alice:pa ss@word@example.com:notaport/path",
    ]) {
      await expectRedactedError(() => fetchBounded(badInitial, 1024, 5_000), /refusing/)
    }

    for (const badBase of [
      "https://alice:supersecret@example.com/releases",
      "https://alice:pa ss@word@example.com/path",
      "https://alice:pa ss@word@example.com:notaport/path",
    ]) {
      await expectRedactedError(() => resolveReleaseBaseUrl({ baseUrl: badBase }), /refusing/)
    }

    await expect(fetchBounded(`${base}/downgrade`, 1024, 5_000)).rejects.toThrow("refusing redirect")
    await expectRedactedError(() => fetchBounded(`${base}/userinfo-redirect`, 1024, 5_000), /refusing redirect/, ["alice", "supersecret"])

    expect(new TextDecoder().decode(await fetchBounded(`${base}/relative-hop`, 1024, 5_000))).toBe("verified-payload")
    await expect(fetchBounded(`${base}/loop/0`, 1024, 5_000)).rejects.toThrow("too many redirects")

    const seenAllowed: string[] = []
    const okBytes = await fetchBounded(
      "https://github.com/Norn-cloud/norn/releases/download/drop-1/a.tar.gz",
      1024,
      5_000,
      async (url, init) => {
        expect(init?.redirect).toBe("manual")
        seenAllowed.push(url)
        return url.startsWith("https://github.com/")
          ? new Response(null, { status: 302, headers: { location: "https://objects.githubusercontent.com/release/a.tar.gz" } })
          : new Response("cdn-bytes")
      },
    )
    expect(new TextDecoder().decode(okBytes)).toBe("cdn-bytes")
    expect(seenAllowed).toEqual([
      "https://github.com/Norn-cloud/norn/releases/download/drop-1/a.tar.gz",
      "https://objects.githubusercontent.com/release/a.tar.gz",
    ])

    for (const badLocation of [
      "http://objects.githubusercontent.com/insecure/a.tar.gz",
      "http://127.0.0.1:8080/ssrf-downgrade.tar.gz",
      "http://localhost:8080/ssrf-downgrade.tar.gz",
      "http://[::1]:8080/ssrf-downgrade.tar.gz",
      "https://alice:supersecret@example.com/release/a.tar.gz",
      "https://alice:pa ss@word@example.com/path",
      "https://alice:pa ss@word@example.com:notaport/path",
    ]) {
      const seen: string[] = []
      await expectRedactedError(
        () => fetchBounded(
          "https://github.com/Norn-cloud/norn/releases/download/drop-1/a.tar.gz",
          1024,
          5_000,
          async (url) => {
            seen.push(url)
            return new Response(null, { status: 302, headers: { location: badLocation } })
          },
        ),
        /refusing redirect|invalid redirect Location/,
      )
      expect(seen).toEqual(["https://github.com/Norn-cloud/norn/releases/download/drop-1/a.tar.gz"])
    }
  } finally {
    server.stop(true)
  }
})

test("unsafe tar.gz archives are refused BEFORE extraction", () =>
  withScratch((root) => {
    const cases: ReadonlyArray<{ readonly label: string; readonly archive: Uint8Array; readonly pattern: string; readonly maxExtractedBytes?: number }> = [
      { label: "tar-symlink.tar.gz", archive: makeRawTarGz([{ name: "evil-link", typeflag: "2", linkname: "/etc/passwd" }]), pattern: "refusing symlink" },
      { label: "tar-hardlink.tar.gz", archive: makeRawTarGz([{ name: "legit.txt", typeflag: "0", content: enc.encode("ok") }, { name: "evil-hardlink", typeflag: "1", linkname: "legit.txt" }]), pattern: "refusing hard link" },
      { label: "tar-traversal.tar.gz", archive: makeRawTarGz([{ name: "../escape.txt", typeflag: "0", content: enc.encode("pwn") }]), pattern: "refusing unsafe archive entry" },
      { label: "tar-nested-traversal.tar.gz", archive: makeRawTarGz([{ name: "sub/../../escape.txt", typeflag: "0", content: enc.encode("pwn") }]), pattern: "refusing unsafe archive entry" },
      { label: "tar-absolute.tar.gz", archive: makeRawTarGz([{ name: "/etc/evil", typeflag: "0", content: enc.encode("pwn") }]), pattern: "refusing unsafe archive entry" },
      { label: "tar-fifo.tar.gz", archive: makeRawTarGz([{ name: "pipe", typeflag: "6" }]), pattern: "refusing FIFO" },
      { label: "tar-oversized-header.tar.gz", archive: makeRawTarGz([{ name: "huge.bin", typeflag: "0", declaredSize: 300 * 1024 * 1024 }]), pattern: "exceeds maximum uncompressed size" },
      { label: "tar-oversized-content.tar.gz", archive: makeTarGz({ "big.bin": new Uint8Array(4096) }), pattern: "exceeds maximum uncompressed size", maxExtractedBytes: 1024 },
    ]
    for (const c of cases) {
      const archivePath = join(root, c.label)
      const destDir = join(root, `out-${c.label}`)
      writeFileSync(archivePath, c.archive)
      expect(() => extractArchive(archivePath, destDir, c.label, c.maxExtractedBytes !== undefined ? { maxExtractedBytes: c.maxExtractedBytes } : {})).toThrow(c.pattern)
      expect(existsSync(destDir)).toBe(false)
    }

    const validTar = join(root, "valid.tar.gz")
    writeFileSync(validTar, makeTarGz({ "hello.txt": "world" }))
    expect(inspectTarGzArchive(validTar, "valid.tar.gz")).toEqual({ entries: 1, totalBytes: 5 })
  }))

test("setupLocalRuntime preflight-inspects ALL archives BEFORE extracting ANY archive onto disk", () =>
  withScratch(async (root) => {
    const platform = detectPlatform()
    const badThird = startReleaseServer(platform, {
      replaceAsset: { [`norn-${platform}.tar.gz`]: makeRawTarGz([{ name: "norn", typeflag: "2", linkname: "/bin/sh" }]) },
    })
    try {
      const extracted: string[] = []
      await expect(setupLocalRuntime({
        baseUrl: badThird.baseUrl,
        cacheDir: join(root, "cache"),
        platform,
        extractImpl(archivePath, destDir, assetName, limits) {
          extracted.push(assetName)
          extractArchive(archivePath, destDir, assetName, limits)
        },
      })).rejects.toThrow("refusing symlink")
      expect(extracted).toEqual([])
      expect(existsSync(join(root, "cache"))).toBe(false)
    } finally {
      badThird.close()
    }
  }))

test("checkerPackageDir strictly preserves override -> bundled -> local cache precedence with all three installed", () =>
  withScratch((root) => {
    const overrideDir = join(root, "override-checker")
    const bundledDir = join(root, "bundled-checker")
    const cacheDir = join(root, "cache")
    const cachedWasmDir = join(cacheDir, "runtime", "generations", "gen-1", "checker-wasm")

    for (const dir of [overrideDir, bundledDir, cachedWasmDir]) {
      mkdirSync(dir, { recursive: true })
      writeFileSync(join(dir, "norn_checker_wasm.js"), "export function initSync() {}\n")
      writeFileSync(join(dir, "norn_checker_wasm_bg.wasm"), new Uint8Array([0x00, 0x61, 0x73, 0x6d]))
    }
    writeFileSync(join(cacheDir, "runtime", "current"), "gen-1\n")

    expect(checkerPackageDir({ [CHECKER_PKG_ENV]: overrideDir, NORN_CACHE_DIR: cacheDir }, bundledDir)).toBe(overrideDir)
    expect(checkerPackageDir({ [CHECKER_PKG_ENV]: join(root, "missing-override"), NORN_CACHE_DIR: cacheDir }, bundledDir)).toBe(join(root, "missing-override"))
    expect(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, bundledDir)).toBe(bundledDir)
    expect(checkerPackageDir({ [CHECKER_PKG_ENV]: "", NORN_CACHE_DIR: cacheDir }, bundledDir)).toBe(bundledDir)
    expect(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled"))).toBe(cachedWasmDir)
  }))

test("atomic generation replacement preserves active generation on failure and performs concurrent-safe cleanup", () =>
  withScratch(async (root) => {
    const platform = detectPlatform()
    const releaseV1 = startReleaseServer(platform, { fixtureTag: "v1" })
    const releaseV2BadArchive = startReleaseServer(platform, {
      fixtureTag: "v2-bad",
      replaceAsset: { [`norn-dev-host-${platform}.tar.gz`]: makeRawTarGz([{ name: "norn-dev-host", typeflag: "2", linkname: "/bin/sh" }]) },
    })
    const releaseV2Good = startReleaseServer(platform, { fixtureTag: "v2" })
    const cacheDir = join(root, "cache")
    const runtimeDir = join(cacheDir, "runtime")
    const generationsDir = join(runtimeDir, "generations")
    try {
      const first = await setupLocalRuntime({ baseUrl: releaseV1.baseUrl, cacheDir, platform, version: "drop-1" })
      const gen1 = readActiveGenerationId(cacheDir)
      expect(gen1).toBe(first.generationId)
      expect((await loadChecker(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled"))))?.version()).toEqual({ abi: "norn-checker-abi/1", fixture: "v1" })

      await expect(setupLocalRuntime({ baseUrl: releaseV2BadArchive.baseUrl, cacheDir, platform, version: "drop-2-bad" })).rejects.toThrow("refusing symlink")
      expect(readActiveGenerationId(cacheDir)).toBe(gen1)
      expect(existsSync(first.devHostBin)).toBe(true)

      chmodSync(runtimeDir, 0o555)
      try {
        await expect(setupLocalRuntime({ baseUrl: releaseV2Good.baseUrl, cacheDir, platform, version: "drop-2-interrupted" })).rejects.toThrow()
      } finally {
        chmodSync(runtimeDir, 0o755)
      }
      expect(readActiveGenerationId(cacheDir)).toBe(gen1)
      expect(readdirSync(generationsDir)).toEqual([gen1!])
      expect((await loadChecker(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled"))))?.version()).toEqual({ abi: "norn-checker-abi/1", fixture: "v1" })
      expect(await hostBinary({ ...hostLookup({ NORN_CACHE_DIR: cacheDir }), build: undefined, packageDir: () => undefined })).toBe(first.devHostBin)

      mkdirSync(join(generationsDir, ".staging.interrupted-999"), { recursive: true })
      writeFileSync(join(runtimeDir, ".current.999.tmp"), "interrupted-999\n")
      expect(readActiveGenerationId(cacheDir)).toBe(gen1)

      const staleGen = join(generationsDir, "0000-stale-old")
      mkdirSync(staleGen, { recursive: true })
      const tenMinutesAgo = new Date(Date.now() - 10 * 60_000)
      utimesSync(staleGen, tenMinutesAgo, tenMinutesAgo)

      const second = await setupLocalRuntime({ baseUrl: releaseV2Good.baseUrl, cacheDir, platform, version: "drop-2" })
      expect(second.generationId).not.toBe(gen1)
      expect(readActiveGenerationId(cacheDir)).toBe(second.generationId)
      expect(existsSync(join(generationsDir, gen1!))).toBe(true)
      expect(existsSync(staleGen)).toBe(false)
      expect((await loadChecker(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled"))))?.version()).toEqual({ abi: "norn-checker-abi/1", fixture: "v2" })
    } finally {
      if (existsSync(runtimeDir)) chmodSync(runtimeDir, 0o755)
      releaseV1.close()
      releaseV2BadArchive.close()
      releaseV2Good.close()
    }
  }))

test("kernel flock publish lock serializes across real OS processes, releases on SIGKILL, and never unlinks lockfile", () =>
  withScratch(async (root) => {
    const platform = detectPlatform()
    const releaseA = startReleaseServer(platform, { fixtureTag: "concurrent-a" })
    const releaseB = startReleaseServer(platform, { fixtureTag: "concurrent-b" })
    const cacheDir = join(root, "lock-cache")
    const runtimeDir = join(cacheDir, "runtime")
    const lockPath = join(runtimeDir, ".publish.lock")
    const setupModuleUrl = pathToFileURL(join(TOOL_ROOT, "src/setup.ts")).href
    try {
      mkdirSync(runtimeDir, { recursive: true })

      const events: string[] = []
      const firstHold = withPublishLock(runtimeDir, async () => {
        events.push("first:enter")
        await new Promise((wake) => setTimeout(wake, 60))
        events.push("first:exit")
      })
      await new Promise((wake) => setTimeout(wake, 15))
      const secondHold = withPublishLock(runtimeDir, async () => { events.push("second:enter", "second:exit") })
      await Promise.all([firstHold, secondHold])
      expect(events).toEqual(["first:enter", "first:exit", "second:enter", "second:exit"])
      expect(existsSync(lockPath)).toBe(true)

      const tracePath = join(root, "multiprocess-trace.log")
      const workerScript = [
        'import { appendFileSync } from "node:fs";',
        "const [modUrl, dir, trace, id] = process.argv.slice(1);",
        "const { withPublishLock } = await import(modUrl);",
        "await withPublishLock(dir, async () => {",
        "  appendFileSync(trace, `enter:${id}\\n`);",
        "  await new Promise((r) => setTimeout(r, 50));",
        "  appendFileSync(trace, `exit:${id}\\n`);",
        "});",
      ].join("\n")

      const results = await Promise.all(["p1", "p2", "p3"].map((id) =>
        runAsync([process.execPath, "-e", workerScript, setupModuleUrl, runtimeDir, tracePath, id]),
      ))
      for (const r of results) expect({ status: r.status, stderr: r.stderr }).toEqual({ status: 0, stderr: "" })
      const lines = readFileSync(tracePath, "utf8").trim().split("\n")
      expect(lines).toHaveLength(6)
      const seenIds: string[] = []
      for (let i = 0; i < lines.length; i += 2) {
        expect(lines[i]!.startsWith("enter:")).toBe(true)
        const id = lines[i]!.slice("enter:".length)
        expect(lines[i + 1]).toBe(`exit:${id}`)
        seenIds.push(id)
      }
      expect(seenIds.sort()).toEqual(["p1", "p2", "p3"])

      const holderScript = [
        "const [modUrl, dir] = process.argv.slice(1);",
        "const { withPublishLock } = await import(modUrl);",
        'await withPublishLock(dir, async () => { process.stdout.write("LOCKED\\n"); await new Promise(() => {}); });',
      ].join("\n")
      const holder = Bun.spawn([process.execPath, "-e", holderScript, setupModuleUrl, runtimeDir], { stdout: "pipe", stderr: "pipe" })
      try {
        const reader = holder.stdout.getReader()
        const decoder = new TextDecoder()
        let seen = ""
        while (!seen.includes("LOCKED\n")) {
          const { done, value } = await reader.read()
          if (done) throw new Error(`holder exited early: ${await new Response(holder.stderr).text()}`)
          seen += decoder.decode(value)
        }
        reader.releaseLock()

        await expect(withPublishLock(runtimeDir, () => {}, { timeoutMs: 60 })).rejects.toThrow("timed out waiting for runtime publish lock")
        holder.kill("SIGKILL")
        await holder.exited
        expect(await withPublishLock(runtimeDir, () => "recovered-after-sigkill", { timeoutMs: 1_000 })).toBe("recovered-after-sigkill")
        expect(existsSync(lockPath)).toBe(true)
      } finally {
        holder.kill("SIGKILL")
        await holder.exited
      }

      const [resA, resB] = await Promise.all([
        setupLocalRuntime({ baseUrl: releaseA.baseUrl, cacheDir, platform, version: "drop-a" }),
        setupLocalRuntime({ baseUrl: releaseB.baseUrl, cacheDir, platform, version: "drop-b" }),
      ])
      const activeGen = readActiveGenerationId(cacheDir)
      expect(activeGen).toBeDefined()
      expect([resA.generationId, resB.generationId]).toContain(activeGen!)
      expect(existsSync(join(activeRuntimeDir(cacheDir).dir, "bin", "norn-dev-host"))).toBe(true)
      expect(existsSync(lockPath)).toBe(true)
    } finally {
      releaseA.close()
      releaseB.close()
    }
  }))

test("legacy direct runtime cache layout remains discoverable and upgrades cleanly", () =>
  withScratch(async (root) => {
    const platform = detectPlatform()
    const release = startReleaseServer(platform, { fixtureTag: "upgraded" })
    try {
      const cacheDir = join(root, "legacy-cache")
      const legacyWasm = join(cacheDir, "runtime", "checker-wasm")
      const legacyBinDir = join(cacheDir, "runtime", "bin")
      mkdirSync(legacyWasm, { recursive: true })
      mkdirSync(legacyBinDir, { recursive: true })
      writeFileSync(join(legacyWasm, "norn_checker_wasm.js"), "")
      writeFileSync(join(legacyWasm, "norn_checker_wasm_bg.wasm"), "")
      const legacyHost = join(legacyBinDir, "norn-dev-host")
      writeFileSync(legacyHost, "#!/bin/sh\necho legacy\n")
      chmodSync(legacyHost, 0o755)

      expect(readActiveGenerationId(cacheDir)).toBeUndefined()
      expect(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled"))).toBe(legacyWasm)
      expect(await hostBinary({ ...hostLookup({ NORN_CACHE_DIR: cacheDir }), build: undefined, packageDir: () => undefined })).toBe(legacyHost)

      const upgraded = await setupLocalRuntime({ baseUrl: release.baseUrl, cacheDir, platform, version: "drop-9" })
      expect(readActiveGenerationId(cacheDir)).toBe(upgraded.generationId)
      expect(existsSync(legacyWasm)).toBe(false)
      expect(existsSync(legacyBinDir)).toBe(false)
      expect(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled"))).toBe(upgraded.checkerWasmDir)
    } finally {
      release.close()
    }
  }))

test("dev.ts validates cached norn-dev-host file type, executable mode, generation manifest requirement, platform, and version", () =>
  withScratch(async (root) => {
    const releasePlatform = detectPlatform()
    const nodePlatform = `${process.platform}-${process.arch}`
    const wrongPlatform = releasePlatform === "linux-x64" ? "macos-arm64" : "linux-x64"
    const releaseBuild = (version: string): BuildInfo => ({
      version,
      commit: "0123456789abcdef0123456789abcdef01234567",
      toolchain: "1.100.0",
      packages: { "@norn/cli": version, "@norn/authoring": version },
      hosts: {},
    })
    const resolveHost = (env: Record<string, string>, opts: { platform?: string; build?: BuildInfo } = {}) =>
      hostBinary({ ...hostLookup(env), ...opts, build: opts.build, packageDir: () => undefined })

    const dirCache = join(root, "dir-cache")
    mkdirSync(join(dirCache, "runtime", "bin", "norn-dev-host"), { recursive: true })
    await expectHostRefusal(resolveHost({ NORN_CACHE_DIR: dirCache }), "not a regular file")

    const nonExecCache = join(root, "non-exec-cache")
    mkdirSync(join(nonExecCache, "runtime", "bin"), { recursive: true })
    const nonExecBin = join(nonExecCache, "runtime", "bin", "norn-dev-host")
    writeFileSync(nonExecBin, "#!/bin/sh\necho hi\n")
    chmodSync(nonExecBin, 0o644)
    await expectHostRefusal(resolveHost({ NORN_CACHE_DIR: nonExecCache }), "not executable")

    const genCache = join(root, "gen-missing-manifest")
    const genDir = join(genCache, "runtime", "generations", "gen-no-manifest")
    mkdirSync(join(genDir, "bin"), { recursive: true })
    const genHostBin = join(genDir, "bin", "norn-dev-host")
    writeFileSync(genHostBin, "#!/bin/sh\necho hi\n")
    chmodSync(genHostBin, 0o755)
    writeFileSync(join(genCache, "runtime", "current"), "gen-no-manifest\n")
    const genEnv = { NORN_CACHE_DIR: genCache }
    const manifestPath = join(genDir, "manifest.json")

    await expectHostRefusal(resolveHost(genEnv, { platform: nodePlatform }), "missing manifest.json")

    writeFileSync(manifestPath, JSON.stringify({ format: RUNTIME_MANIFEST_FORMAT, version: "drop-28", platform: wrongPlatform }))
    await expectHostRefusal(resolveHost(genEnv, { platform: nodePlatform }), `for platform ${wrongPlatform}`)

    writeFileSync(manifestPath, JSON.stringify({ format: RUNTIME_MANIFEST_FORMAT, version: "drop-1", platform: releasePlatform }))
    await expectHostRefusal(resolveHost(genEnv, { platform: nodePlatform, build: releaseBuild("0.0.28") }), "for version drop-1, not 0.0.28")

    writeFileSync(manifestPath, JSON.stringify({ format: RUNTIME_MANIFEST_FORMAT, version: "drop-28", platform: releasePlatform }))
    expect(await resolveHost(genEnv, { platform: nodePlatform, build: releaseBuild("0.0.28") })).toBe(genHostBin)

    writeFileSync(manifestPath, "{not-json")
    await expectHostRefusal(resolveHost(genEnv, { platform: nodePlatform }), "invalid JSON")

    const dirOverride = join(root, "override-dir")
    mkdirSync(dirOverride)
    await expectHostRefusal(resolveHost({ NORN_DEV_HOST_BIN: dirOverride }), "not a regular file")
  }))

test("tampered archive or missing checksum fails closed without creating or modifying the cache", () =>
  withScratch(async (root) => {
    const platform = detectPlatform()
    const tampered = startReleaseServer(platform, { corruptAsset: `norn-dev-host-${platform}.tar.gz` })
    const missingEntry = startReleaseServer(platform, { omitSumFor: "norn-checker-wasm.tar.gz" })
    try {
      const freshCache = join(root, "fresh-cache")
      await expect(setupLocalRuntime({ baseUrl: tampered.baseUrl, cacheDir: freshCache, platform })).rejects.toThrow("checksum mismatch")
      expect(existsSync(freshCache)).toBe(false)

      const existingCache = join(root, "existing-cache")
      const sentinel = join(existingCache, "runtime", "sentinel.txt")
      mkdirSync(join(existingCache, "runtime"), { recursive: true })
      writeFileSync(sentinel, "untouched")
      await expect(setupLocalRuntime({ baseUrl: tampered.baseUrl, cacheDir: existingCache, platform })).rejects.toThrow("checksum mismatch")
      expect(readFileSync(sentinel, "utf8")).toBe("untouched")

      const missingCache = join(root, "missing-cache")
      await expect(setupLocalRuntime({ baseUrl: missingEntry.baseUrl, cacheDir: missingCache, platform })).rejects.toThrow("no checksum entry for norn-checker-wasm.tar.gz")
      expect(existsSync(missingCache)).toBe(false)

      await expect(setupLocalRuntime({ baseUrl: "http://example.invalid/releases", cacheDir: missingCache, platform })).rejects.toThrow("refusing unsafe or non-https release URL")
    } finally {
      tampered.close()
      missingEntry.close()
    }
  }))

test("scripts/dev/fetch-local-runtime.sh wraps norn setup against a local mirror", () =>
  withScratch(async (root) => {
    const platform = detectPlatform()
    const release = startReleaseServer(platform)
    try {
      const cacheDir = join(root, "wrapper-cache")
      const script = join(TOOL_ROOT, "../../scripts/dev/fetch-local-runtime.sh")
      const run = await runAsync(["bash", script, "--base-url", release.baseUrl, "--cache-dir", cacheDir])
      expect({ status: run.status, stderr: run.stderr }).toEqual({ status: 0, stderr: "" })
      expect(readActiveGenerationId(cacheDir)).toBeDefined()
      expect(existsSync(join(cachedCheckerWasmDir({ NORN_CACHE_DIR: cacheDir })!, "norn_checker_wasm.js"))).toBe(true)
      expect(existsSync(join(activeRuntimeDir(cacheDir).dir, "bin", "norn-dev-host"))).toBe(true)
      expect(existsSync(join(activeRuntimeDir(cacheDir).dir, "bin", "norn"))).toBe(true)
    } finally {
      release.close()
    }
  }))

test("parseSha256Sums rejects duplicate entries and malformed lines", () => {
  const valid = "0".repeat(64)
  expect(parseSha256Sums(`${valid}  a.tar.gz\n${valid} *b.tar.gz\n`).get("b.tar.gz")).toBe(valid)
  expect(() => parseSha256Sums(`${valid}  dup.tar.gz\n${valid}  dup.tar.gz\n`)).toThrow("more than once")
  expect(() => parseSha256Sums("not-a-hash  bad.tar.gz\n")).toThrow("malformed SHA256SUMS line")
})

test("resolveReleaseBaseUrl rejects invalid or traversal --repo values", () => {
  expect(resolveReleaseBaseUrl({ repo: "Norn-cloud/norn", env: {} }).baseUrl).toBe("https://github.com/Norn-cloud/norn/releases/latest/download")
  for (const bad of ["../attacker/repo", "owner/..", "./repo", "owner/repo?x=1", "owner/repo#frag", "just-one"]) {
    expect(() => resolveReleaseBaseUrl({ repo: bad, env: {} })).toThrow("refusing invalid repository")
  }
})

test("inspectTarGzArchive refuses tar header precedence differentials and ambiguous extended headers", () =>
  withScratch((root) => {
    const okFile = { name: "safe.txt", typeflag: "0", content: enc.encode("ok") }
    const aFile = { name: "a.txt", typeflag: "0", content: enc.encode("ok") }
    const cases: ReadonlyArray<{ readonly label: string; readonly archive: Uint8Array; readonly pattern: string }> = [
      { label: "pax-then-gnu-longname.tar.gz", archive: makeRawTarGz([{ name: "PaxHeaders/a", typeflag: "x", content: encodePax([["path", "safe.txt"]]) }, { name: "././@LongLink", typeflag: "L", content: enc.encode("../escape.txt\0") }, okFile]), pattern: "both PAX header and GNU longname" },
      { label: "gnu-longname-then-pax.tar.gz", archive: makeRawTarGz([{ name: "././@LongLink", typeflag: "L", content: enc.encode("../escape.txt\0") }, { name: "PaxHeaders/a", typeflag: "x", content: encodePax([["path", "safe.txt"]]) }, okFile]), pattern: "both GNU longname and PAX header" },
      { label: "duplicate-gnu-longname.tar.gz", archive: makeRawTarGz([{ name: "././@LongLink", typeflag: "L", content: enc.encode("a.txt\0") }, { name: "././@LongLink", typeflag: "L", content: enc.encode("b.txt\0") }, aFile]), pattern: "duplicate GNU longname" },
      { label: "duplicate-pax-header.tar.gz", archive: makeRawTarGz([{ name: "PaxHeaders/1", typeflag: "x", content: encodePax([["path", "a.txt"]]) }, { name: "PaxHeaders/2", typeflag: "x", content: encodePax([["path", "b.txt"]]) }, aFile]), pattern: "duplicate PAX extended header" },
      { label: "duplicate-pax-path-key.tar.gz", archive: makeRawTarGz([{ name: "PaxHeaders/1", typeflag: "x", content: encodePax([["path", "a.txt"], ["path", "b.txt"]]) }, aFile]), pattern: "duplicate PAX key 'path'" },
      { label: "global-pax-path.tar.gz", archive: makeRawTarGz([{ name: "pax_global_header", typeflag: "g", content: encodePax([["path", "override.txt"]]) }, aFile]), pattern: "global PAX path override" },
      { label: "pax-ustar-size-desync.tar.gz", archive: makeRawTarGz([{ name: "PaxHeaders/1", typeflag: "x", content: encodePax([["size", "1024"]]) }, { name: "a.txt", typeflag: "0", content: new Uint8Array(1024), declaredSize: 512 }]), pattern: "conflicting PAX and USTAR entry sizes" },
      { label: "unsafe-rawname-under-pax.tar.gz", archive: makeRawTarGz([{ name: "PaxHeaders/1", typeflag: "x", content: encodePax([["path", "safe.txt"]]) }, { name: "../evil.txt", typeflag: "0", content: enc.encode("ok") }]), pattern: "refusing unsafe archive entry" },
      { label: "orphaned-pax-eof.tar.gz", archive: makeRawTarGz([okFile, { name: "PaxHeaders/1", typeflag: "x", content: encodePax([["path", "dangling.txt"]]) }]), pattern: "orphaned extended tar header" },
    ]
    for (const c of cases) {
      const archivePath = join(root, c.label)
      writeFileSync(archivePath, c.archive)
      expect(() => inspectTarGzArchive(archivePath, c.label)).toThrow(c.pattern)
    }

    const validPax = join(root, "valid-pax.tar.gz")
    writeFileSync(validPax, makeRawTarGz([
      { name: "PaxHeaders/1", typeflag: "x", content: encodePax([["path", "dir/valid.txt"], ["size", "2"]]) },
      { name: "dir/valid.txt", typeflag: "0", content: enc.encode("ok") },
    ]))
    expect(inspectTarGzArchive(validPax, "valid-pax.tar.gz")).toEqual({ entries: 1, totalBytes: 2 })

    const firstBlock = (entry: Parameters<typeof makeRawTarGz>[0][number]) =>
      Bun.gunzipSync(new Uint8Array(makeRawTarGz([entry]))).subarray(0, 512)

    const singleZeroThenEntry = new Uint8Array(512 * 4)
    singleZeroThenEntry.set(firstBlock({ name: "after-zero.txt", typeflag: "0", content: new Uint8Array(0) }), 512)
    const zeroGapArchive = join(root, "zero-gap.tar.gz")
    writeFileSync(zeroGapArchive, Bun.gzipSync(singleZeroThenEntry))
    expect(() => inspectTarGzArchive(zeroGapArchive, "zero-gap.tar.gz")).toThrow("unexpected archive header after zero block")

    // Leading-NUL octal size ("\00000001000\0"): GNU tar skips one leading NUL and reads 512,
    // which would hide a trailing symlink header if the inspector stopped at byte 0.
    // UTF-8 BOM, Unicode NBSP, newline, and all-space size fields: GNU tar rejects them while TextDecoder/.trim() would accept.
    const smuggledBase = new Uint8Array(512 * 5)
    smuggledBase.set(firstBlock({ name: "decoy.txt", typeflag: "0", content: new Uint8Array(0) }), 0)
    smuggledBase.set(firstBlock({ name: "evil", typeflag: "2", linkname: "/etc" }), 1024)
    const badRawSizes: ReadonlyArray<{ readonly label: string; readonly bytes: Uint8Array }> = [
      { label: "leading-nul-size.tar.gz", bytes: enc.encode("\0" + "0000001000\0") },
      { label: "bom-size.tar.gz", bytes: new Uint8Array([0xef, 0xbb, 0xbf, 0x30, 0x30, 0x30, 0x31, 0x30, 0x30, 0x30, 0x00, 0x00]) },
      { label: "nbsp-prefix-size.tar.gz", bytes: new Uint8Array([0xc2, 0xa0, 0x30, 0x30, 0x30, 0x30, 0x31, 0x30, 0x30, 0x30, 0x00, 0x00]) },
      { label: "nbsp-suffix-size.tar.gz", bytes: new Uint8Array([0x30, 0x30, 0x30, 0x30, 0x31, 0x30, 0x30, 0x30, 0xc2, 0xa0, 0x00, 0x00]) },
      { label: "newline-prefix-size.tar.gz", bytes: enc.encode("\n0000001000\0") },
      { label: "all-spaces-size.tar.gz", bytes: enc.encode("            ") },
    ]
    for (const bad of badRawSizes) {
      const buf = new Uint8Array(smuggledBase)
      buf.set(bad.bytes, 124)
      finalizeTarChecksum(buf)
      const p = join(root, bad.label)
      writeFileSync(p, Bun.gzipSync(buf))
      expect(() => inspectTarGzArchive(p, bad.label)).toThrow("invalid tar entry size")
    }

    // All-space checksum field with signed byte sum == 0 ("Blanks in header" in GNU tar).
    const blankChecksumTar = new Uint8Array(512 * 3)
    blankChecksumTar.fill(0x20, 148, 156)
    blankChecksumTar[0] = 0x80
    blankChecksumTar[1] = 0x80
    const blankChecksumArchive = join(root, "all-spaces-checksum.tar.gz")
    writeFileSync(blankChecksumArchive, Bun.gzipSync(blankChecksumTar))
    expect(() => inspectTarGzArchive(blankChecksumArchive, "all-spaces-checksum.tar.gz")).toThrow("invalid tar header checksum")
  }))
