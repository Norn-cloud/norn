/**
 * Fast local DX runtime setup & auto-discovery tests:
 * - `norn setup --base-url ...` downloads, verifies every SHA-256 against
 *   `SHA256SUMS`, preflight-inspects ALL archives before extracting any,
 *   publishes immutable generations with an atomic `current` pointer, and
 *   `checkerPackageDir` / `loadChecker` / `hostBinary` discover it.
 * - Redirect hops reject userinfo, reject ANY `https:` -> `http:` downgrade
 *   (including to loopback), and re-validate every hop.
 * - Archive entries (`.tar.gz` and `.zip`) are inspected BEFORE extraction
 *   (including non-Unix-origin ZIP symlinks and bounded raw inflation against
 *   lying ZIP headers / zip bombs).
 * - Checker precedence strictly preserves override -> bundled -> cache with
 *   all three genuinely installed simultaneously.
 * - Interrupted/failed replacements and concurrent generation cleanup preserve
 *   live and recent generations safely.
 * - `hostBinary` validates cached `norn-dev-host` from a single generation root
 *   snapshot, requires `manifest.json` on generation caches, and fails closed
 *   via `cli.setup.host_unavailable`.
 * - Wrapper `scripts/dev/fetch-local-runtime.sh` works end-to-end against a
 *   local HTTP fixture server (zero network required).
 */
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
import { deflateRawSync, gzipSync } from "node:zlib"
import type { BuildInfo } from "../src/build-info.ts"
import { CHECKER_PKG_ENV, checkerPackageDir, loadChecker } from "../src/checker-abi.ts"
import { hostBinary, hostLookup } from "../src/dev.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { ProjectError } from "../src/project.ts"
import {
  RUNTIME_MANIFEST_FORMAT,
  cachedCheckerWasmDir,
  cachedCliPath,
  cachedDevHostPath,
  detectPlatform,
  readActiveGenerationId,
} from "../src/runtime-cache.ts"
import {
  crc32,
  extractArchive,
  fetchBounded,
  inspectTarGzArchive,
  inspectZipArchive,
  isFetchableUrl,
  parseSha256Sums,
  setupLocalRuntime,
} from "../src/setup.ts"

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

/** Build a minimal valid USTAR `.tar.gz` with arbitrary entry type, path, linkname, and declared size. */
function makeRawTarGz(
  entries: ReadonlyArray<{
    readonly name: string
    readonly typeflag: string
    readonly content?: Uint8Array
    readonly declaredSize?: number
    readonly linkname?: string
  }>,
): Uint8Array {
  const encoder = new TextEncoder()
  const blocks: Uint8Array[] = []
  const writeAscii = (target: Uint8Array, offset: number, text: string, maxLen: number) => {
    const bytes = encoder.encode(text)
    target.set(bytes.subarray(0, maxLen), offset)
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
    header.fill(0x20, 148, 156)
    header[156] = entry.typeflag.charCodeAt(0)
    if (entry.linkname) writeAscii(header, 157, entry.linkname, 100)
    writeAscii(header, 257, "ustar\0", 6)
    writeAscii(header, 263, "00", 2)

    let sum = 0
    for (let i = 0; i < 512; i++) sum += header[i]!
    writeAscii(header, 148, `${sum.toString(8).padStart(6, "0")}\0 `, 8)
    blocks.push(header)

    if (content.length > 0) {
      const paddedLen = Math.ceil(content.length / 512) * 512
      const data = new Uint8Array(paddedLen)
      data.set(content, 0)
      blocks.push(data)
    }
  }
  blocks.push(new Uint8Array(1024))

  const total = blocks.reduce((acc, b) => acc + b.length, 0)
  const tar = new Uint8Array(total)
  let offset = 0
  for (const block of blocks) {
    tar.set(block, offset)
    offset += block.length
  }
  return gzipSync(tar)
}

/** Build a minimal stored or deflated ZIP archive for pre-extraction inspection & extraction tests. */
function makeRawZip(
  entries: ReadonlyArray<{
    readonly name: string
    readonly content?: Uint8Array
    readonly deflate?: boolean
    readonly declaredUncompressedSize?: number
    readonly unixMode?: number
    readonly hostOs?: number
  }>,
): Uint8Array {
  const encoder = new TextEncoder()
  const localParts: Uint8Array[] = []
  const cdParts: Uint8Array[] = []
  let localOffset = 0

  for (const entry of entries) {
    const nameBytes = encoder.encode(entry.name)
    const rawContent = entry.content ?? new Uint8Array(0)
    const payload = entry.deflate ? deflateRawSync(rawContent) : rawContent
    const method = entry.deflate ? 8 : 0
    const uncompressedSize = entry.declaredUncompressedSize ?? rawContent.length
    const checksum = crc32(rawContent)
    const hostOs = entry.hostOs ?? 3

    const lfh = new Uint8Array(30 + nameBytes.length)
    const lfhView = new DataView(lfh.buffer)
    lfhView.setUint32(0, 0x04034b50, true)
    lfhView.setUint16(4, 20, true)
    lfhView.setUint16(8, method, true)
    lfhView.setUint32(14, checksum, true)
    lfhView.setUint32(18, payload.length, true)
    lfhView.setUint32(22, uncompressedSize, true)
    lfhView.setUint16(26, nameBytes.length, true)
    lfh.set(nameBytes, 30)

    const cdh = new Uint8Array(46 + nameBytes.length)
    const cdhView = new DataView(cdh.buffer)
    cdhView.setUint32(0, 0x02014b50, true)
    cdhView.setUint16(4, (hostOs << 8) | 20, true)
    cdhView.setUint16(6, 20, true)
    cdhView.setUint16(10, method, true)
    cdhView.setUint32(16, checksum, true)
    cdhView.setUint32(20, payload.length, true)
    cdhView.setUint32(24, uncompressedSize, true)
    cdhView.setUint16(28, nameBytes.length, true)
    cdhView.setUint32(38, ((entry.unixMode ?? 0o100644) << 16) >>> 0, true)
    cdhView.setUint32(42, localOffset, true)
    cdh.set(nameBytes, 46)

    localParts.push(lfh, payload)
    cdParts.push(cdh)
    localOffset += lfh.length + payload.length
  }

  const cdSize = cdParts.reduce((acc, b) => acc + b.length, 0)
  const eocd = new Uint8Array(22)
  const eocdView = new DataView(eocd.buffer)
  eocdView.setUint32(0, 0x06054b50, true)
  eocdView.setUint16(8, entries.length, true)
  eocdView.setUint16(10, entries.length, true)
  eocdView.setUint32(12, cdSize, true)
  eocdView.setUint32(16, localOffset, true)

  const all = [...localParts, ...cdParts, eocd]
  const out = new Uint8Array(all.reduce((acc, b) => acc + b.length, 0))
  let offset = 0
  for (const part of all) {
    out.set(part, offset)
    offset += part.length
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
  const wasmArchive = makeTarGz({
    "norn_checker_wasm.js":
      'export function initSync() {}\n'
      + 'export function checkArtifactSet() { return JSON.stringify({ format: "norn-check/1", admitted: true, diagnostics: { diagnostics: [] }, receipt: null, receiptSha256: null }) }\n'
      + `export function version() { return JSON.stringify({ abi: "norn-checker-abi/1", fixture: ${JSON.stringify(fixtureTag)} }) }\n`,
    "norn_checker_wasm_bg.wasm": new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]),
    "package.json": '{"name":"@norn/checker-wasm","version":"0.1.0"}\n',
    LICENSE: "Apache-2.0\n",
    NOTICE: "Norn\n",
  })
  const devHostArchive = makeTarGz({
    "norn-dev-host": `#!/bin/sh\necho ${fixtureTag}-dev-host\n`,
    LICENSE: "Apache-2.0\n",
    NOTICE: "Norn\n",
  })
  const cliArchive = makeTarGz({
    norn: `#!/bin/sh\necho ${fixtureTag}-norn-cli\n`,
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
    ...(overrides.replaceAsset ?? {}),
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

async function expectHostRefusal(run: Promise<unknown>): Promise<ProjectError> {
  try {
    await run
  } catch (error) {
    if (error instanceof ProjectError) return error
    throw error
  }
  throw new Error("expected ProjectError refusal")
}

test("norn setup downloads, verifies SHA-256, publishes atomic generation, and checker-abi + dev auto-discover it", async () => {
  const root = scratch()
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

    // Verify generation-based layout under $XDG_CACHE_HOME/norn/runtime/
    const cacheRoot = join(xdgCache, "norn")
    const genId = readActiveGenerationId(cacheRoot)
    expect(genId).toBeDefined()
    const expectedWasmDir = cachedCheckerWasmDir({ XDG_CACHE_HOME: xdgCache })!
    const expectedDevHost = cachedDevHostPath(cacheRoot)
    const expectedCli = cachedCliPath(cacheRoot)
    expect(expectedWasmDir).toBe(join(cacheRoot, "runtime", "generations", genId!, "checker-wasm"))
    expect(existsSync(join(expectedWasmDir, "norn_checker_wasm.js"))).toBe(true)
    expect(existsSync(join(expectedWasmDir, "norn_checker_wasm_bg.wasm"))).toBe(true)
    expect(existsSync(expectedDevHost)).toBe(true)
    expect(statSync(expectedDevHost).mode & 0o111).toBeGreaterThan(0)
    expect(existsSync(expectedCli)).toBe(true)

    // checker-abi.ts auto-discovers cached runtime when bundled & override are absent.
    expect(checkerPackageDir({ XDG_CACHE_HOME: xdgCache }, join(root, "no-bundled"))).toBe(expectedWasmDir)
    expect(checkerPackageDir({ XDG_CACHE_HOME: xdgCache, [CHECKER_PKG_ENV]: "/custom/override" }, join(root, "no-bundled"))).toBe("/custom/override")
    const checker = await loadChecker(checkerPackageDir({ XDG_CACHE_HOME: xdgCache }, join(root, "no-bundled")))
    expect(checker).toBeDefined()
    expect(checker!.version()).toEqual({ abi: "norn-checker-abi/1", fixture: "cached-runtime" })

    // dev.ts auto-discovers cached norn-dev-host in a checkout when NORN_DEV_HOST_BIN is unset.
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

test("redirects reject userinfo, reject HTTPS->HTTP downgrades (even loopback), and re-validate every hop", async () => {
  expect(isFetchableUrl("https://github.com/Norn-cloud/norn/releases/latest/download/SHA256SUMS")).toBe(true)
  expect(isFetchableUrl("http://127.0.0.1:8080/SHA256SUMS")).toBe(true)
  expect(isFetchableUrl("http://localhost:8080/SHA256SUMS")).toBe(true)
  expect(isFetchableUrl("http://[::1]:8080/SHA256SUMS")).toBe(true)
  expect(isFetchableUrl("http://example.invalid/SHA256SUMS")).toBe(false)
  expect(isFetchableUrl("http://127.0.0.1@example.invalid/SHA256SUMS")).toBe(false)
  expect(isFetchableUrl("https://user:pass@github.com/Norn-cloud/norn/releases/latest/download/SHA256SUMS")).toBe(false)
  expect(isFetchableUrl("https://token@github.com/Norn-cloud/norn/releases/latest/download/SHA256SUMS")).toBe(false)
  expect(isFetchableUrl("http://user:pass@127.0.0.1:8080/SHA256SUMS")).toBe(false)
  expect(isFetchableUrl("file:///etc/passwd")).toBe(false)

  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    fetch(request) {
      const url = new URL(request.url)
      const path = url.pathname
      if (path === "/downgrade") {
        return new Response(null, { status: 302, headers: { location: "http://example.invalid/plaintext.tar.gz" } })
      }
      if (path === "/userinfo-redirect") {
        return new Response(null, { status: 302, headers: { location: `http://user:pass@127.0.0.1:${url.port}/payload` } })
      }
      if (path === "/relative-hop") {
        return new Response(null, { status: 302, headers: { location: "/absolute-hop" } })
      }
      if (path === "/absolute-hop") {
        return new Response(null, { status: 307, headers: { location: `${url.origin}/payload` } })
      }
      if (path === "/payload") {
        return new Response("verified-payload")
      }
      if (path.startsWith("/loop/")) {
        const n = Number(path.slice("/loop/".length))
        return new Response(null, { status: 302, headers: { location: `/loop/${n + 1}` } })
      }
      return new Response("not found", { status: 404 })
    },
  })

  try {
    const base = `http://127.0.0.1:${server.port}`
    // 1. Initial URL with userinfo is refused.
    await expect(fetchBounded(`http://user:pass@127.0.0.1:${server.port}/payload`, 1024, 5_000)).rejects.toThrow("refusing")
    await expect(fetchBounded("https://user:pass@github.com/a.tar.gz", 1024, 5_000)).rejects.toThrow("refusing")

    // 2. Redirect to remote plaintext HTTP or URL with userinfo is refused fail-closed.
    await expect(fetchBounded(`${base}/downgrade`, 1024, 5_000)).rejects.toThrow("refusing redirect")
    await expect(fetchBounded(`${base}/userinfo-redirect`, 1024, 5_000)).rejects.toThrow("refusing redirect")

    // 3. Allowed relative + loopback redirects succeed.
    const payload = await fetchBounded(`${base}/relative-hop`, 1024, 5_000)
    expect(new TextDecoder().decode(payload)).toBe("verified-payload")

    // 4. Excessive redirect chains fail closed.
    await expect(fetchBounded(`${base}/loop/0`, 1024, 5_000)).rejects.toThrow("too many redirects")

    // 5. Mock fetch: HTTPS -> HTTPS CDN succeeds.
    const seenAllowed: string[] = []
    const okBytes = await fetchBounded(
      "https://github.com/Norn-cloud/norn/releases/download/drop-1/a.tar.gz",
      1024,
      5_000,
      async (url, init) => {
        expect(init?.redirect).toBe("manual")
        seenAllowed.push(url)
        if (url.startsWith("https://github.com/")) {
          return new Response(null, {
            status: 302,
            headers: { location: "https://objects.githubusercontent.com/release/a.tar.gz" },
          })
        }
        return new Response("cdn-bytes")
      },
    )
    expect(new TextDecoder().decode(okBytes)).toBe("cdn-bytes")
    expect(seenAllowed).toEqual([
      "https://github.com/Norn-cloud/norn/releases/download/drop-1/a.tar.gz",
      "https://objects.githubusercontent.com/release/a.tar.gz",
    ])

    // 6. Mock fetch: HTTPS -> remote HTTP AND HTTPS -> loopback HTTP AND HTTPS -> userinfo HTTPS are ALL refused before issuing the hop.
    for (const badLocation of [
      "http://objects.githubusercontent.com/insecure/a.tar.gz",
      "http://127.0.0.1:8080/ssrf-downgrade.tar.gz",
      "http://localhost:8080/ssrf-downgrade.tar.gz",
      "http://[::1]:8080/ssrf-downgrade.tar.gz",
      "https://user:pass@objects.githubusercontent.com/release/a.tar.gz",
    ]) {
      const seen: string[] = []
      await expect(
        fetchBounded(
          "https://github.com/Norn-cloud/norn/releases/download/drop-1/a.tar.gz",
          1024,
          5_000,
          async (url) => {
            seen.push(url)
            return new Response(null, { status: 302, headers: { location: badLocation } })
          },
        ),
      ).rejects.toThrow("refusing redirect")
      expect(seen).toEqual(["https://github.com/Norn-cloud/norn/releases/download/drop-1/a.tar.gz"])
    }
  } finally {
    server.stop(true)
  }
})

test("unsafe tar.gz and zip archives (including non-Unix ZIP symlinks & lying ZIP bombs) are refused BEFORE extraction", () => {
  const root = scratch()
  try {
    const cases: ReadonlyArray<{
      readonly label: string
      readonly archive: Uint8Array
      readonly pattern: string
      readonly maxExtractedBytes?: number
    }> = [
      {
        label: "tar-symlink.tar.gz",
        archive: makeRawTarGz([{ name: "evil-link", typeflag: "2", linkname: "/etc/passwd" }]),
        pattern: "refusing symlink",
      },
      {
        label: "tar-hardlink.tar.gz",
        archive: makeRawTarGz([
          { name: "legit.txt", typeflag: "0", content: new TextEncoder().encode("ok") },
          { name: "evil-hardlink", typeflag: "1", linkname: "legit.txt" },
        ]),
        pattern: "refusing hard link",
      },
      {
        label: "tar-traversal.tar.gz",
        archive: makeRawTarGz([
          { name: "../escape.txt", typeflag: "0", content: new TextEncoder().encode("pwn") },
        ]),
        pattern: "refusing unsafe archive entry",
      },
      {
        label: "tar-nested-traversal.tar.gz",
        archive: makeRawTarGz([
          { name: "sub/../../escape.txt", typeflag: "0", content: new TextEncoder().encode("pwn") },
        ]),
        pattern: "refusing unsafe archive entry",
      },
      {
        label: "tar-absolute.tar.gz",
        archive: makeRawTarGz([
          { name: "/etc/evil", typeflag: "0", content: new TextEncoder().encode("pwn") },
        ]),
        pattern: "refusing unsafe archive entry",
      },
      {
        label: "tar-fifo.tar.gz",
        archive: makeRawTarGz([{ name: "pipe", typeflag: "6" }]),
        pattern: "refusing FIFO",
      },
      {
        label: "tar-oversized-header.tar.gz",
        archive: makeRawTarGz([
          { name: "huge.bin", typeflag: "0", declaredSize: 300 * 1024 * 1024 },
        ]),
        pattern: "exceeds maximum uncompressed size",
      },
      {
        label: "tar-oversized-content.tar.gz",
        archive: makeTarGz({ "big.bin": new Uint8Array(4096) }),
        pattern: "exceeds maximum uncompressed size",
        maxExtractedBytes: 1024,
      },
      {
        label: "zip-unix-symlink.zip",
        archive: makeRawZip([{ name: "link", content: new TextEncoder().encode("/etc/passwd"), unixMode: 0o120777, hostOs: 3 }]),
        pattern: "refusing symlink",
      },
      {
        // Non-Unix origin (hostOs = 0 DOS / 19 macOS) carrying Unix symlink mode bits must STILL be refused.
        label: "zip-non-unix-origin-symlink.zip",
        archive: makeRawZip([{ name: "sneaky-link", content: new TextEncoder().encode("/etc/passwd"), unixMode: 0o120777, hostOs: 0 }]),
        pattern: "refusing symlink",
      },
      {
        label: "zip-non-unix-origin-fifo.zip",
        archive: makeRawZip([{ name: "sneaky-fifo", content: new Uint8Array(0), unixMode: 0o010644, hostOs: 19 }]),
        pattern: "refusing special file",
      },
      {
        label: "zip-traversal.zip",
        archive: makeRawZip([{ name: "../escape.txt", content: new TextEncoder().encode("x") }]),
        pattern: "refusing unsafe archive entry",
      },
      {
        label: "zip-oversized-declared.zip",
        archive: makeRawZip([{ name: "declared-bomb.bin", declaredUncompressedSize: 300 * 1024 * 1024 }]),
        pattern: "exceeds maximum uncompressed size",
      },
      {
        // Lying ZIP bomb: central directory claims 16 bytes, but the deflate stream actually expands to 64 KiB!
        label: "zip-lying-deflate-bomb.zip",
        archive: makeRawZip([
          {
            name: "bomb.bin",
            content: new Uint8Array(64 * 1024),
            deflate: true,
            declaredUncompressedSize: 16,
          },
        ]),
        pattern: "exceeds maximum uncompressed size",
        maxExtractedBytes: 1024,
      },
      {
        // Lying ZIP size within budget: expands to 256 bytes while declaring 16 bytes -> refused on size mismatch!
        label: "zip-lying-size-mismatch.zip",
        archive: makeRawZip([
          {
            name: "lie.bin",
            content: new Uint8Array(256),
            deflate: true,
            declaredUncompressedSize: 16,
          },
        ]),
        pattern: "does not match declared size",
        maxExtractedBytes: 4096,
      },
    ]

    for (const c of cases) {
      const archivePath = join(root, c.label)
      const destDir = join(root, `out-${c.label}`)
      writeFileSync(archivePath, c.archive)
      expect(() =>
        extractArchive(
          archivePath,
          destDir,
          c.label,
          c.maxExtractedBytes !== undefined ? { maxExtractedBytes: c.maxExtractedBytes } : {},
        ),
      ).toThrow(c.pattern)
      // Must fail BEFORE creating destDir or extracting any member!
      expect(existsSync(destDir)).toBe(false)
    }

    // Valid tar.gz and deflated zip pass preflight inspection and extract cleanly without external `unzip`.
    const validTar = join(root, "valid.tar.gz")
    writeFileSync(validTar, makeTarGz({ "hello.txt": "world" }))
    expect(inspectTarGzArchive(validTar, "valid.tar.gz")).toEqual({ entries: 1, totalBytes: 5 })

    const validZip = join(root, "valid.zip")
    const validZipDest = join(root, "out-valid-zip")
    writeFileSync(
      validZip,
      makeRawZip([
        { name: "bin/", unixMode: 0o040755 },
        { name: "bin/tool", content: new TextEncoder().encode("#!/bin/sh\necho ok\n"), deflate: true, unixMode: 0o100755 },
      ]),
    )
    expect(inspectZipArchive(validZip, "valid.zip")).toEqual({ entries: 2, totalBytes: 18 })
    extractArchive(validZip, validZipDest, "valid.zip")
    expect(readFileSync(join(validZipDest, "bin/tool"), "utf8")).toBe("#!/bin/sh\necho ok\n")
    expect(statSync(join(validZipDest, "bin/tool")).mode & 0o111).toBeGreaterThan(0)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("setupLocalRuntime preflight-inspects ALL archives BEFORE extracting ANY archive onto disk", async () => {
  const root = scratch()
  const platform = detectPlatform()
  // First two archives (wasm & dev-host) are completely valid, while the third (cli) contains a symlink.
  const badThird = startReleaseServer(platform, {
    replaceAsset: {
      [`norn-${platform}.tar.gz`]: makeRawTarGz([
        { name: "norn", typeflag: "2", linkname: "/bin/sh" },
      ]),
    },
  })
  try {
    const extracted: string[] = []
    await expect(
      setupLocalRuntime({
        baseUrl: badThird.baseUrl,
        cacheDir: join(root, "cache"),
        platform,
        extractImpl(archivePath, destDir, assetName, limits) {
          extracted.push(assetName)
          extractArchive(archivePath, destDir, assetName, limits)
        },
      }),
    ).rejects.toThrow("refusing symlink")
    // Zero archives must have been extracted because preflight runs across ALL archives first.
    expect(extracted).toEqual([])
    expect(existsSync(join(root, "cache"))).toBe(false)
  } finally {
    badThird.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test("checkerPackageDir strictly preserves override -> bundled -> local cache precedence with all three installed", () => {
  const root = scratch()
  try {
    const overrideDir = join(root, "override-checker")
    const bundledDir = join(root, "bundled-checker")
    const cacheDir = join(root, "cache")
    const genDir = join(cacheDir, "runtime", "generations", "gen-1")
    const cachedWasmDir = join(genDir, "checker-wasm")

    // Actually install valid checker files in ALL THREE directories simultaneously.
    for (const dir of [overrideDir, bundledDir, cachedWasmDir]) {
      mkdirSync(dir, { recursive: true })
      writeFileSync(join(dir, "norn_checker_wasm.js"), "export function initSync() {}\n")
      writeFileSync(join(dir, "norn_checker_wasm_bg.wasm"), new Uint8Array([0x00, 0x61, 0x73, 0x6d]))
    }
    writeFileSync(join(cacheDir, "runtime", "current"), "gen-1\n")

    // 1. All three genuinely exist: explicit override beats bundled and cache.
    expect(
      checkerPackageDir({ [CHECKER_PKG_ENV]: overrideDir, NORN_CACHE_DIR: cacheDir }, bundledDir),
    ).toBe(overrideDir)
    // Explicit override also wins fail-closed even when naming a nonexistent path.
    expect(
      checkerPackageDir({ [CHECKER_PKG_ENV]: join(root, "missing-override"), NORN_CACHE_DIR: cacheDir }, bundledDir),
    ).toBe(join(root, "missing-override"))

    // 2. Bundled + cache present (no override): bundled checker MUST win over local cache.
    expect(
      checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, bundledDir),
    ).toBe(bundledDir)
    expect(
      checkerPackageDir({ [CHECKER_PKG_ENV]: "", NORN_CACHE_DIR: cacheDir }, bundledDir),
    ).toBe(bundledDir)

    // 3. Bundled absent, cache present: local cache is used.
    expect(
      checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled")),
    ).toBe(cachedWasmDir)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("atomic generation replacement preserves active generation on failure and performs concurrent-safe cleanup", async () => {
  const root = scratch()
  const platform = detectPlatform()
  const releaseV1 = startReleaseServer(platform, { fixtureTag: "v1" })
  const releaseV2BadArchive = startReleaseServer(platform, {
    fixtureTag: "v2-bad",
    replaceAsset: {
      [`norn-dev-host-${platform}.tar.gz`]: makeRawTarGz([
        { name: "norn-dev-host", typeflag: "2", linkname: "/bin/sh" },
      ]),
    },
  })
  const releaseV2Good = startReleaseServer(platform, { fixtureTag: "v2" })
  const cacheDir = join(root, "cache")
  const runtimeDir = join(cacheDir, "runtime")
  const generationsDir = join(runtimeDir, "generations")
  try {
    // 1. Install generation 1.
    const first = await setupLocalRuntime({ baseUrl: releaseV1.baseUrl, cacheDir, platform, version: "drop-1" })
    const gen1 = readActiveGenerationId(cacheDir)
    expect(gen1).toBe(first.generationId)

    const checkerV1 = await loadChecker(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled")))
    expect(checkerV1!.version()).toEqual({ abi: "norn-checker-abi/1", fixture: "v1" })

    // 2. Failed replacement (unsafe archive): generation 1 stays active and intact.
    await expect(
      setupLocalRuntime({ baseUrl: releaseV2BadArchive.baseUrl, cacheDir, platform, version: "drop-2-bad" }),
    ).rejects.toThrow("refusing symlink")
    expect(readActiveGenerationId(cacheDir)).toBe(gen1)
    expect(existsSync(first.devHostBin)).toBe(true)

    // 3. Simulate failure right before atomic pointer swap:
    //    Make runtime/ read-only while runtime/generations/ stays writable so staging
    //    succeeds into generations/<gen> and writing .current.*.tmp fails.
    chmodSync(runtimeDir, 0o555)
    try {
      await expect(
        setupLocalRuntime({ baseUrl: releaseV2Good.baseUrl, cacheDir, platform, version: "drop-2-interrupted" }),
      ).rejects.toThrow()
    } finally {
      chmodSync(runtimeDir, 0o755)
    }
    expect(readActiveGenerationId(cacheDir)).toBe(gen1)
    expect(readdirSync(generationsDir)).toEqual([gen1!])
    const checkerAfterFail = await loadChecker(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled")))
    expect(checkerAfterFail!.version()).toEqual({ abi: "norn-checker-abi/1", fixture: "v1" })
    const hostAfterFail = await hostBinary({
      ...hostLookup({ NORN_CACHE_DIR: cacheDir }),
      build: undefined,
      packageDir: () => undefined,
    })
    expect(hostAfterFail).toBe(first.devHostBin)

    // 4. Stray leftover mid-staging directory does not disrupt active generation discovery.
    mkdirSync(join(generationsDir, ".staging.interrupted-999"), { recursive: true })
    writeFileSync(join(runtimeDir, ".current.999.tmp"), "interrupted-999\n")
    expect(readActiveGenerationId(cacheDir)).toBe(gen1)

    // 5. Seed an old stale generation (> 60s old) alongside fresh gen1, then install gen2:
    //    - fresh gen1 (< 60s old and one of the 2 most recent) MUST be preserved for concurrent readers;
    //    - aged stale generation beyond the 2 most recent IS pruned.
    const staleGen = join(generationsDir, "0000-stale-old")
    mkdirSync(staleGen, { recursive: true })
    const tenMinutesAgo = new Date(Date.now() - 10 * 60_000)
    utimesSync(staleGen, tenMinutesAgo, tenMinutesAgo)

    const second = await setupLocalRuntime({ baseUrl: releaseV2Good.baseUrl, cacheDir, platform, version: "drop-2" })
    expect(second.generationId).not.toBe(gen1)
    expect(readActiveGenerationId(cacheDir)).toBe(second.generationId)
    expect(existsSync(join(generationsDir, gen1!))).toBe(true) // recent generation preserved
    expect(existsSync(staleGen)).toBe(false) // stale generation pruned
    const checkerV2 = await loadChecker(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled")))
    expect(checkerV2!.version()).toEqual({ abi: "norn-checker-abi/1", fixture: "v2" })
  } finally {
    if (existsSync(runtimeDir)) chmodSync(runtimeDir, 0o755)
    releaseV1.close()
    releaseV2BadArchive.close()
    releaseV2Good.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test("legacy direct runtime cache layout remains discoverable and upgrades cleanly", async () => {
  const root = scratch()
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

    // Legacy direct layout without `current` (and without manifest.json) is discovered.
    expect(readActiveGenerationId(cacheDir)).toBeUndefined()
    expect(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled"))).toBe(legacyWasm)
    expect(
      await hostBinary({ ...hostLookup({ NORN_CACHE_DIR: cacheDir }), build: undefined, packageDir: () => undefined }),
    ).toBe(legacyHost)

    // Running setup transitions cleanly to generations + `current` and removes legacy top-level dirs.
    const upgraded = await setupLocalRuntime({ baseUrl: release.baseUrl, cacheDir, platform, version: "drop-9" })
    expect(readActiveGenerationId(cacheDir)).toBe(upgraded.generationId)
    expect(existsSync(legacyWasm)).toBe(false)
    expect(existsSync(legacyBinDir)).toBe(false)
    expect(checkerPackageDir({ NORN_CACHE_DIR: cacheDir }, join(root, "no-bundled"))).toBe(upgraded.checkerWasmDir)
  } finally {
    release.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test("dev.ts validates cached norn-dev-host file type, executable mode, generation manifest requirement, platform, and version", async () => {
  const root = scratch()
  const releasePlatform = detectPlatform()
  const nodePlatform = `${process.platform}-${process.arch}`
  const wrongPlatform = releasePlatform === "linux-x64" ? "macos-arm64" : "linux-x64"

  const makeReleaseBuild = (version: string): BuildInfo => ({
    version,
    commit: "0123456789abcdef0123456789abcdef01234567",
    toolchain: "1.100.0",
    packages: { "@norn/cli": version, "@norn/authoring": version },
    hosts: {},
  })

  try {
    // 1. Directory masquerading as norn-dev-host -> cli.setup.host_unavailable
    const dirCache = join(root, "dir-cache")
    mkdirSync(join(dirCache, "runtime", "bin", "norn-dev-host"), { recursive: true })
    const dirErr = await expectHostRefusal(
      hostBinary({ ...hostLookup({ NORN_CACHE_DIR: dirCache }), build: undefined, packageDir: () => undefined }),
    )
    expect(dirErr).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(dirErr.message).toContain("not a regular file")

    // 2. Non-executable regular file on POSIX -> cli.setup.host_unavailable
    const nonExecCache = join(root, "non-exec-cache")
    mkdirSync(join(nonExecCache, "runtime", "bin"), { recursive: true })
    const nonExecBin = join(nonExecCache, "runtime", "bin", "norn-dev-host")
    writeFileSync(nonExecBin, "#!/bin/sh\necho hi\n")
    chmodSync(nonExecBin, 0o644)
    const nonExecErr = await expectHostRefusal(
      hostBinary({ ...hostLookup({ NORN_CACHE_DIR: nonExecCache }), build: undefined, packageDir: () => undefined }),
    )
    expect(nonExecErr).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(nonExecErr.message).toContain("not executable")

    // 3. Generation-based cache MISSING manifest.json -> refused via cli.setup.host_unavailable!
    const missingManifestCache = join(root, "gen-missing-manifest")
    const missingManifestGen = join(missingManifestCache, "runtime", "generations", "gen-no-manifest")
    mkdirSync(join(missingManifestGen, "bin"), { recursive: true })
    const genHostBin = join(missingManifestGen, "bin", "norn-dev-host")
    writeFileSync(genHostBin, "#!/bin/sh\necho hi\n")
    chmodSync(genHostBin, 0o755)
    writeFileSync(join(missingManifestCache, "runtime", "current"), "gen-no-manifest\n")
    const missingManifestErr = await expectHostRefusal(
      hostBinary({
        ...hostLookup({ NORN_CACHE_DIR: missingManifestCache }),
        platform: nodePlatform,
        build: undefined,
        packageDir: () => undefined,
      }),
    )
    expect(missingManifestErr).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(missingManifestErr.message).toContain("missing manifest.json")

    // 4. Platform mismatch in manifest.json -> cli.setup.host_unavailable
    writeFileSync(
      join(missingManifestGen, "manifest.json"),
      JSON.stringify({ format: RUNTIME_MANIFEST_FORMAT, version: "drop-28", platform: wrongPlatform }),
    )
    const platErr = await expectHostRefusal(
      hostBinary({
        ...hostLookup({ NORN_CACHE_DIR: missingManifestCache }),
        platform: nodePlatform,
        build: undefined,
        packageDir: () => undefined,
      }),
    )
    expect(platErr).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(platErr.message).toContain(`for platform ${wrongPlatform}`)

    // 5. Version mismatch against a release build -> cli.setup.host_unavailable
    writeFileSync(
      join(missingManifestGen, "manifest.json"),
      JSON.stringify({ format: RUNTIME_MANIFEST_FORMAT, version: "drop-1", platform: releasePlatform }),
    )
    const verErr = await expectHostRefusal(
      hostBinary({
        ...hostLookup({ NORN_CACHE_DIR: missingManifestCache }),
        platform: nodePlatform,
        build: makeReleaseBuild("0.0.28"),
        packageDir: () => undefined,
      }),
    )
    expect(verErr).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(verErr.message).toContain("for version drop-1, not 0.0.28")

    // 6. Matching drop-28 <-> 0.0.28 release version succeeds!
    writeFileSync(
      join(missingManifestGen, "manifest.json"),
      JSON.stringify({ format: RUNTIME_MANIFEST_FORMAT, version: "drop-28", platform: releasePlatform }),
    )
    expect(
      await hostBinary({
        ...hostLookup({ NORN_CACHE_DIR: missingManifestCache }),
        platform: nodePlatform,
        build: makeReleaseBuild("0.0.28"),
        packageDir: () => undefined,
      }),
    ).toBe(genHostBin)

    // 7. Corrupt manifest.json -> cli.setup.host_unavailable
    writeFileSync(join(missingManifestGen, "manifest.json"), "{not-json")
    const badManifestErr = await expectHostRefusal(
      hostBinary({
        ...hostLookup({ NORN_CACHE_DIR: missingManifestCache }),
        platform: nodePlatform,
        build: undefined,
        packageDir: () => undefined,
      }),
    )
    expect(badManifestErr).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(badManifestErr.message).toContain("invalid JSON")

    // 8. Directory at NORN_DEV_HOST_BIN -> cli.setup.host_unavailable
    const dirOverride = join(root, "override-dir")
    mkdirSync(dirOverride)
    const overrideErr = await expectHostRefusal(
      hostBinary({
        ...hostLookup({ NORN_DEV_HOST_BIN: dirOverride }),
        build: undefined,
        packageDir: () => undefined,
      }),
    )
    expect(overrideErr).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(overrideErr.message).toContain("not a regular file")
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("tampered archive or missing checksum fails closed without creating or modifying the cache", async () => {
  const root = scratch()
  const platform = detectPlatform()
  const tampered = startReleaseServer(platform, { corruptAsset: `norn-dev-host-${platform}.tar.gz` })
  const missingEntry = startReleaseServer(platform, { omitSumFor: "norn-checker-wasm.tar.gz" })
  try {
    const freshCache = join(root, "fresh-cache")
    await expect(
      setupLocalRuntime({ baseUrl: tampered.baseUrl, cacheDir: freshCache, platform }),
    ).rejects.toThrow("checksum mismatch")
    expect(existsSync(freshCache)).toBe(false)

    const existingCache = join(root, "existing-cache")
    const sentinel = join(existingCache, "runtime", "sentinel.txt")
    mkdirSync(join(existingCache, "runtime"), { recursive: true })
    writeFileSync(sentinel, "untouched")
    await expect(
      setupLocalRuntime({ baseUrl: tampered.baseUrl, cacheDir: existingCache, platform }),
    ).rejects.toThrow("checksum mismatch")
    expect(readFileSync(sentinel, "utf8")).toBe("untouched")

    const missingCache = join(root, "missing-cache")
    await expect(
      setupLocalRuntime({ baseUrl: missingEntry.baseUrl, cacheDir: missingCache, platform }),
    ).rejects.toThrow("no checksum entry for norn-checker-wasm.tar.gz")
    expect(existsSync(missingCache)).toBe(false)

    await expect(
      setupLocalRuntime({ baseUrl: "http://example.invalid/releases", cacheDir: missingCache, platform }),
    ).rejects.toThrow("refusing unsafe or non-https release URL")
  } finally {
    tampered.close()
    missingEntry.close()
    rmSync(root, { recursive: true, force: true })
  }
})

test("scripts/dev/fetch-local-runtime.sh wraps norn setup against a local mirror", async () => {
  const root = scratch()
  const platform = detectPlatform()
  const release = startReleaseServer(platform)
  try {
    const cacheDir = join(root, "wrapper-cache")
    const script = join(TOOL_ROOT, "../../scripts/dev/fetch-local-runtime.sh")
    const run = await runAsync(["bash", script, "--base-url", release.baseUrl, "--cache-dir", cacheDir])
    expect({ status: run.status, stderr: run.stderr }).toEqual({ status: 0, stderr: "" })
    const genId = readActiveGenerationId(cacheDir)
    expect(genId).toBeDefined()
    const wasmDir = cachedCheckerWasmDir({ NORN_CACHE_DIR: cacheDir })!
    expect(existsSync(join(wasmDir, "norn_checker_wasm.js"))).toBe(true)
    expect(existsSync(cachedDevHostPath(cacheDir))).toBe(true)
    expect(existsSync(cachedCliPath(cacheDir))).toBe(true)
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
