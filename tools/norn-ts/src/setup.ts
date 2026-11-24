#!/usr/bin/env bun
/**
 * `norn setup`: download and verify prebuilt local runtime artifacts
 * (`norn-checker-wasm`, `norn-dev-host`, and `norn` CLI) from GitHub Releases
 * (or a `--base-url` mirror) into `~/.cache/norn/runtime/`.
 *
 * Fail-closed verification:
 * 1. Fetches `SHA256SUMS` and every archive with `redirect: "manual"`,
 *    rejecting URL userinfo, rejecting any `https:` -> `http:` downgrade (even
 *    to loopback), and re-validating every redirect hop against the HTTPS /
 *    loopback policy.
 * 2. Checks that every required archive has a single 64-hex SHA-256 entry and
 *    verifies EVERY archive's SHA-256 BEFORE inspecting or extracting anything.
 * 3. Preflight-inspects ALL archives (type, path, linkname, actual bounded
 *    decompressed byte length, CRC32/checksums, member count) BEFORE extracting
 *    ANY archive onto disk, plus post-extraction verification as defense-in-depth.
 * 4. Publishes complete runtime generations into `runtime/generations/<genId>/`
 *    and switches via ONE atomic pointer update (`current.tmp` -> `current`),
 *    re-reading `current` and preserving both recent and live generations so
 *    concurrent installers and readers are never stranded.
 */
import { spawnSync } from "node:child_process"
import { createHash, randomBytes } from "node:crypto"
import {
  chmodSync,
  closeSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join, posix, resolve, sep } from "node:path"
import { gunzipSync, inflateRawSync } from "node:zlib"
import {
  RUNTIME_MANIFEST_FORMAT,
  defaultCacheRoot,
  detectPlatform,
  readActiveGenerationId,
} from "./runtime-cache.ts"

export const DEFAULT_REPO = "Norn-cloud/norn"
export const SETUP_DOWNLOAD_TIMEOUT_MS = 600_000
export const MAX_REDIRECTS = 5
export const MAX_EXTRACTED_BYTES = 256 * 1024 * 1024
export const MAX_ARCHIVE_ENTRIES = 4096
export const GENERATION_CLEANUP_GRACE_MS = 60_000
export const KEEP_RECENT_GENERATIONS = 2
export const PUBLISH_LOCK_TIMEOUT_MS = 15_000
export const PUBLISH_LOCK_STALE_MS = 60_000
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
  readonly extractImpl?: typeof extractArchive
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
export const detectReleasePlatform = detectPlatform

/**
 * Strip/redact URL userinfo (`username` and `password`) before formatting a
 * URL into any error message or diagnostic so credentials never leak to stderr.
 */
export function sanitizeUrlForDiagnostic(rawUrl: string): string {
  try {
    const parsed = new URL(rawUrl)
    if (parsed.username !== "" || parsed.password !== "") {
      parsed.username = ""
      parsed.password = ""
      const stripped = parsed.toString()
      const sepIdx = stripped.indexOf("://")
      return sepIdx >= 0
        ? `${stripped.slice(0, sepIdx + 3)}***@${stripped.slice(sepIdx + 3)}`
        : stripped
    }
    return parsed.toString()
  } catch {
    return rawUrl.replace(/((?:[a-zA-Z][a-zA-Z0-9+.-]*:)?\/\/)([^/@\s]+)@/g, "$1***@")
  }
}

/**
 * Reject URL userinfo and remote plaintext HTTP; allow https or local loopback
 * mirrors for offline/tests.
 */
export function isFetchableUrl(url: string): boolean {
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return false
  }
  if (parsed.username !== "" || parsed.password !== "") {
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
    throw new Error(
      `refusing unsafe or non-https release URL ${sanitizeUrlForDiagnostic(baseUrl)} (userinfo is forbidden; plain http is allowed only on loopback)`,
    )
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
 * `MAX_REDIRECTS` redirects manually, rejecting userinfo, rejecting ANY
 * `https:` -> `http:` downgrade (even to loopback), and re-validating every
 * redirect hop against `isFetchableUrl` before issuing the next request.
 */
export async function fetchBounded(
  url: string,
  limit: number,
  timeoutMs: number,
  fetchImpl: FetchLike = fetch,
): Promise<Uint8Array> {
  if (!isFetchableUrl(url)) {
    throw new Error(
      `refusing unsafe or non-https URL ${sanitizeUrlForDiagnostic(url)} (userinfo is forbidden; plain http is allowed only on loopback)`,
    )
  }
  const deadline = Date.now() + timeoutMs
  let currentUrl = url
  let redirects = 0
  let response: Response

  for (;;) {
    const remaining = deadline - Date.now()
    if (remaining <= 0) {
      throw new Error(`timed out fetching ${sanitizeUrlForDiagnostic(currentUrl)}`)
    }
    response = await fetchImpl(currentUrl, {
      redirect: "manual",
      signal: AbortSignal.timeout(remaining),
    })
    if (!REDIRECT_STATUSES.has(response.status)) break

    await response.body?.cancel()
    if (redirects >= MAX_REDIRECTS) {
      throw new Error(`too many redirects (>${MAX_REDIRECTS}) fetching ${sanitizeUrlForDiagnostic(url)}`)
    }
    const location = response.headers.get("location")
    if (!location) {
      throw new Error(
        `HTTP ${response.status} redirect without Location header fetching ${sanitizeUrlForDiagnostic(currentUrl)}`,
      )
    }
    const prevParsed = new URL(currentUrl)
    let nextParsed: URL
    try {
      nextParsed = new URL(location, currentUrl)
    } catch {
      throw new Error(
        `invalid redirect Location '${sanitizeUrlForDiagnostic(location)}' from ${sanitizeUrlForDiagnostic(currentUrl)}`,
      )
    }
    const nextUrl = nextParsed.toString()
    if (prevParsed.protocol === "https:" && nextParsed.protocol === "http:") {
      throw new Error(
        `refusing redirect downgrade from https (${sanitizeUrlForDiagnostic(currentUrl)}) to http (${sanitizeUrlForDiagnostic(nextUrl)})`,
      )
    }
    if (!isFetchableUrl(nextUrl)) {
      throw new Error(
        `refusing redirect from ${sanitizeUrlForDiagnostic(currentUrl)} to unsafe or non-https URL ${sanitizeUrlForDiagnostic(nextUrl)} (userinfo is forbidden; plain http is allowed only on loopback)`,
      )
    }
    currentUrl = nextUrl
    redirects++
  }

  if (!response.ok || !response.body) {
    throw new Error(`HTTP ${response.status} fetching ${sanitizeUrlForDiagnostic(currentUrl)}`)
  }
  const declared = Number(response.headers.get("content-length") ?? "NaN")
  if (Number.isFinite(declared) && declared > limit) {
    await response.body.cancel()
    throw new Error(`${sanitizeUrlForDiagnostic(currentUrl)} exceeds size limit (${declared} > ${limit} bytes)`)
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
      throw new Error(`${sanitizeUrlForDiagnostic(currentUrl)} exceeded size limit (${limit} bytes)`)
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

const CRC32_TABLE = (() => {
  const table = new Uint32Array(256)
  for (let i = 0; i < 256; i++) {
    let c = i
    for (let k = 0; k < 8; k++) {
      c = (c & 1) !== 0 ? (0xedb88320 ^ (c >>> 1)) : (c >>> 1)
    }
    table[i] = c >>> 0
  }
  return table
})()

export function crc32(data: Uint8Array): number {
  let c = 0xffffffff
  for (let i = 0; i < data.length; i++) {
    c = CRC32_TABLE[(c ^ data[i]!) & 0xff]! ^ (c >>> 8)
  }
  return (c ^ 0xffffffff) >>> 0
}

interface VerifiedZipEntry {
  readonly name: string
  readonly isDir: boolean
  readonly data: Uint8Array
  readonly unixPerm: number
}

function parseAndVerifyZipArchive(
  archivePath: string,
  assetName: string,
  limits: ArchiveLimits = {},
): { readonly entries: ReadonlyArray<VerifiedZipEntry>; readonly totalBytes: number } {
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
  const verified: VerifiedZipEntry[] = []
  let totalBytes = 0

  while (offset < cdEnd) {
    if (offset + 46 > cdEnd || view.getUint32(offset, true) !== 0x02014b50) {
      throw new Error(`invalid zip archive ${assetName}: corrupt central directory header`)
    }
    const flags = view.getUint16(offset + 8, true)
    const method = view.getUint16(offset + 10, true)
    const expectedCrc = view.getUint32(offset + 16, true)
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

    if ((flags & 0x1) !== 0) {
      throw new Error(`refusing encrypted zip entry '${fileName}' in ${assetName}`)
    }
    if (method !== 0 && method !== 8) {
      throw new Error(`refusing unsupported zip compression method ${method} for '${fileName}' in ${assetName}`)
    }

    let compressedSize = compressedSize32
    let uncompressedSize = uncompressedSize32
    let localHeaderOffset = localHeaderOffset32
    if (
      uncompressedSize32 === 0xffffffff
      || compressedSize32 === 0xffffffff
      || localHeaderOffset32 === 0xffffffff
    ) {
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
            const v = view.getBigUint64(cursor, true)
            if (v > BigInt(Number.MAX_SAFE_INTEGER)) {
              throw new Error(`archive ${assetName} exceeds maximum compressed size in ${assetName}`)
            }
            compressedSize = Number(v)
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

    // Reject symlinks and special file type bits regardless of `versionMadeBy` OS byte.
    const upperMode = externalAttr >>> 16
    const fileType = upperMode & 0o170000
    let isDir = fileName.endsWith("/")
    if (fileType === 0o120000) {
      throw new Error(`refusing symlink '${fileName}' in ${assetName}`)
    }
    if (fileType === 0o040000) {
      isDir = true
    } else if (fileType !== 0 && fileType !== 0o100000) {
      throw new Error(`refusing special file '${fileName}' (mode 0o${fileType.toString(8)}) in ${assetName}`)
    }

    assertSafeArchiveEntryPath(fileName, isDir, assetName)

    if (verified.length + 1 > maxEntries) {
      throw new Error(`archive ${assetName} exceeds maximum entry count (${maxEntries})`)
    }

    // Verify local file header signature, compression method, and filename match.
    if (localHeaderOffset + 30 > buf.length || view.getUint32(localHeaderOffset, true) !== 0x04034b50) {
      throw new Error(`invalid zip archive ${assetName}: missing local header for '${fileName}'`)
    }
    const localMethod = view.getUint16(localHeaderOffset + 8, true)
    if (localMethod !== method) {
      throw new Error(`refusing zip compression method mismatch for '${fileName}' in ${assetName}`)
    }
    const localNameLen = view.getUint16(localHeaderOffset + 26, true)
    const localExtraLen = view.getUint16(localHeaderOffset + 28, true)
    const dataStart = localHeaderOffset + 30 + localNameLen + localExtraLen
    const dataEnd = dataStart + compressedSize
    if (localHeaderOffset + 30 + localNameLen > buf.length || dataEnd > buf.length) {
      throw new Error(`invalid zip archive ${assetName}: truncated local entry data for '${fileName}'`)
    }
    const localName = decoder.decode(buf.subarray(localHeaderOffset + 30, localHeaderOffset + 30 + localNameLen))
    if (localName !== fileName) {
      throw new Error(`refusing zip filename mismatch ('${localName}' != '${fileName}') in ${assetName}`)
    }

    const remainingBudget = maxExtractedBytes - totalBytes
    if (uncompressedSize > remainingBudget) {
      throw new Error(`archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
    }

    // Decompress with a strict byte budget instead of trusting the declared uncompressed size.
    const compressedSlice = buf.subarray(dataStart, dataEnd)
    let expanded: Uint8Array
    if (method === 0) {
      if (compressedSlice.byteLength > remainingBudget) {
        throw new Error(`archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
      }
      expanded = compressedSlice
    } else {
      try {
        expanded = inflateRawSync(compressedSlice, { maxOutputLength: Math.max(remainingBudget, 1) })
      } catch (error) {
        const code = (error as NodeJS.ErrnoException | undefined)?.code
        if (code === "ERR_BUFFER_TOO_LARGE") {
          throw new Error(`archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
        }
        throw new Error(
          `failed to inflate zip entry '${fileName}' in ${assetName}: ${error instanceof Error ? error.message : String(error)}`,
        )
      }
      if (expanded.byteLength > remainingBudget) {
        throw new Error(`archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
      }
    }

    if (expanded.byteLength !== uncompressedSize) {
      throw new Error(
        `zip entry '${fileName}' expanded size (${expanded.byteLength}) does not match declared size (${uncompressedSize}) in ${assetName}`,
      )
    }
    if (isDir && expanded.byteLength !== 0) {
      throw new Error(`refusing directory '${fileName}' with non-zero size in ${assetName}`)
    }
    const actualCrc = crc32(expanded)
    if (actualCrc !== expectedCrc) {
      throw new Error(
        `zip entry '${fileName}' CRC-32 mismatch (expected ${expectedCrc.toString(16)}, got ${actualCrc.toString(16)}) in ${assetName}`,
      )
    }

    totalBytes += expanded.byteLength
    if (totalBytes > maxExtractedBytes) {
      throw new Error(`archive ${assetName} exceeds maximum uncompressed size (${maxExtractedBytes} bytes)`)
    }

    verified.push({
      name: fileName,
      isDir,
      data: expanded,
      unixPerm: upperMode & 0o777,
    })

    offset = headerEnd
  }

  if (verified.length === 0) {
    throw new Error(`archive ${assetName} contains no entries`)
  }
  return { entries: verified, totalBytes }
}

/**
 * Inspect EVERY entry of a `.zip` archive via its Central Directory, Local File
 * Headers, and bounded raw decompression BEFORE extraction:
 * - rejects symlinks and special POSIX file modes regardless of `versionMadeBy` OS byte;
 * - rejects absolute paths, backslashes, drive prefixes, `..` traversal, and
 *   LFH-vs-CD filename/method mismatches;
 * - inflates every entry with a strict remaining byte budget (`maxOutputLength`)
 *   and checks actual expanded byte length + CRC-32 instead of trusting headers.
 */
export function inspectZipArchive(
  archivePath: string,
  assetName: string,
  limits: ArchiveLimits = {},
): ArchiveSummary {
  const { entries, totalBytes } = parseAndVerifyZipArchive(archivePath, assetName, limits)
  return { entries: entries.length, totalBytes }
}

/** Dispatch preflight inspection for `.tar.gz` / `.tgz` or `.zip` archives. */
export function inspectArchive(
  archivePath: string,
  assetName: string,
  limits: ArchiveLimits = {},
): ArchiveSummary {
  const isZip = assetName.endsWith(".zip") || archivePath.endsWith(".zip")
  return isZip
    ? inspectZipArchive(archivePath, assetName, limits)
    : inspectTarGzArchive(archivePath, assetName, limits)
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
 * Inspect an archive BEFORE creating `destDir` or extracting, extract it
 * safely (writing verified `.zip` entries directly via bounded `inflateRawSync`
 * with no `unzip` subprocess), and run post-extraction tree verification as
 * defense-in-depth.
 */
export function extractArchive(
  archivePath: string,
  destDir: string,
  assetName: string,
  limits: ArchiveLimits = {},
): void {
  const isZip = assetName.endsWith(".zip") || archivePath.endsWith(".zip")
  if (isZip) {
    const { entries } = parseAndVerifyZipArchive(archivePath, assetName, limits)
    mkdirSync(destDir, { recursive: true })
    const rootResolved = resolve(destDir)
    for (const entry of entries) {
      const rel = posix.normalize(entry.name)
      const target = rel === "." ? rootResolved : resolve(rootResolved, ...rel.split("/").filter(Boolean))
      if (target !== rootResolved && !target.startsWith(`${rootResolved}${sep}`)) {
        throw new Error(`refusing unsafe archive entry '${entry.name}' in ${assetName}`)
      }
      if (entry.isDir) {
        mkdirSync(target, { recursive: true })
      } else {
        mkdirSync(dirname(target), { recursive: true })
        writeFileSync(target, entry.data)
        if ((entry.unixPerm & 0o111) !== 0) {
          chmodSync(target, 0o755)
        }
      }
    }
  } else {
    inspectTarGzArchive(archivePath, assetName, limits)
    mkdirSync(destDir, { recursive: true })
    const run = spawnSync("tar", ["-xzf", archivePath, "-C", destDir], { encoding: "utf8" })
    if (run.status !== 0) {
      throw new Error(`failed to extract ${assetName}: ${run.stderr.trim() || "tar exited non-zero"}`)
    }
  }
  assertSafeExtractedTree(destDir, assetName, limits)
}

function assertRegularFile(path: string, label: string): void {
  if (!existsSync(path)) throw new Error(`${label} is missing from the extracted archive`)
  if (!lstatSync(path).isFile()) throw new Error(`${label} is not a regular file`)
}

function isProcessDead(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false
  if (pid === process.pid) return false
  try {
    process.kill(pid, 0)
    return false
  } catch (error) {
    return (error as NodeJS.ErrnoException | undefined)?.code === "ESRCH"
  }
}

function isPublishLockStale(lockPath: string, staleMs: number): boolean {
  let stat: ReturnType<typeof lstatSync>
  try {
    stat = lstatSync(lockPath)
  } catch {
    return false
  }
  const now = Date.now()
  if (now - stat.mtimeMs > staleMs) return true
  let raw: string
  try {
    raw = readFileSync(lockPath, "utf8").trim()
  } catch {
    return false
  }
  if (!raw) return false
  try {
    const parsed = JSON.parse(raw) as { readonly pid?: unknown; readonly createdAt?: unknown }
    if (typeof parsed.createdAt === "number" && Number.isFinite(parsed.createdAt) && now - parsed.createdAt > staleMs) {
      return true
    }
    if (typeof parsed.pid === "number" && isProcessDead(parsed.pid)) {
      return true
    }
  } catch {
    // Only treat corrupt/partially-written lock content as stale after the grace period above.
    return false
  }
  return false
}

/**
 * Acquire an exclusive filesystem lock (`<runtimeDir>/.publish.lock`) via
 * `openSync(..., "wx")` with bounded retry/backoff and stale-lock reclaim
 * (dead holder PID or expired lock age), serializing generation promotion,
 * atomic `current` pointer swap, and old-generation pruning.
 */
export async function withPublishLock<T>(
  runtimeDir: string,
  fn: () => Promise<T> | T,
  options: { readonly timeoutMs?: number; readonly staleMs?: number } = {},
): Promise<T> {
  const lockPath = join(runtimeDir, ".publish.lock")
  const timeoutMs = options.timeoutMs ?? PUBLISH_LOCK_TIMEOUT_MS
  const staleMs = options.staleMs ?? PUBLISH_LOCK_STALE_MS
  const token = randomBytes(8).toString("hex")
  const deadline = Date.now() + timeoutMs
  let backoffMs = 10

  for (;;) {
    try {
      const fd = openSync(lockPath, "wx", 0o600)
      try {
        writeFileSync(
          fd,
          `${JSON.stringify({ pid: process.pid, createdAt: Date.now(), token })}\n`,
        )
      } finally {
        closeSync(fd)
      }
      break
    } catch (error) {
      const code = (error as NodeJS.ErrnoException | undefined)?.code
      if (code !== "EEXIST") throw error
      if (isPublishLockStale(lockPath, staleMs)) {
        try {
          rmSync(lockPath, { force: true })
        } catch {
          // Another contender may have reclaimed or released the lock first.
        }
        continue
      }
      if (Date.now() >= deadline) {
        throw new Error(`timed out waiting for runtime publish lock ${lockPath}`)
      }
      await new Promise((wake) => setTimeout(wake, backoffMs))
      backoffMs = Math.min(backoffMs * 2, 100)
    }
  }

  try {
    return await fn()
  } finally {
    try {
      const current = readFileSync(lockPath, "utf8")
      if (current.includes(token)) {
        rmSync(lockPath, { force: true })
      }
    } catch {
      // Lock file already gone or inaccessible; do not mask caller errors.
    }
  }
}

/**
 * Prune superseded generations safely after a pointer swap:
 * - called inside the exclusive `.publish.lock` critical section;
 * - re-reads `runtime/current` so any live active generation is preserved
 *   alongside `generationId`;
 * - keeps the `KEEP_RECENT_GENERATIONS` most recent generations;
 * - skips any generation whose mtime is newer than `graceMs` (default 60s) so
 *   in-flight readers never lose their files.
 */
function pruneOldGenerations(
  cacheRoot: string,
  generationsDir: string,
  generationId: string,
  graceMs: number = GENERATION_CLEANUP_GRACE_MS,
): void {
  const liveCurrent = readActiveGenerationId(cacheRoot)
  const protectedIds = new Set<string>([generationId])
  if (liveCurrent) protectedIds.add(liveCurrent)

  const nowMs = Date.now()
  const completed: Array<{ readonly name: string; readonly mtimeMs: number }> = []
  for (const name of readdirSync(generationsDir)) {
    if (name.startsWith(".staging.")) continue
    try {
      const stat = lstatSync(join(generationsDir, name))
      completed.push({ name, mtimeMs: stat.mtimeMs })
    } catch {
      // Entry disappeared concurrently.
    }
  }
  completed.sort((a, b) => (b.mtimeMs - a.mtimeMs) || b.name.localeCompare(a.name))
  for (const recent of completed.slice(0, KEEP_RECENT_GENERATIONS)) {
    protectedIds.add(recent.name)
  }

  for (const entry of completed) {
    if (protectedIds.has(entry.name)) continue
    if (nowMs - entry.mtimeMs < graceMs) continue
    try {
      rmSync(join(generationsDir, entry.name), { recursive: true, force: true })
    } catch {
      // Best-effort cleanup; ignore concurrent deletion races.
    }
  }
}

/**
 * Download, verify, preflight-inspect, and install the prebuilt local runtime
 * into `<cacheRoot>/runtime/generations/<genId>` with an atomic `current`
 * pointer.
 *
 * Guarantees fail-closed behavior: if `SHA256SUMS` is missing an asset, any
 * download or redirect check fails, any archive's SHA-256 differs from
 * `SHA256SUMS`, or ANY archive fails preflight safety inspection, this throws
 * before extracting ANY archive onto disk and without touching an existing
 * active generation.
 */
export async function setupLocalRuntime(options: SetupOptions = {}): Promise<SetupResult> {
  const env = options.env ?? process.env
  const platform = options.platform ?? detectPlatform()
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
  const extractImpl = options.extractImpl ?? extractArchive

  const wasmAsset = "norn-checker-wasm.tar.gz"
  const devHostAsset = `norn-dev-host-${platform}.tar.gz`
  const cliAsset = `norn-${platform}.tar.gz`
  const assets = [wasmAsset, devHostAsset, cliAsset] as const

  // Phase 1: Download & verify SHA-256 strictly in a temporary directory. Cache is untouched.
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

    // Phase 2a: Preflight-inspect ALL verified archives BEFORE extracting ANY onto disk.
    for (const asset of assets) {
      inspectArchive(downloaded.get(asset)!, asset)
    }

    // Phase 2b: Extract verified & preflighted archives in temp dir and check required payloads.
    const wasmExtract = join(workDir, "extract-wasm")
    const devHostExtract = join(workDir, "extract-dev-host")
    const cliExtract = join(workDir, "extract-cli")

    extractImpl(downloaded.get(wasmAsset)!, wasmExtract, wasmAsset)
    assertRegularFile(join(wasmExtract, "norn_checker_wasm.js"), `${wasmAsset}: norn_checker_wasm.js`)
    assertRegularFile(join(wasmExtract, "norn_checker_wasm_bg.wasm"), `${wasmAsset}: norn_checker_wasm_bg.wasm`)

    extractImpl(downloaded.get(devHostAsset)!, devHostExtract, devHostAsset)
    assertRegularFile(join(devHostExtract, "norn-dev-host"), `${devHostAsset}: norn-dev-host`)

    extractImpl(downloaded.get(cliAsset)!, cliExtract, cliAsset)
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

      // Serialize promotion, `current` pointer swap, and old-generation pruning
      // under an exclusive filesystem lock so no concurrent publisher/pruner can
      // interleave between promoting `generationDir` and switching `current`.
      await withPublishLock(runtimeDir, () => {
        renameSync(stagingDir, generationDir)
        if (!existsSync(generationDir)) {
          throw new Error(`staged runtime generation ${generationDir} vanished before pointer swap`)
        }
        writeFileSync(pointerTmp, `${generationId}\n`)
        renameSync(pointerTmp, pointerPath)
        committed = true

        // Phase 4: Prune superseded generations & legacy direct layout inside the lock.
        pruneOldGenerations(cacheRoot, generationsDir, generationId)
        for (const legacy of ["checker-wasm", "bin", "manifest.json"]) {
          const legacyPath = join(runtimeDir, legacy)
          if (existsSync(legacyPath)) {
            rmSync(legacyPath, { recursive: true, force: true })
          }
        }
      })
    } catch (error) {
      rmSync(pointerTmp, { force: true })
      rmSync(stagingDir, { recursive: true, force: true })
      if (!committed) rmSync(generationDir, { recursive: true, force: true })
      throw error
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
