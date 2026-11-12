/**
 * What a released package knows about itself. `generated/build.json` is
 * written by the release assembly (`scripts/release/assemble.py`) and exists
 * only in a built package; a repository checkout has none.
 *
 * The version lives in one place, `tools/norn-ts/package.json`; the assembly
 * stamps it here with the commit it built from.
 */
import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { TOOL_ROOT } from "./profile.ts"

/** One prebuilt dev host of a release: where to get it and what it must hash to. */
export interface HostAsset {
  readonly url: string
  readonly sha256: string
  readonly size: number
}

export interface BuildInfo {
  readonly version: string
  /** The commit the package was built from. */
  readonly commit: string
  /** The Rust toolchain the dev host was built with, for building it from source. */
  readonly toolchain: string
  /** What a new project depends on: package name to version or tarball URL. */
  readonly packages: Readonly<Record<string, string>>
  /** Prebuilt dev hosts by `<platform>-<arch>` (`linux-x64`). */
  readonly hosts: Readonly<Record<string, HostAsset>>
}

function text(value: unknown): value is string { return typeof value === "string" && value.length > 0 }
function record(value: unknown): value is Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value) }

/** Parse a build manifest strictly: a malformed one is an error, never a silent checkout. */
export function parseBuildInfo(value: unknown): BuildInfo {
  if (!record(value) || !text(value.version) || !text(value.commit) || !text(value.toolchain) || !record(value.packages) || !record(value.hosts)) {
    throw new Error("generated/build.json is not a build manifest")
  }
  const packages = Object.entries(value.packages)
  const hosts = Object.entries(value.hosts)
  if (!packages.every(([, spec]) => text(spec))
    || !hosts.every(([, host]) => record(host) && text(host.url) && typeof host.sha256 === "string" && /^[0-9a-f]{64}$/.test(host.sha256) && Number.isInteger(host.size))) {
    throw new Error("generated/build.json is not a build manifest")
  }
  return value as unknown as BuildInfo
}

/** The build manifest of this package, or undefined in a repository checkout. */
export function buildInfo(root: string = TOOL_ROOT): BuildInfo | undefined {
  const path = join(root, "generated", "build.json")
  return existsSync(path) ? parseBuildInfo(JSON.parse(readFileSync(path, "utf8"))) : undefined
}

/** `norn --version`: the released version and commit, or the checkout's version. */
export function versionLine(root: string = TOOL_ROOT): string {
  const build = buildInfo(root)
  if (build) return `norn ${build.version} (${build.commit.slice(0, 12)})`
  const manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8")) as { version?: string }
  return `norn ${manifest.version ?? "0.0.0"} (checkout)`
}
