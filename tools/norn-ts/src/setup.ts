#!/usr/bin/env bun
/**
 * `norn setup`: download and verify prebuilt local runtime artifacts
 * (`norn-checker-wasm`, `norn-dev-host`, and `norn` CLI) from GitHub Releases
 * (or a `--base-url` mirror) into `~/.cache/norn/runtime/`.
 *
 * Fail-closed verification:
 * 1. Fetches `SHA256SUMS` and every archive with `redirect: "manual"`,
 *    re-validating every redirect hop against the HTTPS / loopback policy.
 * 2. Checks that every required archive has a single 64-hex SHA-256 entry and
 *    verifies EVERY archive's SHA-256 BEFORE inspecting or extracting anything.
 * 3. Inspects EVERY archive entry (type, path, linkname, uncompressed size,
 *    member count) BEFORE extracting onto disk, plus post-extraction
 *    verification as defense-in-depth.
 * 4. Publishes complete runtime generations into `runtime/generations/<genId>/`
 *    and switches via ONE atomic pointer update (`current.tmp` -> `current`),
 *    cleaning up old generations only AFTER the pointer swap succeeds.
 */
import { spawnSync } from "node:child_process"
import { createHash, randomBytes } from "node:crypto"
import {
  chmodSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs"
import { tmpdir } from "node:os"
import { join, posix } from "node:path"
import { gunzipSync } from "node:zlib"
import {
  RUNTIME_MANIFEST_FORMAT,
  defaultCacheRoot,
} from "./runtime-cache.ts"

export const DEFAULT_REPO = "Norn-cloud/norn"
export const SETUP_DOWNLOAD_TIMEOUT_MS = 600_000
export const MAX_REDIRECTS = 5
export const MAX_EXTRACTED_BYTES = 256 * 1024 * 1024
export const MAX_ARCHIVE_ENTRIES = 4096
const MAX_SUMS_BYTES = 1024 * 1024
const MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
const MAX_PAX_HEADER_BYTES = 1024 * 1024
const MAX_GNU_LONGNAME_BYTES = 64 * 1024

const REDIRECT_STATUSES = new Set([301, 302, 303, 307, 308])

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>

export interface ArchiveLimits {
  readonly maxExtractedBytes?: number
  readonly maxEntries?: number
}

export interface ArchiveSummary {
  readonly entries: number
  readonly totalBytes: number
}

export interface SetupOptions {
  readonly version?: string
  readonly baseUrl?: string
  readonly cacheDir?: string
  readonly repo?: string
  readonly platform?: string
  readonly env?: Readonly<Record<string, string | undefined>>
  readonly timeoutMs?: number
  readonly fetchImpl?: FetchLike
  readonly stdout?: (text: string) => void
  readonly stderr?: (text: string) => void
}

export interface SetupResult {
  readonly version: string
  readonly platform: string
  readonly baseUrl: string
  readonly generationId: string
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
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return false
  }
  if (parsed.protocol === "https:") return parsed.hostname.length > 0
  if (parsed.protocol === "http:") {
    return parsed.hostname === "127.0.0.1"
      || parsed.hostname === "localhost"
      || parsed.hostname === "[::1]"
      || parsed.hostname === "::1"
  }
  return false
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

/**
 * Fetch up to `limit` bytes from `url` within `timeoutMs`, following at most
 * `MAX_REDIRECTS` redirects manually and re-validating every redirect hop
 * against `isFetchableUrl` before issuing the next request.
 */
export async function fetchBounded(
  url: string,
  limit: number,
  timeoutMs: number,
  fetchImpl: FetchLike = fetch,
): Promise<Uint8Array> {
  if (!isFetchableUrl(url)) {
    throw new Error(`refusing non-https URL ${url} (plain http is allowed only on loopback)`)
  }
  const deadline = Date.now() + timeoutMs
  let currentUrl = url
  let redirects = 0
  let response: Response

  for (;;) {
    const remaining = deadline - Date.now()
    if (remaining <= 0) {
      throw new Error(`timed out fetching ${currentUrl}`)
    }
    response = await fetchImpl(currentUrl, {
      redirect: "manual",
      signal: AbortSignal.timeout(remaining),
    })
    if (!REDIRECT_STATUSES.has(response.status)) break

    await response.body?.cancel()
    if (redirects >= MAX_REDIRECTS) {
      throw new Error(`too many redirects (>${MAX_REDIRECTS}) fetching ${url}`)
    }
    const location = response.headers.get("location")
    if (!location) {
      throw new Error(`HTTP ${response.status} redirect without Location header fetching ${currentUrl}`)
    }
    let nextUrl: string
    try {
      nextUrl = new URL(location, currentUrl).toString()
    } catch {
      throw new Error(`invalid redirect Location '${location}' from ${currentUrl}`)
    }
    if (!isFetchableUrl(nextUrl)) {
      throw new Error(
        `refusing redirect from ${currentUrl} to non-https URL ${nextUrl} (plain http is allowed only on loopback)`,
      )
    }
    currentUrl = nextUrl
    redirects++
  }

  if (!response.ok || !response.body) {
    throw new Error(`HTTP ${response.status} fetching ${currentUrl}`)
  }
  const declared = Number(response.headers.get("content-length") ?? "NaN")
  if (Number.isFinite(declared) && declared > limit) {
    await response.body.cancel()
    throw new Error(`${currentUrl} exceeds size limit (${declared} > ${limit} bytes)`)
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
      throw new Error(`${currentUrl} exceeded size limit (${limit} bytes)`)
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

function assertSafeArchiveEntryPath(entryPath: string, isDirectory: boolean, assetName: string): void {
  if (!entryPath || entryPath.includes("\0")) {
    throw new Error(`refusing unsafe archive entry '${entryPath}' in ${assetName}`)
  }
  if (entryPath.startsWith("/") || entryPath.includes("\\") || /^[A-Za-z]:/.test(entryPath)) {
    throw new Error(`refusing unsafe archive entry '${entryPath}' in ${assetName}`)
  }
  const segments = entryPath.split("/")
  for (const segment of segments) {
    if (segment === "..") {
      throw new Error(`refusing unsafe archive entry '${entryPath}' in ${assetName}`)
    }
  }
  const normalized = posix.normalize(entryPath)
  if (normalized === ".." || normalized.startsWith("../") || normalized.startsWith("/")) {
    throw new Error(`refusing unsafe archive entry '${entryPath}' in ${assetName}`)
  }
  if (!isDirectory && (normalized === "." || entryPath.endsWith("/"))) {
    throw new Error(`refusing unsafe regular file entry '${entryPath}' in ${assetName}`)
  }
}

function readTarString(buf: Uint8Array, start: number, length: number): string {
  let end = start
  const max = start + length
  while (end < max && buf[end] !== 0) end++
  return new TextDecoder().decode(buf.subarray(start, end))
}

function parseTarOctal(buf: Uint8Array, start: number, length: number): number | undefined {
  const raw = readTarString(buf, start, length).trim()
  if (!raw) return 0
  if (!/^[0-7]+$/.test(raw)) return undefined
  const value = Number.parseInt(raw, 8)
  return Number.isSafeInteger(value) ? value : undefined
}

function parseTarSize(buf: Uint8Array, start: number, length: number): number | undefined {
  const first = buf[start]!
  if ((first & 0x80) !== 0) {
    // GNU base-256 big-endian integer; negative values have bit 0x40 set.
    if ((first & 0x40) !== 0) return undefined
    let value = BigInt(first & 0x7f)
    for (let i = 1; i < length; i++) {
      value = (value << 8n) | BigInt(buf[start + i]!)
    }
    if (value > BigInt(Number.MAX_SAFE_INTEGER)) return undefined
    return Number(value)
  }
  return parseTarOctal(buf, start, length)
}

function verifyTarChecksum(block: Uint8Array): boolean {
  const recorded = parseTarOctal(block, 148, 8)
  if (recorded === undefined) return false
  let unsignedSum = 0
  let signedSum = 0
  for (let i = 0; i < 512; i++) {
    const byte = i >= 148 && i < 156 ? 0x20 : block[i]!
    unsignedSum += byte
    signedSum += byte > 127 ? byte - 256 : byte
  }
  return recorded === unsignedSum || recorded === signedSum
}

function parsePaxRecords(data: Uint8Array, assetName: string): Record<string, string> {
  const records: Record<string, string> = {}
  const decoder = new TextDecoder()
  let offset = 0
  while (offset < data.length) {
    let space = offset
    while (space < data.length && data[space] !== 0x20) space++
    if (space === offset || space >= data.length) {
      throw new Error(`malformed PAX extended header in ${assetName}`)
    }
    const lenStr = decoder.decode(data.subarray(offset, space))
    if (!/^\d+$/.test(lenStr)) {
      throw new Error(`malformed PAX record length '${lenStr}' in ${assetName}`)
    }
    const recordLen = Number.parseInt(lenStr, 10)
    if (recordLen <= space - offset + 1 || offset + recordLen > data.length) {
      throw new Error(`truncated PAX extended header record in ${assetName}`)
    }
    if (data[offset + recordLen - 1] !== 0x0a) {
      throw new Error(`malformed PAX record terminator in ${assetName}`)
    }
    const payload = decoder.decode(data.subarray(space + 1, offset + recordLen - 1))
    const eq = payload.indexOf("=")
    if (eq <= 0) {
      throw new Error(`malformed PAX key/value record in ${assetName}`)
    }
    const key = payload.slice(0, eq)
    const value = payload.slice(eq + 1)
    if (key.startsWith("GNU.sparse.")) {
      throw new Error(`refusing sparse tar entry (${key}) in ${assetName}`)
    }
    if (key === "linkpath" && value.length > 0) {
      throw new Error(`refusing PAX linkpath '${value}' in ${assetName}`)
    }
    records[key] = value
    offset += recordLen
  }
  return records
}

/**
 * Inspect EVERY entry of a `.tar.gz` archive in memory BEFORE extraction:
 * - enforces gzip decompression output cap against gzip bombs;
 * - verifies tar header checksums and handles USTAR, PAX (`x`/`g`), and GNU `L`;
 * - allows ONLY regular files (`0`/`\0`) and directories (`5`);
 * - rejects symlinks (`2`), hard links (`1`), devices (`3`/`4`), FIFOs (`6`),
 *   sparse entries, non-empty linknames, absolute paths, and `..` traversal;
 * - enforces strict upper bounds on entry count and total uncompressed bytes.
 */
export function inspectTarGzArchive(
  archivePath: string,
  assetName: string,
  limits: ArchiveLimits = {},
): ArchiveSummary {
  const maxExtractedBytes = limits.maxExtractedBytes ?? MAX_EXTRACTED_BYTES
  const maxEntries = limits.maxEntries ?? MAX_ARCHIVE_ENTRIES
  // Allow 512-byte header/padding overhead on top of maxExtractedBytes while capping gzip bombs.
  const maxTarStreamBytes = Math.min(
    maxExtractedBytes + Math.max(64 * 1024, (maxEntries + 4) * 2048),
    MAX_EXTRACTED_BYTES + 64 * 1024 * 1024,
  )

  const compressed = readFileSync(archivePath)
  let tar: Uint8Array
  try {
    tar = gunzipSync(compressed, { maxOutputLength: maxTarStreamBytes })
  } catch (error) {
    const code = (error as NodeJS.ErrnoException | undefined)?.code
    if (code === "ERR_BUFFER_TOO_LARGE") {
      throw new Error(`archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
    }
    throw new Error(
      `failed to inspect archive ${assetName}: ${error instanceof Error ? error.message : String(error)}`,
    )
  }

  if (tar.length === 0) {
    throw new Error(`archive ${assetName} is empty`)
  }

  let offset = 0
  let consecutiveZeroBlocks = 0
  let entryCount = 0
  let totalBytes = 0
  const globalPax: Record<string, string> = {}
  let nextPax: Record<string, string> | undefined
  let nextGnuLongName: string | undefined

  while (offset + 512 <= tar.length) {
    const block = tar.subarray(offset, offset + 512)
    let allZero = true
    for (let i = 0; i < 512; i++) {
      if (block[i] !== 0) {
        allZero = false
        break
      }
    }
    if (allZero) {
      consecutiveZeroBlocks++
      offset += 512
      if (consecutiveZeroBlocks >= 2) break
      continue
    }
    consecutiveZeroBlocks = 0

    if (!verifyTarChecksum(block)) {
      throw new Error(`invalid tar header checksum in ${assetName}`)
    }

    const rawName = readTarString(block, 0, 100)
    const rawSize = parseTarSize(block, 124, 12)
    if (rawSize === undefined) {
      throw new Error(`invalid tar entry size for '${rawName}' in ${assetName}`)
    }
    const typeByte = block[156]!
    const typeChar = typeByte === 0 ? "0" : String.fromCharCode(typeByte)
    const rawLinkname = readTarString(block, 157, 100)
    const magic = readTarString(block, 257, 6)
    const rawPrefix = magic.startsWith("ustar") ? readTarString(block, 345, 155) : ""

    if (typeChar === "x" || typeChar === "g") {
      if (rawSize > MAX_PAX_HEADER_BYTES) {
        throw new Error(`oversized PAX header (${rawSize} bytes) in ${assetName}`)
      }
      const dataStart = offset + 512
      const dataEnd = dataStart + rawSize
      const paddedEnd = dataStart + Math.ceil(rawSize / 512) * 512
      if (paddedEnd > tar.length) {
        throw new Error(`truncated PAX header in ${assetName}`)
      }
      const parsed = parsePaxRecords(tar.subarray(dataStart, dataEnd), assetName)
      if (typeChar === "g") {
        Object.assign(globalPax, parsed)
      } else {
        nextPax = { ...(nextPax ?? {}), ...parsed }
      }
      offset = paddedEnd
      continue
    }

    if (typeChar === "L") {
      if (rawSize > MAX_GNU_LONGNAME_BYTES) {
        throw new Error(`oversized GNU longname (${rawSize} bytes) in ${assetName}`)
      }
      const dataStart = offset + 512
      const dataEnd = dataStart + rawSize
      const paddedEnd = dataStart + Math.ceil(rawSize / 512) * 512
      if (paddedEnd > tar.length) {
        throw new Error(`truncated GNU longname in ${assetName}`)
      }
      nextGnuLongName = new TextDecoder().decode(tar.subarray(dataStart, dataEnd)).replace(/\0+$/, "")
      offset = paddedEnd
      continue
    }

    if (typeChar === "K") {
      throw new Error(`refusing GNU longlink entry in ${assetName}`)
    }

    const pax = nextPax ? { ...globalPax, ...nextPax } : globalPax
    nextPax = undefined
    const gnuLong = nextGnuLongName
    nextGnuLongName = undefined

    const entryPath = pax.path ?? gnuLong ?? (rawPrefix ? `${rawPrefix}/${rawName}` : rawName)

    let entrySize = rawSize
    if (pax.size !== undefined) {
      if (!/^\d+$/.test(pax.size)) {
        throw new Error(`invalid PAX size '${pax.size}' for '${entryPath}' in ${assetName}`)
      }
      const parsedSize = Number(pax.size)
      if (!Number.isSafeInteger(parsedSize)) {
        throw new Error(`archive ${assetName} entry '${entryPath}' exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
      }
      entrySize = parsedSize
    }

    if (typeChar === "1") {
      throw new Error(`refusing hard link '${entryPath}' in ${assetName}`)
    }
    if (typeChar === "2") {
      throw new Error(`refusing symlink '${entryPath}' in ${assetName}`)
    }
    if (typeChar === "3") {
      throw new Error(`refusing character device '${entryPath}' in ${assetName}`)
    }
    if (typeChar === "4") {
      throw new Error(`refusing block device '${entryPath}' in ${assetName}`)
    }
    if (typeChar === "6") {
      throw new Error(`refusing FIFO '${entryPath}' in ${assetName}`)
    }
    if (typeChar !== "0" && typeChar !== "5") {
      throw new Error(`refusing unsupported archive entry type '${typeChar}' for '${entryPath}' in ${assetName}`)
    }

    if (rawLinkname.length > 0 || (pax.linkpath !== undefined && pax.linkpath.length > 0)) {
      throw new Error(`refusing link target on '${entryPath}' in ${assetName}`)
    }

    const isDir = typeChar === "5"
    assertSafeArchiveEntryPath(entryPath, isDir, assetName)

    entryCount++
    if (entryCount > maxEntries) {
      throw new Error(`archive ${assetName} exceeds maximum entry count (${maxEntries})`)
    }

    if (isDir && entrySize !== 0) {
      throw new Error(`refusing directory '${entryPath}' with non-zero size in ${assetName}`)
    }
    totalBytes += entrySize
    if (entrySize > maxExtractedBytes || totalBytes > maxExtractedBytes) {
      throw new Error(`archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
    }

    const paddedDataBytes = Math.ceil(entrySize / 512) * 512
    const nextOffset = offset + 512 + paddedDataBytes
    if (nextOffset > tar.length) {
      throw new Error(`truncated archive entry '${entryPath}' in ${assetName}`)
    }
    offset = nextOffset
  }

  if (entryCount === 0) {
    throw new Error(`archive ${assetName} contains no entries`)
  }
  return { entries: entryCount, totalBytes }
}

/**
 * Inspect EVERY entry of a `.zip` archive via its Central Directory and Local
 * File Headers BEFORE extraction:
 * - rejects symlinks and non-regular/non-directory Unix file modes;
 * - rejects absolute paths, backslashes, drive prefixes, and `..` traversal;
 * - rejects local-vs-central filename mismatches;
 * - enforces strict upper bounds on member count and total expanded bytes.
 */
export function inspectZipArchive(
  archivePath: string,
  assetName: string,
  limits: ArchiveLimits = {},
): ArchiveSummary {
  const maxExtractedBytes = limits.maxExtractedBytes ?? MAX_EXTRACTED_BYTES
  const maxEntries = limits.maxEntries ?? MAX_ARCHIVE_ENTRIES
  const buf = readFileSync(archivePath)
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)

  if (buf.length < 22) {
    throw new Error(`invalid zip archive ${assetName}: file too small`)
  }

  // Locate End of Central Directory (EOCD) record (signature 0x06054b50).
  const minEocdOffset = Math.max(0, buf.length - 22 - 65535)
  let eocdOffset = -1
  for (let i = buf.length - 22; i >= minEocdOffset; i--) {
    if (view.getUint32(i, true) === 0x06054b50) {
      const commentLen = view.getUint16(i + 20, true)
      if (i + 22 + commentLen === buf.length) {
        eocdOffset = i
        break
      }
    }
  }
  if (eocdOffset < 0) {
    throw new Error(`invalid zip archive ${assetName}: missing end-of-central-directory record`)
  }

  let expectedEntries = view.getUint16(eocdOffset + 10, true)
  let cdSize = view.getUint32(eocdOffset + 12, true)
  let cdOffset = view.getUint32(eocdOffset + 16, true)

  // Handle ZIP64 EOCD when sentinel values are present.
  if (expectedEntries === 0xffff || cdSize === 0xffffffff || cdOffset === 0xffffffff) {
    const locatorOffset = eocdOffset - 20
    if (locatorOffset < 0 || view.getUint32(locatorOffset, true) !== 0x07064b50) {
      throw new Error(`invalid zip64 archive ${assetName}: missing ZIP64 locator`)
    }
    const zip64EocdOffset = Number(view.getBigUint64(locatorOffset + 8, true))
    if (
      !Number.isSafeInteger(zip64EocdOffset)
      || zip64EocdOffset < 0
      || zip64EocdOffset + 56 > buf.length
      || view.getUint32(zip64EocdOffset, true) !== 0x06064b50
    ) {
      throw new Error(`invalid zip64 archive ${assetName}: missing ZIP64 EOCD`)
    }
    expectedEntries = Number(view.getBigUint64(zip64EocdOffset + 32, true))
    cdSize = Number(view.getBigUint64(zip64EocdOffset + 40, true))
    cdOffset = Number(view.getBigUint64(zip64EocdOffset + 48, true))
  }

  if (expectedEntries > maxEntries) {
    throw new Error(`archive ${assetName} exceeds maximum entry count (${maxEntries})`)
  }
  if (cdOffset < 0 || cdSize < 0 || cdOffset + cdSize > buf.length) {
    throw new Error(`invalid zip archive ${assetName}: central directory out of bounds`)
  }

  const decoder = new TextDecoder()
  let offset = cdOffset
  const cdEnd = cdOffset + cdSize
  let entryCount = 0
  let totalBytes = 0

  while (offset < cdEnd) {
    if (offset + 46 > cdEnd || view.getUint32(offset, true) !== 0x02014b50) {
      throw new Error(`invalid zip archive ${assetName}: corrupt central directory header`)
    }
    const versionMadeBy = view.getUint16(offset + 4, true)
    const compressedSize32 = view.getUint32(offset + 20, true)
    const uncompressedSize32 = view.getUint32(offset + 24, true)
    const fileNameLen = view.getUint16(offset + 28, true)
    const extraLen = view.getUint16(offset + 30, true)
    const commentLen = view.getUint16(offset + 32, true)
    const externalAttr = view.getUint32(offset + 38, true)
    const localHeaderOffset32 = view.getUint32(offset + 42, true)

    const headerEnd = offset + 46 + fileNameLen + extraLen + commentLen
    if (headerEnd > cdEnd) {
      throw new Error(`invalid zip archive ${assetName}: truncated central directory entry`)
    }

    const fileName = decoder.decode(buf.subarray(offset + 46, offset + 46 + fileNameLen))

    let uncompressedSize = uncompressedSize32
    let localHeaderOffset = localHeaderOffset32
    if (
      uncompressedSize32 === 0xffffffff
      || compressedSize32 === 0xffffffff
      || localHeaderOffset32 === 0xffffffff
    ) {
      // Parse ZIP64 extra field (header ID 0x0001).
      let ex = offset + 46 + fileNameLen
      const exEnd = ex + extraLen
      let foundZip64 = false
      while (ex + 4 <= exEnd) {
        const tag = view.getUint16(ex, true)
        const size = view.getUint16(ex + 2, true)
        if (ex + 4 + size > exEnd) break
        if (tag === 0x0001) {
          let cursor = ex + 4
          if (uncompressedSize32 === 0xffffffff) {
            if (cursor + 8 > ex + 4 + size) throw new Error(`truncated zip64 extra field in ${assetName}`)
            const v = view.getBigUint64(cursor, true)
            if (v > BigInt(Number.MAX_SAFE_INTEGER)) {
              throw new Error(`archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
            }
            uncompressedSize = Number(v)
            cursor += 8
          }
          if (compressedSize32 === 0xffffffff) {
            if (cursor + 8 > ex + 4 + size) throw new Error(`truncated zip64 extra field in ${assetName}`)
            cursor += 8
          }
          if (localHeaderOffset32 === 0xffffffff) {
            if (cursor + 8 > ex + 4 + size) throw new Error(`truncated zip64 extra field in ${assetName}`)
            localHeaderOffset = Number(view.getBigUint64(cursor, true))
          }
          foundZip64 = true
          break
        }
        ex += 4 + size
      }
      if (!foundZip64) {
        throw new Error(`invalid zip64 archive ${assetName}: missing zip64 extra field for '${fileName}'`)
      }
    }

    // Validate Unix file type bits when created by a Unix host (versionMadeBy >> 8 === 3).
    const hostOs = versionMadeBy >> 8
    let isDir = fileName.endsWith("/")
    if (hostOs === 3) {
      const fileType = (externalAttr >>> 16) & 0o170000
      if (fileType === 0o120000) {
        throw new Error(`refusing symlink '${fileName}' in ${assetName}`)
      }
      if (fileType === 0o040000) {
        isDir = true
      } else if (fileType !== 0 && fileType !== 0o100000) {
        throw new Error(`refusing special file '${fileName}' (mode 0o${fileType.toString(8)}) in ${assetName}`)
      }
    }

    assertSafeArchiveEntryPath(fileName, isDir, assetName)

    // Verify local file header signature & matching filename (prevents parser differentials).
    if (localHeaderOffset + 30 > buf.length || view.getUint32(localHeaderOffset, true) !== 0x04034b50) {
      throw new Error(`invalid zip archive ${assetName}: missing local header for '${fileName}'`)
    }
    const localNameLen = view.getUint16(localHeaderOffset + 26, true)
    if (localHeaderOffset + 30 + localNameLen > buf.length) {
      throw new Error(`invalid zip archive ${assetName}: truncated local header for '${fileName}'`)
    }
    const localName = decoder.decode(buf.subarray(localHeaderOffset + 30, localHeaderOffset + 30 + localNameLen))
    if (localName !== fileName) {
      throw new Error(`refusing zip filename mismatch ('${localName}' != '${fileName}') in ${assetName}`)
    }

    entryCount++
    if (entryCount > maxEntries) {
      throw new Error(`archive ${assetName} exceeds maximum entry count (${maxEntries})`)
    }
    totalBytes += uncompressedSize
    if (uncompressedSize > maxExtractedBytes || totalBytes > maxExtractedBytes) {
      throw new Error(`archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
    }

    offset = headerEnd
  }

  if (entryCount === 0) {
    throw new Error(`archive ${assetName} contains no entries`)
  }
  return { entries: entryCount, totalBytes }
}

/** Post-extraction defense-in-depth verification of extracted filesystem tree. */
function assertSafeExtractedTree(
  dir: string,
  assetName: string,
  limits: ArchiveLimits = {},
): void {
  const maxExtractedBytes = limits.maxExtractedBytes ?? MAX_EXTRACTED_BYTES
  const maxEntries = limits.maxEntries ?? MAX_ARCHIVE_ENTRIES
  let entries = 0
  let totalBytes = 0

  const walk = (current: string): void => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name)
      const stat = lstatSync(full)
      entries++
      if (entries > maxEntries) {
        throw new Error(`extracted archive ${assetName} exceeds maximum entry count (${maxEntries})`)
      }
      if (stat.isSymbolicLink()) {
        throw new Error(`refusing symlink '${entry.name}' in ${assetName}`)
      }
      if (stat.isDirectory()) {
        walk(full)
      } else if (stat.isFile()) {
        if (stat.nlink > 1) {
          throw new Error(`refusing hard link '${entry.name}' in ${assetName}`)
        }
        totalBytes += stat.size
        if (totalBytes > maxExtractedBytes) {
          throw new Error(`extracted archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
        }
      } else {
        throw new Error(`refusing non-regular file '${entry.name}' in ${assetName}`)
      }
    }
  }
  walk(dir)
}

/**
 * Inspect an archive BEFORE creating `destDir` or extracting, extract it, and
 * run post-extraction tree verification as defense-in-depth.
 */
export function extractArchive(
  archivePath: string,
  destDir: string,
  assetName: string,
  limits: ArchiveLimits = {},
): void {
  const isZip = assetName.endsWith(".zip") || archivePath.endsWith(".zip")
  if (isZip) {
    inspectZipArchive(archivePath, assetName, limits)
  } else {
    inspectTarGzArchive(archivePath, assetName, limits)
  }

  mkdirSync(destDir, { recursive: true })
  const run = isZip
    ? spawnSync("unzip", ["-q", archivePath, "-d", destDir], { encoding: "utf8" })
    : spawnSync("tar", ["-xzf", archivePath, "-C", destDir], { encoding: "utf8" })
  if (run.status !== 0) {
    throw new Error(
      `failed to extract ${assetName}: ${run.stderr.trim() || `${isZip ? "unzip" : "tar"} exited non-zero`}`,
    )
  }
  assertSafeExtractedTree(destDir, assetName, limits)
}

function assertRegularFile(path: string, label: string): void {
  if (!existsSync(path)) throw new Error(`${label} is missing from the extracted archive`)
  if (!lstatSync(path).isFile()) throw new Error(`${label} is not a regular file`)
}

/**
 * Download, verify, and install the prebuilt local runtime into
 * `<cacheRoot>/runtime/generations/<genId>` with an atomic `current` pointer.
 *
 * Guarantees fail-closed behavior: if `SHA256SUMS` is missing an asset, any
 * download or redirect check fails, any archive's SHA-256 differs from
 * `SHA256SUMS`, or any archive fails pre-extraction safety inspection, this
 * throws before extracting anything and without touching an existing active
 * generation.
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
  const fetchImpl = options.fetchImpl ?? fetch

  const wasmAsset = "norn-checker-wasm.tar.gz"
  const devHostAsset = `norn-dev-host-${platform}.tar.gz`
  const cliAsset = `norn-${platform}.tar.gz`
  const assets = [wasmAsset, devHostAsset, cliAsset] as const

  // Phase 1: Download & verify strictly in a temporary directory. Cache is untouched until verified.
  const workDir = mkdtempSync(join(tmpdir(), "norn-setup-"))
  try {
    const sumsBytes = await fetchBounded(`${baseUrl}/SHA256SUMS`, MAX_SUMS_BYTES, timeoutMs, fetchImpl)
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
      const bytes = await fetchBounded(`${baseUrl}/${asset}`, MAX_ARCHIVE_BYTES, timeoutMs, fetchImpl)
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

    // Phase 2: Inspect & extract verified archives in temp dir and check required payloads.
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

    // Phase 3: Stage a complete immutable generation and switch `runtime/current` atomically.
    const runtimeDir = join(cacheRoot, "runtime")
    const generationsDir = join(runtimeDir, "generations")
    mkdirSync(generationsDir, { recursive: true })

    const generationId = `${Date.now()}-${process.pid}-${randomBytes(4).toString("hex")}`
    const stagingDir = join(generationsDir, `.staging.${generationId}`)
    const generationDir = join(generationsDir, generationId)
    const pointerPath = join(runtimeDir, "current")
    const pointerTmp = join(runtimeDir, `.current.${process.pid}.${randomBytes(4).toString("hex")}.tmp`)

    let committed = false
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
            format: RUNTIME_MANIFEST_FORMAT,
            version,
            platform,
            baseUrl,
            sha256: expectedDigests,
          },
          null,
          2,
        )}\n`,
      )

      // Move completed staging tree into its immutable generation directory,
      // then atomically switch the single `current` pointer file.
      renameSync(stagingDir, generationDir)
      writeFileSync(pointerTmp, `${generationId}\n`)
      renameSync(pointerTmp, pointerPath)
      committed = true
    } catch (error) {
      rmSync(pointerTmp, { force: true })
      rmSync(stagingDir, { recursive: true, force: true })
      if (!committed) rmSync(generationDir, { recursive: true, force: true })
      throw error
    }

    // Phase 4: Clean up superseded generations and any legacy direct layout ONLY after the pointer swap succeeded.
    for (const entry of readdirSync(generationsDir)) {
      if (entry === generationId) continue
      if (entry.startsWith(".staging.")) continue
      rmSync(join(generationsDir, entry), { recursive: true, force: true })
    }
    for (const legacy of ["checker-wasm", "bin", "manifest.json"]) {
      const legacyPath = join(runtimeDir, legacy)
      if (existsSync(legacyPath)) {
        rmSync(legacyPath, { recursive: true, force: true })
      }
    }

    return {
      version,
      platform,
      baseUrl,
      generationId,
      runtimeDir: generationDir,
      checkerWasmDir: join(generationDir, "checker-wasm"),
      devHostBin: join(generationDir, "bin", "norn-dev-host"),
      cliBin: join(generationDir, "bin", "norn"),
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
