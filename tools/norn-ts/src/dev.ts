/**
 * `norn dev` (#207): find the project, then run the native dev host with
 * this package's compile worker. The native host owns the cell, admission and
 * serving; this front door only locates the pieces.
 *
 * The host binary is `norn-dev-host`, built from the `norn-cell-do` crate. It
 * is found in this order:
 * 1. `NORN_DEV_HOST_BIN`, an override that wins whenever it is set;
 * 2. the platform package `@norn/dev-host-<platform>-<arch>`, when installed;
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
import { dirname, join } from "node:path"
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
  readonly download: (url: string) => Promise<Uint8Array>
}

function installedPackage(name: string): string | undefined {
  try { return dirname(createRequire(join(TOOL_ROOT, "package.json")).resolve(`${name}/package.json`)) }
  catch { return undefined }
}

async function download(url: string): Promise<Uint8Array> {
  const response = await fetch(url, { redirect: "follow" })
  if (!response.ok) throw new Error(`HTTP ${response.status}`)
  return new Uint8Array(await response.arrayBuffer())
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

function unavailable(detail: string, toolchain: string | undefined): ProjectError {
  return new ProjectError("cli.setup.host_unavailable",
    `No native Norn host: ${detail} Build it from a checkout of the Norn repository with \`cargo build --release -p norn-cell-do --bin norn-dev-host\`${toolchain ? ` (Rust ${toolchain})` : ""} and set NORN_DEV_HOST_BIN to target/release/norn-dev-host.`, 3)
}

/** A release asset is fetched over https; plain http only from this machine (a local dry run). */
function fetchable(url: string): boolean {
  return url.startsWith("https://") || /^http:\/\/(127\.0\.0\.1|localhost)(:\d+)?\//.test(url)
}

/** The dev host binary to run, by the order in the module comment. */
export async function hostBinary(lookup: HostLookup = hostLookup()): Promise<string> {
  const toolchain = lookup.build?.toolchain
  const override = lookup.env.NORN_DEV_HOST_BIN
  if (override) {
    if (!existsSync(override)) throw unavailable(`NORN_DEV_HOST_BIN names ${override}, which does not exist.`, toolchain)
    return override
  }
  const packaged = lookup.packageDir(`@norn/dev-host-${lookup.platform}`)
  if (packaged && existsSync(join(packaged, "bin", "norn-dev-host"))) return join(packaged, "bin", "norn-dev-host")
  if (!lookup.build) throw unavailable("this is a checkout, which carries no prebuilt host.", toolchain)
  const asset = lookup.build.hosts[lookup.platform]
  if (!asset) {
    throw unavailable(`this release has no prebuilt host for ${lookup.platform} (it has ${Object.keys(lookup.build.hosts).sort().join(", ") || "none"}).`, toolchain)
  }
  const cached = join(lookup.cacheDir, "dev-host", lookup.build.version, lookup.platform, "norn-dev-host")
  if (existsSync(cached) && sha256(readFileSync(cached)) === asset.sha256) return cached
  if (!fetchable(asset.url)) throw unavailable(`the release records ${asset.url}, which is not an https address.`, toolchain)
  let bytes: Uint8Array
  process.stderr.write(`norn dev: fetching the dev host for ${lookup.platform} (${(asset.size / 1_000_000).toFixed(0)} MB), once\n`)
  try { bytes = await lookup.download(asset.url) }
  catch (error) { throw unavailable(`fetching ${asset.url} failed (${error instanceof Error ? error.message : String(error)}).`, toolchain) }
  const found = sha256(bytes)
  if (found !== asset.sha256) {
    // Never keep or run a file that is not the one the release recorded.
    throw unavailable(`the file fetched from ${asset.url} has sha256 ${found}, not the ${asset.sha256} this release records; it was not kept.`, toolchain)
  }
  mkdirSync(dirname(cached), { recursive: true })
  const partial = `${cached}.${process.pid}.partial`
  try {
    writeFileSync(partial, bytes, { mode: 0o755 })
    chmodSync(partial, 0o755)
    renameSync(partial, cached)
  } catch (error) {
    rmSync(partial, { force: true })
    throw unavailable(`the fetched host could not be stored in ${dirname(cached)} (${error instanceof Error ? error.message : String(error)}).`, toolchain)
  }
  return cached
}

export async function dev(args: readonly string[]): Promise<number> {
  const { root } = readProject()
  const host = await hostBinary()
  const child = Bun.spawn([host, "--project", root, ...args, "--", process.execPath, join(TOOL_ROOT, "src/worker.ts")],
    { stdin: "inherit", stdout: "inherit", stderr: "inherit" })
  const stop = () => child.kill("SIGTERM")
  process.on("SIGINT", stop)
  process.on("SIGTERM", stop)
  return await child.exited
}
