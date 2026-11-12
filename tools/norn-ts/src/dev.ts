/**
 * `norn dev` (#207): find the project, then run the native dev host with
 * this package's compile worker. The native host owns the cell, admission and
 * serving; this front door only locates the pieces.
 *
 * The host binary is `norn-dev-host`, built from the `norn-cell-do` crate. It
 * is found in this order:
 * 1. `NORN_DEV_HOST_BIN`, an override that wins whenever it is set;
 * 2. the platform package `@norn/dev-host-<platform>-<arch>`, when installed,
 *    run only if its sha256 is the one this release records (no record, no run);
 * 3. the prebuilt host this release records for the platform
 *    (`generated/build.json`), downloaded once into the cache and kept only
 *    if its sha256 is the recorded one.
 * Otherwise `norn dev` refuses with `cli.setup.host_unavailable` and says how
 * to build the host from source.
 *
 * `norn dev --json` events and `.norn/dev.json` (`norn-dev/1`) are
 * experimental: they carry `"stability": "experimental"` and are not a
 * frozen contract (D84) until they are enrolled in `scripts/ci/contract_diff.py`.
 */
import { createHash } from "node:crypto"
import { chmodSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs"
import { createRequire } from "node:module"
import { homedir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { warnAuthoringVersion } from "./authoring-version.ts"
import { buildInfo, type BuildInfo } from "./build-info.ts"
import { TOOL_ROOT } from "./profile.ts"
import { ProjectError, readProject } from "./project.ts"

/** What finding the host depends on; a test replaces parts of it. */
export interface HostLookup {
  readonly env: Readonly<Record<string, string | undefined>>
  /** `<platform>-<arch>`, as Node names them: `linux-x64`, `darwin-arm64`. */
  readonly platform: string
  readonly build: BuildInfo | undefined
  readonly cacheDir: string
  /** The directory of an installed platform package, if there is one. */
  readonly packageDir: (name: string) => string | undefined
  readonly download: (url: string, limit: number) => Promise<Uint8Array>
}

function installedPackage(name: string): string | undefined {
  try { return dirname(createRequire(join(TOOL_ROOT, "package.json")).resolve(`${name}/package.json`)) }
  catch { return undefined }
}

/** How long a host download may take before it is abandoned. */
export const DOWNLOAD_TIMEOUT_MS = 600_000

/**
 * Fetch at most `limit` bytes from `url`, within `timeoutMs`. The body is
 * streamed and abandoned as soon as it grows past the limit, so a server that
 * sends more than the release recorded, or stalls, cannot fill memory or hang
 * `norn dev`.
 */
export async function download(url: string, limit: number, timeoutMs: number = DOWNLOAD_TIMEOUT_MS): Promise<Uint8Array> {
  const response = await fetch(url, { redirect: "follow", signal: AbortSignal.timeout(timeoutMs) })
  if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`)
  const declared = Number(response.headers.get("content-length") ?? "NaN")
  if (Number.isFinite(declared) && declared > limit) {
    await response.body.cancel()
    throw new Error(`the server offers ${declared} bytes; the release records ${limit}`)
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
      throw new Error(`the server sent more than the ${limit} bytes the release records`)
    }
    chunks.push(value)
  }
  const bytes = new Uint8Array(size)
  let offset = 0
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength }
  return bytes
}

export function hostLookup(env: Readonly<Record<string, string | undefined>> = process.env): HostLookup {
  return {
    env,
    platform: `${process.platform}-${process.arch}`,
    build: buildInfo(),
    cacheDir: env.NORN_CACHE_DIR || join(homedir(), ".cache", "norn"),
    packageDir: installedPackage,
    download,
  }
}

const sha256 = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex")

/**
 * The one set of build-from-source instructions, the same as the registered
 * guidance for `cli.setup.host_unavailable`, with this CLI's commit and Rust
 * toolchain filled in when the package records them: a host built from another
 * commit may not match this CLI's compile worker.
 */
/** Where `cargo build --release`, run from this checkout's root, puts the host: CARGO_TARGET_DIR when set (relative to that root), else target/. */
export function checkoutHost(env: HostLookup["env"]): string {
  const repo = resolve(TOOL_ROOT, "..", "..")
  return join(env.CARGO_TARGET_DIR ? resolve(repo, env.CARGO_TARGET_DIR) : join(repo, "target"), "release", "norn-dev-host")
}

function unavailable(detail: string, lookup: Pick<HostLookup, "build" | "env">): ProjectError {
  const { build, env } = lookup
  const command = "`cargo build --release -p norn-cell-do --bin norn-dev-host`"
  // A checkout knows where its own build lands; a release names the commit to build from.
  const how = build
    ? `Build it from a clone of the Norn repository at commit ${build.commit} (\`git checkout ${build.commit}\`; Rust ${build.toolchain}) with ${command}, then set NORN_DEV_HOST_BIN to the binary's absolute path, for example \`export NORN_DEV_HOST_BIN="\${CARGO_TARGET_DIR:-$PWD/target}/release/norn-dev-host"\` from the repository root.`
    : `Build it in this checkout with ${command}, then \`export NORN_DEV_HOST_BIN="${checkoutHost(env)}"\`.`
  return new ProjectError("cli.setup.host_unavailable", `No native Norn host: ${detail} ${how}`, 3)
}

/** A release asset is fetched over https; plain http only from this machine (a local dry run). */
function fetchable(url: string): boolean {
  return url.startsWith("https://") || /^http:\/\/(127\.0\.0\.1|localhost)(:\d+)?\//.test(url)
}

/** The dev host binary to run, by the order in the module comment. */
export async function hostBinary(lookup: HostLookup = hostLookup()): Promise<string> {
  const build = lookup.build
  const override = lookup.env.NORN_DEV_HOST_BIN
  if (override) {
    if (!existsSync(override)) throw unavailable(`NORN_DEV_HOST_BIN names ${override}, which does not exist.`, lookup)
    return override
  }
  const asset = build?.hosts[lookup.platform]
  const packageName = `@norn/dev-host-${lookup.platform}`
  const packaged = lookup.packageDir(packageName)
  if (packaged && existsSync(join(packaged, "bin", "norn-dev-host"))) {
    const binary = join(packaged, "bin", "norn-dev-host")
    // An installed host runs only if it is the one this release records: no record, no run.
    if (!asset) {
      throw unavailable(`${packageName} is installed at ${packaged}, but ${build ? `this release records no host for ${lookup.platform}` : "a checkout records no host digest"}, so it is not run.`, lookup)
    }
    if (sha256(readFileSync(binary)) !== asset.sha256) {
      throw unavailable(`the installed ${packageName} at ${packaged} is not the host this release records (sha256 ${asset.sha256}); reinstall @norn/cli.`, lookup)
    }
    return binary
  }
  if (!build) throw unavailable("this is a checkout, which carries no prebuilt host.", lookup)
  if (!asset) {
    throw unavailable(`this release has no prebuilt host for ${lookup.platform} (it has ${Object.keys(build.hosts).sort().join(", ") || "none"}).`, lookup)
  }
  const cached = join(lookup.cacheDir, "dev-host", build.version, lookup.platform, "norn-dev-host")
  if (existsSync(cached) && sha256(readFileSync(cached)) === asset.sha256) return cached
  if (!asset.url) throw unavailable(`${packageName} is not installed; reinstall @norn/cli for this platform.`, lookup)
  if (!fetchable(asset.url)) throw unavailable(`the release records ${asset.url}, which is not an https address.`, lookup)
  let bytes: Uint8Array
  process.stderr.write(`norn dev: fetching the dev host for ${lookup.platform} (${(asset.size / 1_000_000).toFixed(0)} MB), once\n`)
  try { bytes = await lookup.download(asset.url, asset.size) }
  catch (error) { throw unavailable(`fetching ${asset.url} failed (${error instanceof Error ? error.message : String(error)}).`, lookup) }
  const found = sha256(bytes)
  if (found !== asset.sha256) {
    // Never keep or run a file that is not the one the release recorded.
    throw unavailable(`the file fetched from ${asset.url} has sha256 ${found}, not the ${asset.sha256} this release records; it was not kept.`, lookup)
  }
  mkdirSync(dirname(cached), { recursive: true })
  const partial = `${cached}.${process.pid}.partial`
  try {
    writeFileSync(partial, bytes, { mode: 0o755 })
    chmodSync(partial, 0o755)
    renameSync(partial, cached)
  } catch (error) {
    rmSync(partial, { force: true })
    throw unavailable(`the fetched host could not be stored in ${dirname(cached)} (${error instanceof Error ? error.message : String(error)}).`, lookup)
  }
  return cached
}

export async function dev(args: readonly string[]): Promise<number> {
  const { root } = readProject()
  warnAuthoringVersion(root)
  const host = await hostBinary()
  const child = Bun.spawn([host, "--project", root, ...args, "--", process.execPath, join(TOOL_ROOT, "src/worker.ts")],
    { stdin: "inherit", stdout: "inherit", stderr: "inherit" })
  const stop = () => child.kill("SIGTERM")
  process.on("SIGINT", stop)
  process.on("SIGTERM", stop)
  return await child.exited
}
